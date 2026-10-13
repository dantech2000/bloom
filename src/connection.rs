// SPDX-License-Identifier: AGPL-3.0-or-later
//! The connection to the server as the app holds it: the state machine of
//! `bloom_core::connection` with its tasks, and what the pages do when the
//! state changes.

use std::time::{Duration, Instant};

use gpui_kit::{Context, Task};

pub use bloom_core::connection::*;

use crate::{
    app::{Bloom, Page},
    downloads::engine,
    downloads::engine::EntryState,
    realtime::SocketEvent,
};

/// The connection as the app holds it: the state machine and its tasks.
#[derive(Default)]
pub struct ConnectionState {
    pub core: Connection,
    /// Ticks once a second while offline: the retry timer, and the
    /// countdown on the page.
    tick: Option<Task<()>>,
    probes: Probes,
    /// The current page failed while the state was checked; a good probe
    /// loads it again.
    pub page_failed: bool,
    last_auto_reload: Option<Instant>,
    /// The app opened the Downloads page when it went offline; recovery
    /// goes home.
    sent_to_downloads: bool,
}

impl Bloom {
    /// A new session: the state starts online. The probe of the session
    /// before, when one is under way, gets no say in it.
    pub fn connection_reset(&mut self) {
        let mut probes = std::mem::take(&mut self.connection.probes);
        probes.reset();
        self.connection = ConnectionState { probes, ..ConnectionState::default() };
        set_offline(false);
    }

    fn server_name(&self) -> String {
        self.session
            .as_ref()
            .map(|s| s.server_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "The server".to_string())
    }

    /// True when there is a finished download to play from this Mac.
    pub fn has_downloads(&self) -> bool {
        engine().entries().iter().any(|e| e.state == EntryState::Done)
    }

    /// A request of a page failed. True when nothing reached the server:
    /// the page shows no raw error then, and a probe decides whether the
    /// app is offline.
    pub fn request_failed(&mut self, err: &anyhow::Error, cx: &mut Context<Self>) -> bool {
        let verdict = classify(err);
        let unreachable = matches!(verdict, Verdict::Unreachable(_));
        if unreachable {
            log::info!("request failed, the server may be offline: {err:#}");
            self.connection.page_failed = true;
        }
        let action = self.connection.core.handle(Event::RequestFailed(verdict), Instant::now());
        self.connection_act(action, cx);
        unreachable || self.connection.core.is_offline()
    }

    /// The socket of the server opened or closed.
    pub fn connection_socket(&mut self, event: &SocketEvent, cx: &mut Context<Self>) {
        let event = match event {
            SocketEvent::Open { .. } => Event::SocketOpened,
            SocketEvent::Closed => Event::SocketClosed,
            SocketEvent::Message { .. } => return,
        };
        let action = self.connection.core.handle(event, Instant::now());
        self.connection_act(action, cx);
    }

    /// A probe now: Retry on the page, in the chip, and from the debug
    /// channel.
    pub fn probe_now(&mut self, cx: &mut Context<Self>) {
        let action = self.connection.core.handle(Event::Retry, Instant::now());
        self.connection_act(action, cx);
        cx.notify();
    }

