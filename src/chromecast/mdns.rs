// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finds cast devices on the local network. One UDP socket on an ordinary
//! port asks the multicast group for `_googlecast._tcp.local` with the
//! "answer me directly" bit set (RFC 6762, QU); the devices answer with
//! their PTR, SRV, TXT and A records in one packet. A thread asks again
//! now and then and keeps the list fresh.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const SERVICE: &str = "_googlecast._tcp.local";
const GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
/// Time between two questions once the first answers are in.
const REFRESH: Duration = Duration::from_secs(15);
/// A device that did not answer for this long is gone.
const FORGET: Duration = Duration::from_secs(90);

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The `id` of the TXT record: stays the same across restarts.
    pub id: String,
    /// The name the user gave the device (`fn`).
    pub name: String,
    /// The model (`md`), such as "Chromecast" or "Google TV".
    pub model: String,
    pub address: Ipv4Addr,
    pub port: u16,
    /// The instance name of the service, for the log.
    pub instance: String,
}

/// Keeps the list of devices fresh until it is dropped.
pub struct Discovery {
    devices: Arc<Mutex<Vec<(Device, Instant)>>>,
    stop: Arc<AtomicBool>,
    ask: Arc<AtomicBool>,
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Discovery {
    /// The devices seen within `FORGET`, in the order they were first
    /// seen. The thread prunes the list as well; this read does not wait
    /// for it.
    pub fn devices(&self) -> Vec<Device> {
        let mut list = self.devices.lock().unwrap();
        forget_silent(&mut list);
        list.iter().map(|(device, _)| device.clone()).collect()
    }

    /// Asks again now.
    pub fn rescan(&self) {
        self.ask.store(true, Ordering::Release);
    }
}

/// Drops the devices that did not answer for `FORGET`.
fn forget_silent(list: &mut Vec<(Device, Instant)>) {
    list.retain(|(_, seen)| seen.elapsed() < FORGET);
}

/// Starts the thread that looks for devices.
pub fn discover() -> Discovery {
    let devices = Arc::new(Mutex::new(Vec::<(Device, Instant)>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let ask = Arc::new(AtomicBool::new(false));
    let (shared, stopped, asked) = (devices.clone(), stop.clone(), ask.clone());
    let _ = thread::Builder::new().name("cast-discovery".into()).spawn(move || {
        let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
            Ok(socket) => socket,
            Err(err) => {
                log::warn!("cast discovery: no socket: {err}");
                return;
            }
        };
        let _ = socket.set_read_timeout(Some(Duration::from_millis(250)));
        let _ = socket.set_multicast_ttl_v4(255);
        let query = query();
        // The first questions come fast; a device that missed one gets
        // the next.
        let mut asks = [0., 1., 3.].into_iter().map(Duration::from_secs_f64).collect::<Vec<_>>();
        let started = Instant::now();
        let mut last_ask = Instant::now() - REFRESH;
        let mut buffer = [0u8; 9000];
        while !stopped.load(Ordering::Acquire) {
            let due = asks.first().is_some_and(|at| started.elapsed() >= *at)
                || last_ask.elapsed() >= REFRESH
                || asked.swap(false, Ordering::AcqRel);
            if due {
                if asks.first().is_some_and(|at| started.elapsed() >= *at) {
                    asks.remove(0);
                }
                last_ask = Instant::now();
                if let Err(err) = socket.send_to(&query, GROUP) {
                    log::debug!("cast discovery: send: {err}");
                }
            }
            match socket.recv_from(&mut buffer) {
                Ok((n, _)) => {
                    let found = parse(&buffer[..n]);
                    if !found.is_empty() {
                        let mut list = shared.lock().unwrap();
                        for device in found {
                            match list.iter_mut().find(|(known, _)| known.id == device.id) {
                                Some(entry) => *entry = (device, Instant::now()),
                                None => {
                                    log::info!(
                                        "cast device: {:?} ({}) at {}:{}",
                                        device.name,
                                        device.model,
                                        device.address,
                                        device.port
                                    );
                                    list.push((device, Instant::now()));
                                }
                            }
                        }
                    }
                }
                Err(_) => {}
            }
            // A device that went away is forgotten whether or not another
            // one answers in its place.
            forget_silent(&mut shared.lock().unwrap());
        }
    });
    Discovery { devices, stop, ask }
}

/// One question: PTR for the service, with the QU bit.
fn query() -> Vec<u8> {
    let mut out = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in SERVICE.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&TYPE_PTR.to_be_bytes());
    out.extend_from_slice(&0x8001u16.to_be_bytes());
    out
}

/// The devices in one answer packet.
pub fn parse(packet: &[u8]) -> Vec<Device> {
    let Some(records) = records(packet) else {
        return Vec::new();
    };
    let mut instances = Vec::new();
    let mut srv: HashMap<String, (u16, String)> = HashMap::new();
    let mut txt: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut hosts: HashMap<String, Ipv4Addr> = HashMap::new();
    for record in &records {
        match record.kind {
            TYPE_PTR if record.name.eq_ignore_ascii_case(SERVICE) => {
                if let Some(target) = read_name(packet, record.data_at).map(|(name, _)| name) {
                    instances.push(target);
                }
            }
            TYPE_SRV => {
                let data = &packet[record.data_at..record.data_at + record.data_len];
                if data.len() >= 6
                    && let Some((target, _)) = read_name(packet, record.data_at + 6)
                {
                    let port = u16::from_be_bytes([data[4], data[5]]);
                    srv.insert(record.name.to_lowercase(), (port, target));
                }
            }
            TYPE_TXT => {
                let data = &packet[record.data_at..record.data_at + record.data_len];
                txt.insert(record.name.to_lowercase(), read_txt(data));
            }
            TYPE_A => {
                let data = &packet[record.data_at..record.data_at + record.data_len];
                if let [a, b, c, d] = data {
                    hosts.insert(record.name.to_lowercase(), Ipv4Addr::new(*a, *b, *c, *d));
                }
            }
            _ => {}
        }
    }
    // A packet with SRV and TXT but no PTR is an answer as well.
    for name in srv.keys() {
        if !instances.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            instances.push(name.clone());
        }
    }
    let mut devices = Vec::new();
    for instance in instances {
        let key = instance.to_lowercase();
        let Some((port, target)) = srv.get(&key) else { continue };
        let Some(address) = hosts.get(&target.to_lowercase()) else { continue };
        let fields = txt.get(&key).cloned().unwrap_or_default();
        let short = instance.strip_suffix(&format!(".{SERVICE}")).unwrap_or(&instance).to_string();
        devices.push(Device {
            id: fields.get("id").cloned().unwrap_or_else(|| short.clone()),
            name: fields.get("fn").cloned().unwrap_or_else(|| short.clone()),
            model: fields.get("md").cloned().unwrap_or_default(),
            address: *address,
            port: *port,
            instance: short,
        });
    }
    devices
}

