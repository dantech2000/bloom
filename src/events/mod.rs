// SPDX-License-Identifier: AGPL-3.0-or-later
//! Events for other programs: what the player does, as it happens, as one
//! line of JSON for each event on a socket of this Mac. A program reads the
//! lines and does what its user wants with them (dims the lights at the
//! start of a film, sets a status). The design is in
//! `specs/outbound-events.md`; this is its first stage, a proof of concept.
//!
//! The stream is read-only: what a reader sends is not read. It is apart
//! from the debug channel, which can drive the app and write to the server.
//! Off unless the user turns it on (Settings > Automation).
//!
//! An event never has the access token, the address of the server or a
//! path of a file in it. The title of what plays is in it only when the
//! user allows that as well.
//!
//! Cost: with the stream off, an event is one read of a flag. With it on,
//! the UI thread builds the line and hands it to each reader's queue
//! without a wait; a thread for each reader does the writing, so a reader
//! that does not read holds nothing up. A reader that falls a whole queue
//! behind is closed.
//!
//! The parts: the socket and its readers (`hub`), the events between two
//! moments of the player (`diff`), the colours of a picture (`colours`),
//! the tap in the app (`tap`), the lights and their helper
//! (`lights`), and the page of the settings (`ui`).

mod colours;
mod diff;
mod hub;
mod lights;
mod tap;
mod ui;

pub use colours::{artwork_colour, picture_bands};
pub use diff::{ItemInfo, Snapshot, between};
pub use hub::hub;
pub use lights::{Lights, FilmColour, lights_path};
pub use tap::State;

/// The version of the format. 0 while the feature is a proof of concept:
/// names and fields can still change.
pub const VERSION: u32 = 0;

/// The socket of the stream, in the folder of the app. A test instance has
/// its own, so it is not taken for the app the user runs.
pub fn socket_path() -> String {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(crate::brand::FOLDER)
        .join(match &crate::options::options().test_name {
            Some(name) => format!("events-{name}.sock"),
            None => "events.sock".into(),
        })
        .display()
        .to_string()
}
