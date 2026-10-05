// SPDX-License-Identifier: AGPL-3.0-or-later
//! One state of the connection to the server for the whole app: online, or
//! offline with the reason in plain words and the time of the next try.
//!
//! A failed page request alone does not make the app offline: it starts a
//! quick probe (`GET /System/Info/Public`, short timeout), and the probe
//! decides. Offline, the app tries again with a backoff (5, 10, 30, then
//! every 60 seconds), at once on Retry and when the socket of the server
//! opens again. On recovery the current page loads again and the positions
//! of offline plays go to the server.
//!
//! A 401 is not offline; the sign-in handles it. A 500, 404 or 403 is an
//! answer of the server: it is there.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use gpui_kit::{Context, Task};

use crate::{
    app::{Bloom, Page},
    downloads::engine,
    downloads::engine::EntryState,
    realtime::SocketEvent,
};

/// The probe waits this long for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// A page that failed while the state was checked loads again after a good
/// probe, but not more often than this: a page that fails every time would
/// load in a loop.
const AUTO_RELOAD_GAP: Duration = Duration::from_secs(30);

/// Why nothing of the server reached the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The name of the server did not resolve.
    Dns,
    /// No connection could be made, or it broke.
    Connect,
    /// The server did not answer in time.
    Timeout,
    /// The secure connection could not be set up.
    Tls,
    /// A gateway in front of the server answered that the server is not
    /// there: 502, 503, 504, or Cloudflare's 520-527 and 530.
    Gateway(u16),
}

/// What a failed request says about the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The server answered (a 500, 404, 403...): it is there.
    Answered,
    /// 401: the session is not valid. Not an offline matter.
    Unauthorized,
    /// Nothing of the server reached the app.
    Unreachable(Why),
}

/// The rule for a status code: which answers mean "the server is not
/// there".
pub fn classify_status(code: u16) -> Verdict {
    match code {
        401 => Verdict::Unauthorized,
        502..=504 | 520..=527 | 530 => Verdict::Unreachable(Why::Gateway(code)),
        _ => Verdict::Answered,
    }
}

/// The rule for an error of the HTTP client.
pub fn classify_ureq(err: &ureq::Error) -> Verdict {
    match err {
        ureq::Error::StatusCode(code) => classify_status(*code),
        ureq::Error::HostNotFound => Verdict::Unreachable(Why::Dns),
        // macOS reports a name that does not resolve as an io error:
        // "failed to lookup address information: nodename nor servname...".
        ureq::Error::Io(err) if err.to_string().contains("lookup") => Verdict::Unreachable(Why::Dns),
        ureq::Error::Timeout(_) => Verdict::Unreachable(Why::Timeout),
        ureq::Error::Tls(_) => Verdict::Unreachable(Why::Tls),
        ureq::Error::ConnectionFailed | ureq::Error::Io(_) => Verdict::Unreachable(Why::Connect),
        // A bad address, a body that is not JSON, too many redirects:
        // something answered.
        _ => Verdict::Answered,
    }
}

/// The verdict of a failed request. A request of `jellyfin::Client` carries
/// its verdict; any other error is read from its text, by the old rule: a
/// status code is an answer of the server, anything else is the network.
pub fn classify(err: &anyhow::Error) -> Verdict {
    if let Some(request) = err.downcast_ref::<crate::jellyfin::RequestError>() {
        return request.verdict;
    }
    let text = format!("{err:#}");
    if text.starts_with("unauthorized") {
        Verdict::Unauthorized
    } else if let Some(rest) = text.strip_prefix("HTTP ") {
        let code: u16 = rest
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        classify_status(code)
    } else if text.starts_with("decode ") {
        Verdict::Answered
    } else {
        Verdict::Unreachable(Why::Connect)
    }
}

/// The wait before retry number `attempt` (from 0) while offline.
pub fn backoff(attempt: u32) -> Duration {
    const STEPS: [u64; 4] = [5, 10, 30, 60];
    Duration::from_secs(STEPS[(attempt as usize).min(STEPS.len() - 1)])
}

