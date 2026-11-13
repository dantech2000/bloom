// SPDX-License-Identifier: AGPL-3.0-or-later
//! Google Cast: plays an item on a Chromecast or a TV with cast built in.
//! Our own sender, small, in `bloom-core` (`bloom_core::chromecast`): the
//! one protobuf message by hand, the Bonjour browse for the devices
//! (`discovery`), the JSON of the namespaces (`messages`), the connection
//! with its thread (`session`), and the two ways to play a Jellyfin item
//! (`jellyfin`). No part of the interface is in here; `debug` drives it
//! from the debug channel.
//!
//! The API for a panel:
//! - [`discovery::discover`] gives a [`discovery::Discovery`]; its
//!   `devices()` is the fresh list.
//! - [`session::Session::connect`] opens a device and gives the handle and
//!   the channel of [`session::Event`]s. `status()` is the last picture.
//! - [`session::LoadRequest`] names an item both ways; the session takes
//!   the Jellyfin app first and the Default Media Receiver when that app
//!   does not launch.

pub mod debug;
#[cfg(feature = "cast-mock")]
pub use bloom_core::chromecast::mock;
/// The engine is in `bloom-core`, where its tests run without the app.
pub use bloom_core::chromecast::{discovery, jellyfin, messages, session};

/// What the app keeps: the search, a connection the debug channel opened
/// with no target (the target has its own, `cast::remote`), and the last
/// events for the debug channel.
#[derive(Default)]
pub struct State {
    pub discovery: Option<discovery::Discovery>,
    /// The debug channel asked for the search: it stays with the panel
    /// closed (`Bloom::stop_cast_search`).
    pub keep_search: bool,
    /// A connection to a device that is not the target: the debug channel
    /// looks at a TV through it, and the next play does not go there.
    pub probe: Option<crate::cast::remote::ChromecastRemote>,
    /// The last events, one line each, newest last.
    pub events: Vec<String>,
}

impl State {
    pub fn note(&mut self, event: &session::Event) {
        let line = match event {
            session::Event::Connected { again } => format!("connected again={again}"),
            session::Event::Status(status) => format!(
                "status app={} player={} pos={:.1}",
                status.app.as_ref().map_or("-", |app| app.app_id.as_str()),
                status.player_state.map_or("-", |state| state.name()),
                status.position
            ),
            session::Event::Message { namespace, payload } => {
                format!("message {namespace} {}", payload["type"].as_str().unwrap_or(""))
            }
            session::Event::Error(text) => format!("error {text}"),
            session::Event::Ended { load } => format!("ended load={load}"),
            session::Event::Pinned(_) => "certificate kept".into(),
            session::Event::Closed => "closed".into(),
        };
        self.events.push(line);
        if self.events.len() > 30 {
            self.events.remove(0);
        }
    }
}
