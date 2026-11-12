// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bonjour through the dns_sd library of the system: a browse for the
//! instances of a service, the host, port and TXT record of one, and the
//! address of a host. The system keeps the cache, listens on every
//! interface and asks the network; nothing here opens a socket of its own
//! or connects to what it finds.
//!
//! Two parts of the app use it: the names of the AirPlay receivers
//! (`src/airplay/routes.rs`) and the search for cast devices
//! (`chromecast::discovery`). Each runs its browse on a thread of its own
//! and calls [`Browser::wait`] in a loop.

use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    net::Ipv4Addr,
    time::{Duration, Instant},
};

type DNSServiceRef = *mut c_void;

type BrowseReply = unsafe extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);

type ResolveReply = unsafe extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    u16,
    u16,
    *const u8,
    *mut c_void,
);

type AddressReply = unsafe extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const u8,
    u32,
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
        callback: BrowseReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceResolve(
        reference: *mut DNSServiceRef,
        flags: u32,
        interface: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        callback: ResolveReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceGetAddrInfo(
        reference: *mut DNSServiceRef,
        flags: u32,
        interface: u32,
        protocol: u32,
        hostname: *const c_char,
        callback: AddressReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(reference: DNSServiceRef) -> i32;
    fn DNSServiceProcessResult(reference: DNSServiceRef) -> i32;
    fn DNSServiceRefDeallocate(reference: DNSServiceRef);
    fn poll(fds: *mut PollFd, count: u32, timeout_ms: i32) -> i32;
}

const FLAG_ADD: u32 = 0x2;
const PROTOCOL_IPV4: u32 = 0x1;
const POLLIN: i16 = 0x1;
const AF_INET: u8 = 2;

/// One instance of a service, as a browse names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    /// The name of the instance, such as "Living Room".
    pub name: String,
    pub regtype: String,
    pub domain: String,
    /// The interface the instance was seen on.
    pub interface: u32,
}

/// What a browse saw: an instance came, or one went. An instance seen on
/// two interfaces comes twice and goes once for each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Added(Service),
    Removed(Service),
}

/// Where an instance is: from [`resolve`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub host: String,
    pub port: u16,
    /// The TXT record as it came; [`txt_fields`] reads it.
    pub txt: Vec<u8>,
}

/// Waits for the socket of a request to have an answer; true when it has.
fn readable(reference: DNSServiceRef, wait: Duration) -> bool {
    let fd = unsafe { DNSServiceRefSockFD(reference) };
    let mut pfd = PollFd { fd, events: POLLIN, revents: 0 };
    let ms = wait.as_millis().min(i32::MAX as u128) as i32;
    unsafe { poll(&mut pfd, 1, ms) > 0 }
}

/// A browse for the instances of one service. It lives on the thread that
/// made it; dropping it ends the browse.
pub struct Browser {
    reference: DNSServiceRef,
    /// What the callback saw since the last `wait`. In a box: the system
    /// keeps its address.
    seen: Box<Vec<Change>>,
}

impl Browser {
    /// Starts a browse for a service type, such as `_airplay._tcp`.
    pub fn new(regtype: &CStr) -> Result<Self, String> {
        let mut seen = Box::new(Vec::new());
        let mut reference: DNSServiceRef = std::ptr::null_mut();
        let context = &mut *seen as *mut Vec<Change> as *mut c_void;
        let code = unsafe {
            DNSServiceBrowse(&mut reference, 0, 0, regtype.as_ptr(), std::ptr::null(), on_browse, context)
        };
        if code != 0 {
            return Err(format!("DNSServiceBrowse failed with {code}"));
        }
        Ok(Self { reference, seen })
    }

    /// Waits up to `wait` for the browse to see something, and gives what
    /// it saw; nothing when the time passed. An error is the end of the
    /// connection to the system: the browse sees nothing more.
    pub fn wait(&mut self, wait: Duration) -> Result<Vec<Change>, String> {
        if readable(self.reference, wait) && unsafe { DNSServiceProcessResult(self.reference) } != 0 {
            return Err("browse connection lost".into());
        }
        Ok(std::mem::take(&mut *self.seen))
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        unsafe { DNSServiceRefDeallocate(self.reference) };
    }
}

fn text(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned()
}