    fn connection_act(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::None => {}
            Action::Probe => self.start_probe(cx),
            Action::WentOffline => self.went_offline(cx),
            Action::Recovered => self.recovered(cx),
        }
    }

    fn start_probe(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            self.connection.core.probing = false;
            return;
        };
        let id = self.connection.probes.start();
        let (base, auth) = (client.base.to_string(), client.auth_header());
        self.fetch_with(
            client,
            cx,
            move |_| Ok(probe(&base, &auth)),
            move |this, result, cx| {
                if !this.connection.probes.is_current(id) {
                    return;
                }
                let event = match result {
                    Ok(Ok(())) => Event::ProbeOk,
                    Ok(Err(why)) => Event::ProbeFailed(why),
                    Err(_) => Event::ProbeFailed(Why::Connect),
                };
                let was = this.connection.core.state;
                let action = this.connection.core.handle(event, Instant::now());
                this.connection_act(action, cx);
                // The page failed, the server is there: load it again.
                if was == State::Checking
                    && this.connection.core.state == State::Online
                    && this.connection.page_failed
                    && this
                        .connection
                        .last_auto_reload
                        .is_none_or(|at| at.elapsed() >= AUTO_RELOAD_GAP)
                {
                    this.connection.last_auto_reload = Some(Instant::now());
                    this.connection.page_failed = false;
                    this.load_page(cx);
                }
                cx.notify();
            },
        );
    }

    /// The probe failed: the app is offline. One toast; with downloads the
    /// Downloads page opens and the pages stop asking the server.
    fn went_offline(&mut self, cx: &mut Context<Self>) {
        let why = self.connection.core.why.unwrap_or(Why::Connect);
        let server = self.server_name();
        log::info!("offline: {server}: {}", plain_words(why));
        set_offline(true);
        self.downloads.offline = true;
        engine().set_client(None, "");
        let mut text = plain_words(why);
        if self.has_downloads() {
            text.push_str(" Your downloads play from this Mac.");
            if !matches!(self.page, Page::Downloads) {
                self.connection.sent_to_downloads = true;
                self.navigate(Page::Downloads, cx);
            }
        }
        self.toast(format!("{server} is offline"), text, cx);
        self.start_connection_tick(cx);
        self.rebuild_menu(cx);
        cx.notify();
    }

    /// The server answers again: the current page loads again, the
    /// positions of offline plays go out, the indicator goes away.
    fn recovered(&mut self, cx: &mut Context<Self>) {
        let server = self.server_name();
        log::info!("online again: {server}");
        set_offline(false);
        self.connection.tick = None;
        self.connection.page_failed = false;
        self.downloads.offline = false;
        crate::jellyfin::reconnected();
        if let Some(session) = self.session.as_ref() {
            engine().set_client(Some(session.client.clone()), &session.server_id);
        }
        // The socket was stopped with the session pointed at a dead
        // address; the real one keeps its socket, which reconnects by itself.
        if self.sync.session.is_none() {
            self.start_sync(cx);
        }
        self.load_download_policy(cx);
        self.toast(format!("{server} is back"), "The server answers again.", cx);
        let home = std::mem::take(&mut self.connection.sent_to_downloads)
            && matches!(self.page, Page::Downloads);
        if home {
            self.open_home(cx);
        } else {
            self.load_page(cx);
        }
        self.rebuild_menu(cx);
        cx.notify();
    }

    fn start_connection_tick(&mut self, cx: &mut Context<Self>) {
        if self.connection.tick.is_some() {
            return;
        }
        self.connection.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let go_on = this.update(cx, |this, cx| {
                    if !this.connection.core.is_offline() {
                        this.connection.tick = None;
                        return false;
                    }
                    let action = this.connection.core.handle(Event::Tick, Instant::now());
                    this.connection_act(action, cx);
                    // The countdown on the page.
                    cx.notify();
                    true
                });
                if !matches!(go_on, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// The debug verb `connection`: the state, and `offline`, `online`,
    /// `probe` to force and to test.
    pub fn debug_connection(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        match rest {
            "" | "state" => {}
            // The session points at a dead address until `online`.
            "offline" => {
                self.simulate_unreachable(true);
                self.connection.core.force_offline(Instant::now());
                self.went_offline(cx);
            }
            "online" => self.retry_connection(cx),
            "probe" => self.probe_now(cx),
            _ => return "error: connection state|offline|online|probe".into(),
        }
        cx.notify();
        self.connection_describe()
    }

    pub fn connection_describe(&self) -> String {
        let core = &self.connection.core;
        let now = Instant::now();
        let c = core.counters;
        format!(
            "connection state={:?} why={:?} reason={:?} since={}s next_retry={} probing={} \
             failures={} probes={} probes_failed={} went_offline={} recoveries={} page_failed={} \
             forced={} downloads_offline={} has_downloads={}",
            core.state,
            core.why,
            core.why.map(plain_words).unwrap_or_default(),
            core.since.map(|at| now.duration_since(at).as_secs()).unwrap_or(0),
            core.retry_in(now)
                .map(|s| format!("{s}s"))
                .unwrap_or_else(|| "none".into()),
            core.probing,
            c.failures,
            c.probes,
            c.probes_failed,
            c.went_offline,
            c.recoveries,
            self.connection.page_failed,
            self.downloads.is_unreachable_simulated(),
            self.downloads.offline,
            self.has_downloads(),
        )
    }
}