struct Record {
    name: String,
    kind: u16,
    data_at: usize,
    data_len: usize,
}

/// Every record of the packet, from all three sections.
fn records(packet: &[u8]) -> Option<Vec<Record>> {
    if packet.len() < 12 {
        return None;
    }
    let count = |at: usize| u16::from_be_bytes([packet[at], packet[at + 1]]) as usize;
    let (questions, answers) = (count(4), count(6) + count(8) + count(10));
    let mut at = 12;
    for _ in 0..questions {
        let (_, next) = read_name(packet, at)?;
        at = next + 4;
    }
    let mut records = Vec::new();
    for _ in 0..answers {
        let (name, next) = read_name(packet, at)?;
        if packet.len() < next + 10 {
            return None;
        }
        let kind = u16::from_be_bytes([packet[next], packet[next + 1]]);
        let data_len = u16::from_be_bytes([packet[next + 8], packet[next + 9]]) as usize;
        let data_at = next + 10;
        if packet.len() < data_at + data_len {
            return None;
        }
        records.push(Record { name, kind, data_at, data_len });
        at = data_at + data_len;
    }
    Some(records)
}

/// A name at `at`, with pointers followed; gives the offset after it.
fn read_name(packet: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut after = None;
    let mut hops = 0;
    loop {
        let len = *packet.get(at)? as usize;
        if len == 0 {
            at += 1;
            break;
        }
        if len & 0xc0 == 0xc0 {
            let pointer = ((len & 0x3f) << 8) | *packet.get(at + 1)? as usize;
            after.get_or_insert(at + 2);
            hops += 1;
            if hops > 16 || pointer >= packet.len() {
                return None;
            }
            at = pointer;
            continue;
        }
        let label = packet.get(at + 1..at + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).to_string());
        at += 1 + len;
    }
    Some((labels.join("."), after.unwrap_or(at)))
}

