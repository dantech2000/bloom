// SPDX-License-Identifier: AGPL-3.0-or-later
//! Google Cast without the app: the framing of the messages (`proto`), the
//! messages themselves (`messages`), the search for devices (`discovery`), the
//! connection with its thread (`session`), and the two ways to play a
//! Jellyfin item (`jellyfin`). The app adds the panel and the debug
//! channel (`src/chromecast`, `src/cast`).

pub mod discovery;
pub mod jellyfin;
pub mod messages;
/// The fake device of the tests; the app runs it as well when
/// `BLOOM_CHROMECAST_MOCK` is set, so the panel can be driven against it.
/// The build that ships has none (the `cast-mock` feature is off there).
#[cfg(any(test, feature = "cast-mock"))]
pub mod mock;
pub mod proto;
pub mod session;
