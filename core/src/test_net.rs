// SPDX-License-Identifier: AGPL-3.0-or-later
//! For the test servers of this crate.

use std::{
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The connections to the listener of a test server, until `stop` is set.
/// The listener does not block: the loop looks at `stop` between two
/// tries, so it ends with no last connection. A server that waited for one
/// to stop hung its test when that connect failed (seen on 2026-10-12: ten
/// tests at once, for good).
pub(crate) fn connections(listener: TcpListener, stop: Arc<AtomicBool>) -> impl Iterator<Item = TcpStream> {
    listener.set_nonblocking(true).expect("nonblocking");
    std::iter::from_fn(move || {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                // A stream takes the mode of its listener on macOS.
                Ok((stream, _)) if stream.set_nonblocking(false).is_ok() => return Some(stream),
                _ => std::thread::sleep(std::time::Duration::from_micros(500)),
            }
        }
        None
    })
}