unsafe extern "C" fn on_browse(
    _reference: DNSServiceRef,
    flags: u32,
    interface: u32,
    error: i32,
    name: *const c_char,
    regtype: *const c_char,
    domain: *const c_char,
    context: *mut c_void,
) {
    if error != 0 || name.is_null() {
        return;
    }
    let seen = unsafe { &mut *(context as *mut Vec<Change>) };
    let service = Service { name: text(name), regtype: text(regtype), domain: text(domain), interface };
    seen.push(if flags & FLAG_ADD != 0 { Change::Added(service) } else { Change::Removed(service) });
}

/// Runs one request until its callback filled `answer` or the time is up.
fn one_answer<T>(
    wait: Duration,
    start: impl FnOnce(*mut DNSServiceRef, *mut c_void) -> i32,
) -> Option<T> {
    let mut answer: Option<T> = None;
    let mut reference: DNSServiceRef = std::ptr::null_mut();
    if start(&mut reference, &mut answer as *mut Option<T> as *mut c_void) != 0 {
        return None;
    }
    let until = Instant::now() + wait;
    while answer.is_none() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() || !readable(reference, left) || unsafe { DNSServiceProcessResult(reference) } != 0 {
            break;
        }
    }
    unsafe { DNSServiceRefDeallocate(reference) };
    answer
}

/// The host, the port and the TXT record of an instance; none when the
/// network does not say within `wait`, or the instance is gone.
pub fn resolve(service: &Service, wait: Duration) -> Option<Resolved> {
    let name = CString::new(service.name.as_str()).ok()?;
    let regtype = CString::new(service.regtype.as_str()).ok()?;
    let domain = CString::new(service.domain.as_str()).ok()?;
    one_answer(wait, |reference, context| unsafe {
        DNSServiceResolve(
            reference,
            0,
            service.interface,
            name.as_ptr(),
            regtype.as_ptr(),
            domain.as_ptr(),
            on_resolve,
            context,
        )
    })
}

unsafe extern "C" fn on_resolve(
    _reference: DNSServiceRef,
    _flags: u32,
    _interface: u32,
    error: i32,
    _fullname: *const c_char,
    host: *const c_char,
    port: u16,
    txt_len: u16,
    txt: *const u8,
    context: *mut c_void,
) {
    if error != 0 || host.is_null() {
        return;
    }
    let answer = unsafe { &mut *(context as *mut Option<Resolved>) };
    let txt = if txt.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(txt, txt_len as usize) }.to_vec() };
    // The port comes in the byte order of the network.
    *answer = Some(Resolved { host: text(host), port: u16::from_be(port), txt });
}

/// The IPv4 address of a host, such as "abc123.local."; none when the
/// network does not say within `wait`.
pub fn address_v4(host: &str, interface: u32, wait: Duration) -> Option<Ipv4Addr> {
    let host = CString::new(host).ok()?;
    one_answer(wait, |reference, context| unsafe {
        DNSServiceGetAddrInfo(reference, 0, interface, PROTOCOL_IPV4, host.as_ptr(), on_address, context)
    })
}

unsafe extern "C" fn on_address(
    _reference: DNSServiceRef,
    flags: u32,
    _interface: u32,
    error: i32,
    _hostname: *const c_char,
    address: *const u8,
    _ttl: u32,
    context: *mut c_void,
) {
    if error != 0 || address.is_null() || flags & FLAG_ADD == 0 {
        return;
    }
    // A `sockaddr_in` is 16 bytes; its first two say what it is.
    let bytes = unsafe { std::slice::from_raw_parts(address, 2) };
    if bytes[1] != AF_INET {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(address, 8) };
    let answer = unsafe { &mut *(context as *mut Option<Ipv4Addr>) };
    *answer = ipv4_of(bytes);
}

/// The address in a `sockaddr_in` of this system: a length and a family
/// of one byte each, the port, then the four bytes of the address.
fn ipv4_of(sockaddr: &[u8]) -> Option<Ipv4Addr> {
    match sockaddr {
        [_, AF_INET, _, _, a, b, c, d, ..] => Some(Ipv4Addr::new(*a, *b, *c, *d)),
        _ => None,
    }
}