/// The reason in plain words, for the page, the chip and the toast. The raw
/// error goes to the log, not here.
pub fn plain_words(why: Why) -> String {
    match why {
        Why::Dns => "The address of the server could not be found.".into(),
        Why::Connect => "No connection to the server could be made.".into(),
        Why::Timeout => "The server did not answer in time.".into(),
        Why::Tls => "The secure connection to the server could not be set up.".into(),
        Why::Gateway(530) => "The proxy in front of the server says it is unreachable (530).".into(),
        Why::Gateway(503) => "The server is not available at the moment (503).".into(),
        Why::Gateway(code) => format!("The proxy in front of the server got no answer from it ({code})."),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Online,
    /// A request failed; a probe decides.
    Checking,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A request of a page failed.
    RequestFailed(Verdict),
    /// The probe got an answer: the server is there.
    ProbeOk,
    ProbeFailed(Why),
    SocketOpened,
    SocketClosed,
    /// Retry, from the user or the debug channel: a probe now.
    Retry,
    /// Once a second while offline.
    Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Start a probe.
    Probe,
    WentOffline,
    Recovered,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub failures: u32,
    pub probes: u32,
    pub probes_failed: u32,
    pub went_offline: u32,
    pub recoveries: u32,
}

/// The state machine, with no app in it, so a test can drive it.
#[derive(Debug)]
pub struct Connection {
    pub state: State,
    /// The reason while checking or offline.
    pub why: Option<Why>,
    /// When the app went offline.
    pub since: Option<Instant>,
    /// When the next probe starts by itself; none while one runs.
    pub next_retry: Option<Instant>,
    pub probing: bool,
    /// Probes that failed in a row while offline; picks the backoff step.
    attempts: u32,
    pub counters: Counters,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            state: State::Online,
            why: None,
            since: None,
            next_retry: None,
            probing: false,
            attempts: 0,
            counters: Counters::default(),
        }
    }
}

impl Connection {
    pub fn is_offline(&self) -> bool {
        self.state == State::Offline
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Action {
        match (self.state, event) {
            (state, Event::RequestFailed(verdict)) => {
                self.counters.failures += 1;
                match (state, verdict) {
                    // One lost request does not flip the state; the probe does.
                    (State::Online, Verdict::Unreachable(why)) => {
                        self.state = State::Checking;
                        self.why = Some(why);
                        self.probe()
                    }
                    _ => Action::None,
                }
            }
            (state, Event::ProbeOk) => {
                self.probing = false;
                self.state = State::Online;
                self.why = None;
                self.since = None;
                self.next_retry = None;
                self.attempts = 0;
                if state == State::Offline {
                    self.counters.recoveries += 1;
                    Action::Recovered
                } else {
                    Action::None
                }
            }
            (State::Checking, Event::ProbeFailed(why)) => {
                self.probing = false;
                self.counters.probes_failed += 1;
                self.counters.went_offline += 1;
                self.go_offline(why, now);
                Action::WentOffline
            }
            (State::Offline, Event::ProbeFailed(why)) => {
                self.probing = false;
                self.counters.probes_failed += 1;
                self.why = Some(why);
                self.attempts += 1;
                self.next_retry = Some(now + backoff(self.attempts));
                Action::None
            }
            // A probe of an earlier state.
            (State::Online, Event::ProbeFailed(_)) => {
                self.probing = false;
                Action::None
            }
            // The socket is lost: a probe tells whether the server is.
            (State::Online, Event::SocketClosed) => {
                self.state = State::Checking;
                self.probe()
            }
            (State::Offline, Event::SocketOpened) | (_, Event::Retry) => self.probe(),
            (State::Offline, Event::Tick) => match self.next_retry {
                Some(at) if at <= now => self.probe(),
                _ => Action::None,
            },
            _ => Action::None,
        }
    }

    /// Offline now, as if a probe failed: the debug channel forces it.
    pub fn force_offline(&mut self, now: Instant) {
        self.probing = false;
        self.counters.went_offline += 1;
        self.go_offline(Why::Connect, now);
    }

    fn go_offline(&mut self, why: Why, now: Instant) {
        self.state = State::Offline;
        self.why = Some(why);
        self.since = Some(now);
        self.attempts = 0;
        self.next_retry = Some(now + backoff(0));
    }

    fn probe(&mut self) -> Action {
        if self.probing {
            return Action::None;
        }
        self.probing = true;
        self.next_retry = None;
        self.counters.probes += 1;
        Action::Probe
    }

    /// Seconds until the next probe starts by itself, for the page.
    pub fn retry_in(&self, now: Instant) -> Option<u64> {
        self.next_retry
            .map(|at| at.saturating_duration_since(now).as_secs())
    }
}

/// Asks the server whether it is there, with a short wait. Any answer that
/// is not a gateway's "the origin is down" counts: a 500 or a 404 is still
/// the server, or something, at that address.
fn probe(base: &str, auth: &str) -> Result<(), Why> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(PROBE_TIMEOUT))
            .build(),
    );
    match agent
        .get(&format!("{base}/System/Info/Public"))
        .header("Authorization", auth)
        .call()
    {
        Ok(_) => Ok(()),
        Err(err) => match classify_ureq(&err) {
            Verdict::Unreachable(why) => {
                log::info!("probe of {base} failed: {err}");
                Err(why)
            }
            _ => Ok(()),
        },
    }
}

/// The state as the rest of the app reads it without the app: the image
/// loader may skip its fetches while this is true.
static OFFLINE: AtomicBool = AtomicBool::new(false);

