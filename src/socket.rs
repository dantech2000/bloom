// SPDX-License-Identifier: AGPL-3.0-or-later
//! The socket of the server in the app: who holds it open, who reads what
//! comes over it (SyncPlay, remote control, the measure of the connection,
//! the offline state), and the news of the server that makes a page load
//! again ("the library changed"). The socket itself is `realtime`.

use std::time::Instant;

use gpui_kit::{Context, Task};
use serde_json::Value;

use crate::{
    app::{Bloom, Page},
    jellyfin::Client,
    news,
    realtime::{self, Realtime, SocketEvent},
};

/// The socket of the server, as the app holds it.
#[derive(Default)]
pub struct ServerSocket {
    realtime: Option<Realtime>,
    /// Gives the events of the socket to the app.
    reader: Option<Task<()>>,
    pub news: NewsState,
}

impl ServerSocket {
    /// Sends a message over the socket, such as a subscription. False
    /// without a socket.
    pub fn send(&self, kind: &str, data: Option<Value>) -> bool {
        match &self.realtime {
            Some(realtime) => {
                realtime.send(kind, data);
                true
            }
            None => false,
        }
    }
}

/// The news of the server that waits for its reload.
#[derive(Default)]
pub struct NewsState {
    /// Waits for the news to go quiet before a page loads again; a scan of
    /// the library sends a message for every few items.
    wait: Option<Task<()>>,
    /// The news that waits is about the library, not only about the user.
    library: bool,
    /// News waits for its quiet moment.
    pending: bool,
    /// When the first message that no reload has handled arrived.
    first: Option<Instant>,
    /// A reload came due while the player was open; it runs when the
    /// player closes.
    owed: bool,
    /// What the news did, for the debug command `events`.
    pub stats: news::NewsStats,
}

impl Bloom {
    /// Opens the socket of the session of `client` and reads it.
    pub(crate) fn start_socket(&mut self, client: Client, cx: &mut Context<Self>) {
        let (realtime, events) = realtime::start(client);
        self.sync.socket.realtime = Some(realtime);
        self.sync.socket.reader = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let fed = this.update(cx, |this, cx| this.on_socket_event(event, cx));
                if fed.is_err() {
                    break;
                }
            }
        }));
    }

    /// Closes the socket. News that waits keeps its reload.
    pub(crate) fn stop_socket(&mut self) {
        self.sync.socket.reader = None;
        self.sync.socket.realtime = None;
    }

    /// An event of the socket: a real one, or a message the debug command
    /// `socket-inject` makes. Both go the same way, to every reader.
    pub fn on_socket_event(&mut self, event: SocketEvent, cx: &mut Context<Self>) {
        if let SocketEvent::Message { kind, .. } = &event {
            self.sync.socket.news.stats.message(kind);
        }
        // Remote control reads the socket as well.
        self.cast_socket(&event, cx);
        // A socket that opens again may mean another network.
        self.adaptive_socket(&event);
        // A lost socket asks whether the server is there; one that opens
        // again ends the offline state.
        self.connection_socket(&event, cx);
        // No session of SyncPlay (the socket is off in a test instance):
        // the news still reaches the pages.
        let ui = self.sync.session.as_mut().map(|session| session.socket(&event));
        self.sync_apply(ui.unwrap_or_default(), cx);
        if let SocketEvent::Message { kind, .. } = &event {
            self.server_news(kind, cx);
            // A reader may have changed what the window shows.
            cx.notify();
        }
    }

    /// News of the server: the pages that show what changed load again,
    /// once the news has been quiet for a moment.
    fn server_news(&mut self, kind: &str, cx: &mut Context<Self>) {
        let Some(library) = news::kind_of(kind) else {
            return;
        };
        log::debug!("server news: {kind}");
        let state = &mut self.sync.socket.news;
        state.library |= library;
        state.pending = true;
        let first = *state.first.get_or_insert_with(Instant::now);
        let wait = news::delay(first.elapsed());
        // A new message replaces the task; `delay` caps the total wait.
        state.wait = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| this.settle_news(cx)).ok();
        }));
    }

    /// Runs the reload that the news asks for. While the player is open the
    /// reload stays owed until the player closes.
    fn settle_news(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.sync.socket.news;
        state.first = None;
        if self.player_open {
            state.owed = true;
            return;
        }
        state.owed = false;
        let library = std::mem::take(&mut state.library);
        state.pending = false;
        let shows = match self.page {
            Page::Home(_) => news::Shows::Home,
            Page::Library(_) => news::Shows::Library,
            _ => news::Shows::Other,
        };
        let outcome = news::outcome(library, shows, false);
        log::info!("server news settled: {outcome:?}");
        state.stats.reloaded(&outcome);
        if outcome.catalog {
            self.reload_catalog(cx);
        }
        if outcome.page {
            self.load_page(cx);
        }
    }

    /// The player view closed: runs the reload that came due under it.
    pub fn news_player_closed(&mut self, cx: &mut Context<Self>) {
        if self.sync.socket.news.owed {
            self.settle_news(cx);
        }
    }

    /// The counters of `events`.
    pub fn news_describe(&self) -> String {
        self.sync.socket.news.stats.describe(self.sync.socket.news.pending)
    }
}