/// The `key=value` strings of a TXT record: each one has its length in
/// the byte before it.
pub fn txt_fields(mut data: &[u8]) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    while let Some((&len, rest)) = data.split_first() {
        let len = len as usize;
        if rest.len() < len {
            break;
        }
        let entry = String::from_utf8_lossy(&rest[..len]);
        if let Some((key, value)) = entry.split_once('=') {
            fields.insert(key.to_string(), value.to_string());
        }
        data = &rest[len..];
    }
    fields
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A TXT record of these entries, as it is on the wire.
    pub(crate) fn txt(entries: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in entries {
            out.push(entry.len() as u8);
            out.extend_from_slice(entry.as_bytes());
        }
        out
    }

    #[test]
    fn reads_the_fields_of_a_txt_record() {
        let record = txt(&["id=abc123", "md=Google TV", "fn=Living room", "rs=", "flag"]);
        let fields = txt_fields(&record);
        assert_eq!(fields.get("id").map(String::as_str), Some("abc123"));
        assert_eq!(fields.get("md").map(String::as_str), Some("Google TV"));
        assert_eq!(fields.get("fn").map(String::as_str), Some("Living room"));
        assert_eq!(fields.get("rs").map(String::as_str), Some(""));
        assert!(!fields.contains_key("flag"), "an entry with no = is no field");
        // A length that runs past the record ends the read; what came
        // before it stays.
        let mut cut = txt(&["id=abc123"]);
        cut.extend_from_slice(&[40, b'x']);
        assert_eq!(txt_fields(&cut).len(), 1);
        assert!(txt_fields(&[]).is_empty());
    }

    #[test]
    fn reads_the_address_of_a_sockaddr() {
        let v4 = [16, AF_INET, 0x1f, 0x49, 192, 168, 1, 40, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(ipv4_of(&v4), Some(Ipv4Addr::new(192, 168, 1, 40)));
        // An IPv6 address (family 30) is not one.
        assert_eq!(ipv4_of(&[28, 30, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(ipv4_of(&[16, AF_INET]), None);
    }

    type RegisterReply =
        unsafe extern "C" fn(DNSServiceRef, u32, i32, *const c_char, *const c_char, *const c_char, *mut c_void);

    unsafe extern "C" {
        fn DNSServiceRegister(
            reference: *mut DNSServiceRef,
            flags: u32,
            interface: u32,
            name: *const c_char,
            regtype: *const c_char,
            domain: *const c_char,
            host: *const c_char,
            port: u16,
            txt_len: u16,
            txt: *const u8,
            callback: Option<RegisterReply>,
            context: *mut c_void,
        ) -> i32;
    }

    /// The whole path against the system: this test announces a service
    /// of a type of its own from this Mac, and the browse, the resolve and
    /// the address find it. It talks to the Bonjour of the system and to
    /// no device, and it needs a network, so it runs by hand:
    /// `cargo test --release -p bloom-core dnssd -- --ignored`.
    #[test]
    #[ignore]
    fn finds_a_service_this_test_announces() {
        let record = txt(&["id=bloom-test-1", "fn=Bloom test"]);
        let mut announced: DNSServiceRef = std::ptr::null_mut();
        let code = unsafe {
            DNSServiceRegister(
                &mut announced,
                0,
                0,
                c"Bloom dnssd test".as_ptr(),
                c"_bloomtest._tcp".as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                8009u16.to_be(),
                record.len() as u16,
                record.as_ptr(),
                None,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(code, 0, "DNSServiceRegister");
        let mut browser = Browser::new(c"_bloomtest._tcp").unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while found.is_none() && Instant::now() < until {
            for change in browser.wait(Duration::from_millis(250)).unwrap() {
                if let Change::Added(service) = change
                    && service.name == "Bloom dnssd test"
                {
                    found = Some(service);
                }
            }
        }
        let service = found.expect("the browse did not see the service");
        let resolved = resolve(&service, Duration::from_secs(5)).expect("no resolve");
        assert_eq!(resolved.port, 8009);
        assert_eq!(txt_fields(&resolved.txt).get("id").map(String::as_str), Some("bloom-test-1"));
        let address = address_v4(&resolved.host, service.interface, Duration::from_secs(5));
        assert!(address.is_some(), "no address for {}", resolved.host);
        unsafe { DNSServiceRefDeallocate(announced) };
    }
}