pub fn is_offline() -> bool {
    OFFLINE.load(Ordering::Relaxed)
}

/// The connection as the app holds it: the state machine and its tasks.
#[derive(Default)]
pub struct ConnectionState {
    pub core: Connection,
    /// Ticks once a second while offline: the retry timer, and the
    /// countdown on the page.
    tick: Option<Task<()>>,
    /// Counts the probes started; the answer of an older one is dropped.
    probe_id: u64,
    /// The current page failed while the state was checked; a good probe
    /// loads it again.
    pub page_failed: bool,
    last_auto_reload: Option<Instant>,
    /// The app opened the Downloads page when it went offline; recovery
    /// goes home.
    sent_to_downloads: bool,
}

impl Bloom {
    /// A new session: the state starts online.
    pub fn connection_reset(&mut self) {
        self.connection = ConnectionState::default();
        OFFLINE.store(false, Ordering::Relaxed);
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
        self.connection.probe_id += 1;
        let id = self.connection.probe_id;
        let (base, auth) = (client.base.to_string(), client.auth_header());
        self.fetch_with(
            client,
            cx,
            move |_| Ok(probe(&base, &auth)),
            move |this, result, cx| {
                if id != this.connection.probe_id {
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
        OFFLINE.store(true, Ordering::Relaxed);
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
        OFFLINE.store(false, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn gateway_answers_and_transport_errors_are_offline_the_rest_is_not() {
        for code in [502, 503, 504, 520, 521, 522, 523, 524, 525, 526, 527, 530] {
            assert_eq!(classify_status(code), Verdict::Unreachable(Why::Gateway(code)), "{code}");
        }
        for code in [200, 400, 403, 404, 500, 501, 505, 528, 529, 531] {
            assert_eq!(classify_status(code), Verdict::Answered, "{code}");
        }
        assert_eq!(classify_status(401), Verdict::Unauthorized);
        assert_eq!(classify_ureq(&ureq::Error::HostNotFound), Verdict::Unreachable(Why::Dns));
        assert_eq!(classify_ureq(&ureq::Error::ConnectionFailed), Verdict::Unreachable(Why::Connect));
        assert_eq!(
            classify_ureq(&ureq::Error::Io(std::io::Error::other("reset"))),
            Verdict::Unreachable(Why::Connect)
        );
        assert_eq!(
            classify_ureq(&ureq::Error::Io(std::io::Error::other(
                "failed to lookup address information: nodename nor servname provided"
            ))),
            Verdict::Unreachable(Why::Dns)
        );
        assert_eq!(
            classify_ureq(&ureq::Error::Timeout(ureq::Timeout::Global)),
            Verdict::Unreachable(Why::Timeout)
        );
        assert_eq!(classify_ureq(&ureq::Error::Tls("bad")), Verdict::Unreachable(Why::Tls));
        assert_eq!(classify_ureq(&ureq::Error::StatusCode(530)), Verdict::Unreachable(Why::Gateway(530)));
        assert_eq!(classify_ureq(&ureq::Error::StatusCode(500)), Verdict::Answered);
        assert_eq!(classify_ureq(&ureq::Error::BadUri("x".into())), Verdict::Answered);
    }

    #[test]
    fn an_error_without_a_verdict_is_read_from_its_text() {
        assert_eq!(classify(&anyhow::anyhow!("HTTP 530 at /Items")), Verdict::Unreachable(Why::Gateway(530)));
        assert_eq!(classify(&anyhow::anyhow!("HTTP 500 at /Items")), Verdict::Answered);
        assert_eq!(classify(&anyhow::anyhow!("HTTP 404 at /Items")), Verdict::Answered);
        assert_eq!(classify(&anyhow::anyhow!("unauthorized (401) at /Items")), Verdict::Unauthorized);
        assert_eq!(classify(&anyhow::anyhow!("decode /Items")), Verdict::Answered);
        assert_eq!(classify(&anyhow::anyhow!("io: connection refused (/Items)")), Verdict::Unreachable(Why::Connect));
        // The typed way: the verdict of the request, not its text.
        let err: anyhow::Error = crate::jellyfin::RequestError::new(
            Verdict::Unreachable(Why::Timeout),
            "timeout: global (/Items)".into(),
        )
        .into();
        assert_eq!(classify(&err), Verdict::Unreachable(Why::Timeout));
        assert_eq!(format!("{err:#}"), "timeout: global (/Items)");
    }

    #[test]
    fn the_backoff_is_5_10_30_then_60() {
        let secs: Vec<u64> = (0..6).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [5, 10, 30, 60, 60, 60]);
    }

    #[test]
    fn one_lost_request_starts_a_probe_and_a_good_probe_keeps_the_app_online() {
        let mut c = Connection::default();
        let t = now();
        assert_eq!(c.handle(Event::RequestFailed(Verdict::Unreachable(Why::Timeout)), t), Action::Probe);
        assert_eq!(c.state, State::Checking);
        // A second failure while the probe runs starts no second probe.
        assert_eq!(c.handle(Event::RequestFailed(Verdict::Unreachable(Why::Connect)), t), Action::None);
        assert_eq!(c.counters.probes, 1);
        assert_eq!(c.handle(Event::ProbeOk, t), Action::None);
        assert_eq!(c.state, State::Online);
        assert!(!c.is_offline());
        assert_eq!(c.counters.went_offline, 0);
    }

    #[test]
    fn an_answer_of_the_server_or_a_401_is_not_offline() {
        let mut c = Connection::default();
        assert_eq!(c.handle(Event::RequestFailed(Verdict::Answered), now()), Action::None);
        assert_eq!(c.handle(Event::RequestFailed(Verdict::Unauthorized), now()), Action::None);
        assert_eq!(c.state, State::Online);
        assert_eq!(c.counters.failures, 2);
    }

    #[test]
    fn a_failed_probe_goes_offline_with_the_reason_and_the_backoff() {
        let mut c = Connection::default();
        let t = now();
        c.handle(Event::RequestFailed(Verdict::Unreachable(Why::Gateway(530))), t);
        assert_eq!(c.handle(Event::ProbeFailed(Why::Gateway(530)), t), Action::WentOffline);
        assert!(c.is_offline());
        assert_eq!(c.why, Some(Why::Gateway(530)));
        assert_eq!(c.retry_in(t), Some(5));
        // Not before the time; at the time.
        assert_eq!(c.handle(Event::Tick, t + Duration::from_secs(4)), Action::None);
        assert_eq!(c.handle(Event::Tick, t + Duration::from_secs(5)), Action::Probe);
        assert_eq!(c.retry_in(t), None);
        let t2 = t + Duration::from_secs(6);
        assert_eq!(c.handle(Event::ProbeFailed(Why::Connect), t2), Action::None);
        assert_eq!(c.why, Some(Why::Connect));
        assert_eq!(c.retry_in(t2), Some(10));
        c.handle(Event::Tick, t2 + Duration::from_secs(10));
        let t3 = t2 + Duration::from_secs(11);
        c.handle(Event::ProbeFailed(Why::Connect), t3);
        assert_eq!(c.retry_in(t3), Some(30));
        c.handle(Event::Tick, t3 + Duration::from_secs(30));
        let t4 = t3 + Duration::from_secs(31);
        c.handle(Event::ProbeFailed(Why::Connect), t4);
        assert_eq!(c.retry_in(t4), Some(60));
        c.handle(Event::Tick, t4 + Duration::from_secs(60));
        let t5 = t4 + Duration::from_secs(61);
        c.handle(Event::ProbeFailed(Why::Connect), t5);
        assert_eq!(c.retry_in(t5), Some(60));
        assert_eq!(c.counters.probes, 5);
        assert_eq!(c.counters.probes_failed, 5);
    }

    #[test]
    fn retry_and_a_socket_that_opens_probe_at_once_and_a_good_probe_recovers() {
        let mut c = Connection::default();
        let t = now();
        c.handle(Event::RequestFailed(Verdict::Unreachable(Why::Connect)), t);
        c.handle(Event::ProbeFailed(Why::Connect), t);
        assert_eq!(c.handle(Event::Retry, t), Action::Probe);
        // One probe at a time.
        assert_eq!(c.handle(Event::Retry, t), Action::None);
        assert_eq!(c.handle(Event::SocketOpened, t), Action::None);
        assert_eq!(c.handle(Event::ProbeFailed(Why::Connect), t), Action::None);
        assert_eq!(c.handle(Event::SocketOpened, t), Action::Probe);
        assert_eq!(c.handle(Event::ProbeOk, t), Action::Recovered);
        assert_eq!(c.state, State::Online);
        assert_eq!(c.why, None);
        assert_eq!(c.retry_in(t), None);
        assert_eq!(c.counters.recoveries, 1);
        // Online again: a tick does nothing.
        assert_eq!(c.handle(Event::Tick, t + Duration::from_secs(100)), Action::None);
    }

    #[test]
    fn a_lost_socket_starts_a_probe_and_a_forced_offline_counts() {
        let mut c = Connection::default();
        let t = now();
        assert_eq!(c.handle(Event::SocketClosed, t), Action::Probe);
        assert_eq!(c.state, State::Checking);
        assert_eq!(c.handle(Event::ProbeOk, t), Action::None);
        assert_eq!(c.state, State::Online);
        c.force_offline(t);
        assert!(c.is_offline());
        assert_eq!(c.retry_in(t), Some(5));
        assert_eq!(c.counters.went_offline, 1);
    }
}
