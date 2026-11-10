// SPDX-License-Identifier: AGPL-3.0-or-later
//! Google Cast: plays an item on a Chromecast or a TV with cast built in.
//! Our own sender, small: the one protobuf message by hand (`proto`), the
//! question on the local network (`mdns`), the JSON of the namespaces
//! (`messages`), the connection with its thread (`session`), and the two
//! ways to play a Jellyfin item (`jellyfin`). No part of the interface is
//! in here; `debug` drives it from the debug channel.
//!
//! The API for a panel:
//! - [`mdns::discover`] gives a [`mdns::Discovery`]; its `devices()` is the
//!   fresh list.
//! - [`session::Session::connect`] opens a device and gives the handle and
//!   the channel of [`session::Event`]s. `status()` is the last picture.
//! - [`session::LoadRequest`] names an item both ways; the session takes
//!   the Jellyfin app first and the Default Media Receiver when that app
//!   does not launch.

pub mod debug;
/// The engine is in `bloom-core`, where its tests run without the app.
pub use bloom_core::chromecast::{jellyfin, mdns, messages, mock, session};

/// What the app keeps: the search, the one connection, and the last events
/// for the debug channel.
#[derive(Default)]
pub struct State {
    pub discovery: Option<mdns::Discovery>,
    /// The debug channel asked for the search: it stays with the panel
    /// closed (`Bloom::stop_cast_search`).
    pub keep_search: bool,
    pub session: Option<session::Session>,
    /// The last events, one line each, newest last.
    pub events: Vec<String>,
    /// Reads the events of the session.
    pub task: Option<gpui_kit::Task<()>>,
}

impl State {
    pub fn note(&mut self, event: &session::Event) {
        let line = match event {
            session::Event::Connected { again } => format!("connected again={again}"),
            session::Event::Status(status) => format!(
                "status app={} player={} pos={:.1}",
                status.app.as_ref().map_or("-", |app| app.app_id.as_str()),
                if status.player_state.is_empty() { "-" } else { &status.player_state },
                status.position
            ),
            session::Event::Message { namespace, payload } => {
                format!("message {namespace} {}", payload["type"].as_str().unwrap_or(""))
            }
            session::Event::Error(text) => format!("error {text}"),
            session::Event::Ended { load } => format!("ended load={load}"),
            session::Event::Closed => "closed".into(),
        };
        self.events.push(line);
        if self.events.len() > 30 {
            self.events.remove(0);
        }
    }
}
