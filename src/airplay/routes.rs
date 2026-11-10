// SPDX-License-Identifier: AGPL-3.0-or-later
//! Names of the AirPlay receivers on the network, from a Bonjour browse of
//! `_airplay._tcp` through the dns_sd library of the system. The browse
//! only listens to what the receivers announce; it does not connect to
//! them. The system route picker chooses the route; this list is for the
//! panel and the debug channel.

use std::{
    ffi::{CStr, c_char, c_void},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

type DNSServiceRef = *mut c_void;
type Reply = unsafe extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

// dns_sd and poll are part of libSystem, which every binary links.
unsafe extern "C" {
    fn DNSServiceBrowse(
        reference: *mut DNSServiceRef,
        flags: u32,
        interface: u32,
        regtype: *const c_char,
        domain: *const c_char,
        callback: Reply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(reference: DNSServiceRef) -> i32;
    fn DNSServiceProcessResult(reference: DNSServiceRef) -> i32;
    fn DNSServiceRefDeallocate(reference: DNSServiceRef);
    fn poll(fds: *mut PollFd, count: u32, timeout_ms: i32) -> i32;
}

const FLAG_ADD: u32 = 0x2;
const POLLIN: i16 = 0x1;

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

unsafe extern "C" fn on_reply(
    _reference: DNSServiceRef,
    flags: u32,
    _interface: u32,
    error: i32,
    name: *const c_char,
    _regtype: *const c_char,
    _domain: *const c_char,
    context: *mut c_void,
) {
    if error != 0 || name.is_null() {
        return;
    }
    let routes = unsafe { &*(context as *const Routes) };
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
    routes.note(name, flags & FLAG_ADD != 0);
}

/// Starts the browse on a thread of its own and returns the list it fills.
/// The thread ends after [`Routes::stop`].
pub fn browse() -> Arc<Routes> {
    let routes = Arc::new(Routes::default());
    let shared = routes.clone();
    let _ = thread::Builder::new().name("airplay-routes".into()).spawn(move || {
        let mut reference: DNSServiceRef = std::ptr::null_mut();
        let context = Arc::as_ptr(&shared) as *mut c_void;
        let code = unsafe {
            DNSServiceBrowse(&mut reference, 0, 0, c"_airplay._tcp".as_ptr(), std::ptr::null(), on_reply, context)
        };
        if code != 0 {
            *shared.error.lock().unwrap() = Some(format!("DNSServiceBrowse failed with {code}"));
            return;
        }
        let fd = unsafe { DNSServiceRefSockFD(reference) };
        while !shared.stop.load(Ordering::Relaxed) {
            let mut pfd = PollFd { fd, events: POLLIN, revents: 0 };
            let ready = unsafe { poll(&mut pfd, 1, 1000) };
            if ready > 0 && unsafe { DNSServiceProcessResult(reference) } != 0 {
                *shared.error.lock().unwrap() = Some("browse connection lost".into());
                break;
            }
            if ready < 0 {
                thread::sleep(Duration::from_millis(200));
            }
        }
        unsafe { DNSServiceRefDeallocate(reference) };
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
