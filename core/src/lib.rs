// SPDX-License-Identifier: AGPL-3.0-or-later
//! The parts of Bloom that know nothing of its window: the client of the
//! Jellyfin server, the config file, and the state machines of SyncPlay,
//! Google Cast and the connection. No code here names GPUI or the player,
//! so its tests run without them, and a change to a page does not build
//! this crate again.

pub mod brand;
pub mod chromecast;
pub mod config;
pub mod connection;
pub mod downloads;
pub mod jellyfin;
pub mod news;
pub mod options;
pub mod realtime;
pub mod search_index;
pub mod syncplay;
#[cfg(test)]
mod test_net;
