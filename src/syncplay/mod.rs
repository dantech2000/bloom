// SPDX-License-Identifier: AGPL-3.0-or-later
//! SyncPlay: playback of a group of clients in step. The server sends
//! commands with a time on its clock; each client runs them at that time.

pub mod clock;
pub mod core;
pub mod drift;
pub mod protocol;
pub mod session;
pub mod ui;
