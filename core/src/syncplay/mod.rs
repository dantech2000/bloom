// SPDX-License-Identifier: AGPL-3.0-or-later
//! SyncPlay without the app: the messages of the server, the clock, the
//! drift correction and the state machine of a member. The app adds the
//! session with the player and the panel (`src/syncplay`).

pub mod clock;
pub mod core;
pub mod drift;
pub mod protocol;
