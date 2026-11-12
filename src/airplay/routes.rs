// SPDX-License-Identifier: AGPL-3.0-or-later
//! Names of the AirPlay receivers on the network, from a Bonjour browse of
//! `_airplay._tcp` through the dns_sd library of the system
//! (`bloom_core::dnssd`, which the search for cast devices uses too). The browse
//! only listens to what the receivers announce; it does not connect to
//! them. The system route picker chooses the route; this list is for the
//! panel and the debug channel.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use bloom_core::dnssd::{Browser, Change};

/// The names seen so far, in the order they came.
#[derive(Default)]
pub struct Routes {
    /// Each name with how often it was added and not yet removed.
    seen: Mutex<Vec<(String, u32)>>,
    /// Set when the browse could not start, with the reason.
    error: Mutex<Option<String>>,
    /// The browse is told to end; its thread looks once a second.
    stop: AtomicBool,
}

impl Routes {
    pub fn names(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(|(name, _)| name.clone()).collect()
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    /// Ends the browse: its thread stops listening within a second.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// A receiver came (`added`) or went. One seen on two interfaces is
    /// added twice and removed once for each: it is gone when the last is.
    fn note(&self, name: String, added: bool) {
        let mut seen = self.seen.lock().unwrap();
        let place = seen.iter().position(|(known, _)| *known == name);
        match (place, added) {
            (Some(place), true) => seen[place].1 += 1,
            (None, true) => seen.push((name, 1)),
            (Some(place), false) if seen[place].1 <= 1 => {
                seen.remove(place);
            }
            (Some(place), false) => seen[place].1 -= 1,
            (None, false) => {}
        }
    }
}

/// Starts the browse on a thread of its own and returns the list it fills.
/// The thread ends after [`Routes::stop`].
pub fn browse() -> Arc<Routes> {
    let routes = Arc::new(Routes::default());
    let shared = routes.clone();
    let _ = thread::Builder::new().name("airplay-routes".into()).spawn(move || {
        let mut browser = match Browser::new(c"_airplay._tcp") {
            Ok(browser) => browser,
            Err(err) => {
                *shared.error.lock().unwrap() = Some(err);
                return;
            }
        };
        while !shared.stop.load(Ordering::Relaxed) {
            match browser.wait(Duration::from_secs(1)) {
                Ok(changes) => {
                    for change in changes {
                        match change {
                            Change::Added(service) => shared.note(service.name, true),
                            Change::Removed(service) => shared.note(service.name, false),
                        }
                    }
                }
                Err(err) => {
                    *shared.error.lock().unwrap() = Some(err);
                    break;
                }
            }
        }
    });
    routes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receiver_on_two_interfaces_is_gone_with_the_last_one() {
        let routes = Routes::default();
        routes.note("Living Room".into(), true);
        routes.note("Bedroom".into(), true);
        routes.note("Living Room".into(), true);
        assert_eq!(routes.names(), ["Living Room", "Bedroom"]);
        routes.note("Living Room".into(), false);
        assert_eq!(routes.names(), ["Living Room", "Bedroom"]);
        routes.note("Living Room".into(), false);
        routes.note("Never Seen".into(), false);
        assert_eq!(routes.names(), ["Bedroom"]);
    }
}
