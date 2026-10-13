// SPDX-License-Identifier: AGPL-3.0-or-later
//! Google Cast without the app: the framing of the messages, the messages
//! themselves and the search for devices. The app adds the connection and
//! the panel (`src/chromecast`).

pub mod mdns;
pub mod messages;
pub mod proto;
