// SPDX-License-Identifier: AGPL-3.0-or-later
//! The parts of Bloom that know nothing of its window: the client of the
//! Jellyfin server, the config file, and the state machines of SyncPlay,
//! Google Cast and the connection. No code here names GPUI or the player,
//! so its tests run without them, and a change to a page does not build
//! this crate again.

pub mod bitrate;
pub mod brand;
pub mod cast;
pub mod chromecast;
pub mod config;
pub mod connection;
pub mod dnssd;
pub mod downloads;
pub mod draft;
pub mod events;
pub mod fsutil;
pub mod images;
pub mod jellyfin;
pub mod languages;
pub mod news;
pub mod options;
pub mod playback;
pub mod quality_tags;
pub mod queue;
pub mod realtime;
pub mod search_index;
pub mod subtitles;
pub mod syncplay;
pub mod trailers;
#[cfg(test)]
mod test_net;