/// The `key=value` strings of a TXT record.
fn read_txt(mut data: &[u8]) -> HashMap<String, String> {
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
mod tests {
    use super::*;

    fn name(out: &mut Vec<u8>, name: &str) {
        for label in name.split('.') {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
    }

    fn record(out: &mut Vec<u8>, owner: &[u8], kind: u16, data: &[u8]) {
        out.extend_from_slice(owner);
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&0x8001u16.to_be_bytes());
        out.extend_from_slice(&120u32.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
    }

    /// An answer the way a device sends it: PTR, SRV, TXT, A, with the
    /// instance and host names compressed.
    fn answer() -> Vec<u8> {
        let mut out = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        // PTR at 12: owner is the service name, data the instance.
        let mut ptr = Vec::new();
        name(&mut ptr, "Chromecast-abc123._googlecast._tcp.local");
        let service_at = 12;
        let mut owner = Vec::new();
        name(&mut owner, SERVICE);
        record(&mut out, &owner, TYPE_PTR, &ptr);
        // The instance is at the start of the PTR data.
        let instance_at = service_at + owner.len() + 10;
        let instance_pointer = [0xc0 | (instance_at >> 8) as u8, instance_at as u8];
        // SRV: priority, weight, port 8009, target host.
        let mut srv = vec![0, 0, 0, 0, 0x1f, 0x49];
        name(&mut srv, "abc123.local");
        record(&mut out, &instance_pointer, TYPE_SRV, &srv);
        let mut txt = Vec::new();
        for entry in ["id=abc123", "md=Google TV", "fn=Living room", "rs="] {
            txt.push(entry.len() as u8);
            txt.extend_from_slice(entry.as_bytes());
        }
        record(&mut out, &instance_pointer, TYPE_TXT, &txt);
        let mut host = Vec::new();
        name(&mut host, "abc123.local");
        record(&mut out, &host, TYPE_A, &[192, 168, 1, 40]);
        out
    }

    #[test]
    fn parses_an_answer_with_compressed_names() {
        let devices = parse(&answer());
        assert_eq!(
            devices,
            vec![Device {
                id: "abc123".into(),
                name: "Living room".into(),
                model: "Google TV".into(),
                address: Ipv4Addr::new(192, 168, 1, 40),
                port: 8009,
                instance: "Chromecast-abc123".into(),
            }]
        );
    }

    #[test]
    fn the_question_asks_for_a_direct_answer() {
        let q = query();
        assert_eq!(&q[..12], &[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert!(q.ends_with(&[0, 0, 12, 0x80, 0x01]));
        assert_eq!(q[12], 11);
        assert_eq!(&q[13..24], b"_googlecast");
    }

    /// A device that stopped answering is gone from the list after
    /// `FORGET`, with no other device around to answer in its place.
    #[test]
    fn a_silent_device_is_forgotten_without_other_answers() {
        let device = parse(&answer()).remove(0);
        let stale = Instant::now() - FORGET - Duration::from_secs(1);
        let discovery = Discovery {
            devices: Arc::new(Mutex::new(vec![(device, stale)])),
            stop: Arc::new(AtomicBool::new(false)),
            ask: Arc::new(AtomicBool::new(false)),
        };
        assert!(discovery.devices().is_empty(), "{:?}", discovery.devices());
    }

    #[test]
    fn ignores_short_and_looping_packets() {
        assert!(parse(&[0; 5]).is_empty());
        let mut looping = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        looping.extend_from_slice(&[0xc0, 12, 0, 12, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert!(parse(&looping).is_empty());
    }
}
