// SPDX-License-Identifier: AGPL-3.0-or-later
//! Events for other programs: what the player does, as it happens, as one
//! line of JSON for each event on a socket of this machine. A program reads the
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
//! moments of the player (`diff`) and the colours of a picture
//! (`colours`), which know nothing of the app and are in `bloom-core`;
//! here the tap in the app (`tap`), the lights and their helper
//! (`lights`), and the page of the settings (`ui`).

mod lights;
mod tap;
mod ui;

// The parts with no window keep their names here.
pub use bloom_core::events::{ItemInfo, Snapshot, artwork_colour, between, hub, picture_bands, socket_path};
use bloom_core::events::colours;
pub use lights::{FilmColour, Lights, lights_path};
pub use tap::State;
