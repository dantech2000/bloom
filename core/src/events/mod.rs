// SPDX-License-Identifier: AGPL-3.0-or-later
//! The events for other programs, without the app: the socket and its
//! readers (`hub`), the events between two moments of the player (`diff`),
//! and the colours of a picture (`colours`). The app looks at its player
//! and its window and gives what it sees to these (`src/events`), where
//! the whole of the feature is told.

pub mod colours;
pub mod diff;
pub mod hub;

pub use colours::{artwork_colour, picture_bands};
pub use diff::{ItemInfo, Snapshot, between};
pub use hub::hub;

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
