// SPDX-License-Identifier: AGPL-3.0-or-later
//! Govee lights over their LAN protocol: JSON in UDP packets. A lamp needs
//! "LAN Control" switched on in the Govee Home app. Nothing goes to the
//! internet, and no account or key is used.
//!
//! - Find: one packet to the group 239.255.255.250, port 4001; each lamp
//!   answers to port 4002 of this Mac with its address, id and model.
//! - Commands go to port 4003 of the lamp: `brightness`, `colorwc`,
//!   `devStatus` (answered to port 4002).
//! - The frame stream is the `razer` command with a packet in base64. Govee
//!   does not document it; it is what OpenRGB sends. Found by trial on an
//!   H6076 (2026-10-08): a frame takes at most 14 colours (with 15 the lamp
//!   shows nothing), the lamp spreads them over its length with the first
//!   at its bottom, and it shows a frame at once, with no glide of its own.
//!   A normal command the lamp glides to by itself, in well under a second.

use std::{
    net::{SocketAddr, UdpSocket},
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::driver::{Driver, Found, Lamp, LampState};

/// Where a find asks, the port lamps answer to, the port they listen on.
const GROUP: &str = "239.255.255.250:4001";
const ANSWERS: u16 = 4002;
const COMMANDS: u16 = 4003;
/// The colours a frame of the stream takes.
const FRAME: usize = 14;
/// The time between two questions of one find.
const ASK_AGAIN: Duration = Duration::from_millis(400);

/// The Govee driver. The two fields are other values for a test only.
pub struct Govee {
    pub group: String,
    pub answers: u16,
}

impl Default for Govee {
    fn default() -> Self {
        Self { group: GROUP.into(), answers: ANSWERS }
    }
}

/// The socket lamps answer to. Every lamp answers to the one port, so one
/// question is out at a time in this program (two lights that are asked at
/// the start of a film take their turns), and a question of another
/// program (a find from the settings of Bloom, next to the helper) is
/// waited for, for a moment.
fn answers_socket(port: u16) -> std::io::Result<(std::sync::MutexGuard<'static, ()>, UdpSocket)> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let until = Instant::now() + Duration::from_millis(2500);
    loop {
        match UdpSocket::bind(("0.0.0.0", port)) {
            Ok(socket) => return Ok((turn, socket)),
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(err),
        }
    }
}

fn message(cmd: &str, data: Value) -> Vec<u8> {
    json!({ "msg": { "cmd": cmd, "data": data } }).to_string().into_bytes()
}

impl Driver for Govee {
    fn name(&self) -> &'static str {
        "govee"
    }

    fn find(&self, wait: Duration) -> Vec<Found> {
        let (_turn, socket) = match answers_socket(self.answers) {
            Ok(socket) => socket,
            Err(err) => {
                eprintln!("bloom-lights: govee: no find, port {} is in use: {err}", self.answers);
                return Vec::new();
            }
        };
        // The question goes to a group, in one packet with no receipt, and
        // a lamp misses it now and then (an H6076 was found by 13 of 16
        // single questions, on or off). So it is asked again every 400 ms
        // for as long as the find waits; a lamp is listed once.
        let ask = || socket.send_to(&message("scan", json!({ "account_topic": "reserve" })), &self.group);
        if let Err(err) = ask() {
            eprintln!("bloom-lights: govee: the find was not sent: {err}");
            return Vec::new();
        }
        let until = Instant::now() + wait;
        let mut again = Instant::now() + ASK_AGAIN;
        let mut found: Vec<Found> = Vec::new();
        let mut buffer = [0u8; 2048];
        while let Some(left) = until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero()) {
            let to_ask = again.saturating_duration_since(Instant::now());
            if to_ask.is_zero() {
                let _ = ask();
                again = Instant::now() + ASK_AGAIN;
                continue;
            }
            if socket.set_read_timeout(Some(left.min(to_ask))).is_err() {
                break;
            }
            // No answer in this stretch: the time to ask again, or the end.
            let Ok((n, from)) = socket.recv_from(&mut buffer) else { continue };
            let Ok(answer) = serde_json::from_slice::<Value>(&buffer[..n]) else { continue };
            let data = &answer["msg"]["data"];
            let (Some(id), true) = (data["device"].as_str(), answer["msg"]["cmd"] == "scan") else { continue };
            // The address the answer came from, not the one it names: an
            // answer cannot name another machine for the commands. (The
            // protocol has no proof of who answers: a machine on the
            // network can say it is a lamp, as a lamp can.)
            let lamp = Found {
                driver: "govee".into(),
                id: id.into(),
                model: data["sku"].as_str().unwrap_or_default().into(),
                address: from.ip().to_string(),
            };
            if !found.iter().any(|known| known.id == lamp.id) {
                found.push(lamp);
            }
        }
        found
    }

    fn open(&self, found: &Found) -> Option<Arc<dyn Lamp>> {
        // An address, or an address with a port (a test, a forwarded port).
        let target = found.address.parse::<SocketAddr>().ok().or_else(|| format!("{}:{COMMANDS}", found.address).parse().ok())?;
        Some(Arc::new(GoveeLamp { target, answers: self.answers }))
    }
}

/// One Govee lamp.
pub struct GoveeLamp {
    target: SocketAddr,
    answers: u16,
}

impl GoveeLamp {
    fn send(&self, cmd: &str, data: Value) {
        let sent = UdpSocket::bind(("0.0.0.0", 0)).and_then(|socket| socket.send_to(&message(cmd, data), self.target));
        if let Err(err) = sent {
            eprintln!("bloom-lights: govee: {cmd} not sent to {}: {err}", self.target);
        }
    }
}

impl Lamp for GoveeLamp {
    fn state(&self) -> Option<LampState> {
        let (_turn, socket) = match answers_socket(self.answers) {
            Ok(socket) => socket,
            Err(err) => {
                eprintln!("bloom-lights: govee: the lamp cannot be asked for its state (port {}): {err}", self.answers);
                return None;
            }
        };
        socket.send_to(&message("devStatus", json!({})), self.target).ok()?;
        // Other lamps answer other programs on the same port: only the
        // answer of this lamp to this question counts.
        let until = Instant::now() + Duration::from_secs(1);
        let mut buffer = [0u8; 2048];
        loop {
            socket.set_read_timeout(Some(until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())?)).ok()?;
            let (n, from) = socket.recv_from(&mut buffer).ok()?;
            let Ok(answer) = serde_json::from_slice::<Value>(&buffer[..n]) else { continue };
            let data = &answer["msg"]["data"];
            let (Some(level), true) = (data["brightness"].as_f64(), from.ip() == self.target.ip() && answer["msg"]["cmd"] == "devStatus")
            else {
                continue;
            };
            let part = |name: &str| data["color"][name].as_u64().and_then(|value| u8::try_from(value).ok());
            return Some(LampState {
                // With no word on it the lamp counts as on.
                on: data["onOff"].as_u64() != Some(0),
                level: level.clamp(1., 100.),
                colour: part("r").zip(part("g")).zip(part("b")).map(|((r, g), b)| [r, g, b]),
                kelvin: data["colorTemInKelvin"].as_u64().filter(|kelvin| *kelvin > 0).map(|kelvin| kelvin as u32),
            });
        }
    }

    fn power(&self, on: bool) {
        self.send("turn", json!({ "value": u8::from(on) }));
    }

    fn level(&self, percent: u32) {
        self.send("brightness", json!({ "value": percent.clamp(1, 100) }));
    }

    fn colour(&self, [r, g, b]: [u8; 3]) {
        self.send("colorwc", json!({ "color": { "r": r, "g": g, "b": b }, "colorTemInKelvin": 0 }));
    }

    fn white(&self, kelvin: u32) {
        // The lamp takes a colour or a temperature, not both.
        self.send("colorwc", json!({ "color": { "r": 0, "g": 0, "b": 0 }, "colorTemInKelvin": kelvin }));
    }

    fn frame_colours(&self) -> usize {
        FRAME
    }

    fn stream(&self, on: bool) {
        self.send("razer", stream_packet(&[0xBB, 0x00, 0x01, 0xB1, u8::from(on)]));
    }

    fn frame(&self, colours: &[[u8; 3]]) {
        let colours = &colours[..colours.len().min(FRAME)];
        let mut body = vec![0xBB, 0x00, (2 + 3 * colours.len()) as u8, 0xB0, 0x00, colours.len() as u8];
        body.extend(colours.iter().flatten());
        self.send("razer", stream_packet(&body));
    }
}

/// One packet of the frame stream: the bytes and their check byte, as the
/// light takes them (base64 in a `razer` command).
fn stream_packet(body: &[u8]) -> Value {
    const LETTERS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = body.to_vec();
    bytes.push(body.iter().fold(0, |check, byte| check ^ byte));
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (at, byte)| n | u32::from(*byte) << (16 - 8 * at));
        for at in 0..4 {
            text.push(if at <= chunk.len() { LETTERS[(n >> (18 - 6 * at)) as usize & 63] as char } else { '=' });
        }
    }
    json!({ "pt": text })
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Mutex,
        thread,
    };

    use super::*;

    /// A lamp that is a socket of this test: it answers a find and the
    /// question for its state to the port they are answered to, and keeps
    /// what it is told.
    fn mock_lamp(answers: u16) -> (SocketAddr, Arc<Mutex<Vec<Value>>>) {
        let socket = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let address = socket.local_addr().unwrap();
        let told = Arc::new(Mutex::new(Vec::new()));
        let kept = told.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 2048];
            while let Ok((n, from)) = socket.recv_from(&mut buffer) {
                let message: Value = serde_json::from_slice(&buffer[..n]).unwrap();
                let answer = match message["msg"]["cmd"].as_str() {
                    Some("devStatus") => Some(json!({ "onOff": 1, "brightness": 80, "color": { "r": 255, "g": 200, "b": 100 }, "colorTemInKelvin": 0 })),
                    Some("scan") => Some(json!({ "ip": "6.6.6.6", "device": "AA:BB:CC", "sku": "H6076" })),
                    _ => None,
                };
                if let Some(data) = answer {
                    // An answer to another question first, then the answer
                    // twice, as a lamp on two networks would.
                    let other = json!({ "msg": { "cmd": "other", "data": { "device": "ZZ", "brightness": 1 } } });
                    let answer = json!({ "msg": { "cmd": message["msg"]["cmd"], "data": data } });
                    for answer in [&other, &answer, &answer] {
                        let _ = socket.send_to(answer.to_string().as_bytes(), (from.ip(), answers));
                    }
                }
                kept.lock().unwrap().push(message["msg"].clone());
            }
        });
        (address, told)
    }

    /// A free port for the answers of this test (the real one is 4002).
    fn free_port() -> u16 {
        UdpSocket::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
    }

    /// A find lists a lamp once, also when it answers twice, with the
    /// address its answer came from (not the one the answer names); an
    /// answer to another question is no lamp and no state; the lamp says
    /// what it shows; each command has the form the lamp takes.
    #[test]
    fn a_lamp_is_found_asked_and_told() {
        let answers = free_port();
        let (address, told) = mock_lamp(answers);
        let govee = Govee { group: address.to_string(), answers };
        let found = govee.find(Duration::from_millis(400));
        assert_eq!(found, [Found { driver: "govee".into(), id: "AA:BB:CC".into(), model: "H6076".into(), address: "127.0.0.1".into() }]);
        let lamp = govee.open(&Found { address: address.to_string(), ..found[0].clone() }).unwrap();
        assert_eq!(lamp.state(), Some(LampState { on: true, level: 80., colour: Some([255, 200, 100]), kelvin: None }));
        lamp.power(false);
        lamp.level(250);
        lamp.colour([1, 2, 3]);
        lamp.white(2700);
        lamp.stream(true);
        lamp.frame(&[[255, 0, 0], [0, 0, 255]]);
        lamp.frame(&[[7; 3]; 20]);
        lamp.stream(false);
        thread::sleep(Duration::from_millis(200));
        let mut told: Vec<Value> = told.lock().unwrap().iter().filter(|m| m["cmd"] != "scan" && m["cmd"] != "devStatus").cloned().collect();
        assert_eq!(told.remove(0), json!({ "cmd": "turn", "data": { "value": 0 } }));
        assert_eq!(told[0], json!({ "cmd": "brightness", "data": { "value": 100 } }));
        assert_eq!(told[1]["data"], json!({ "color": { "r": 1, "g": 2, "b": 3 }, "colorTemInKelvin": 0 }));
        assert_eq!(told[2]["data"], json!({ "color": { "r": 0, "g": 0, "b": 0 }, "colorTemInKelvin": 2700 }));
        assert_eq!((&told[3]["data"]["pt"], &told[6]["data"]["pt"]), (&json!("uwABsQEK"), &json!("uwABsQAL")));
        // A frame of two colours: BB 00 08 B0 00 02, the colours, the check byte.
        assert_eq!(told[4]["data"]["pt"], json!("uwAIsAAC/wAAAAD/AQ=="));
        // More colours than a frame takes are cut to 14: 6 + 42 + 1 bytes.
        assert_eq!(told[5]["data"]["pt"].as_str().unwrap().len(), (6 + 3 * 14 + 1 + 2) / 3 * 4);
        // An address with no port is the port of the commands.
        assert!(govee.open(&Found { address: "10.0.0.9".into(), ..found[0].clone() }).is_some());
        assert!(govee.open(&Found { address: "lamp.local".into(), ..found[0].clone() }).is_none());
    }

    /// A lamp that misses the first questions of a find is found all the
    /// same: the find asks again while it waits.
    #[test]
    fn a_lamp_that_misses_the_first_question_is_found() {
        let answers = free_port();
        let lamp = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let address = lamp.local_addr().unwrap();
        thread::spawn(move || {
            let mut buffer = [0u8; 2048];
            let mut asked = 0;
            while let Ok((_, from)) = lamp.recv_from(&mut buffer) {
                asked += 1;
                if asked >= 3 {
                    let answer = json!({ "msg": { "cmd": "scan", "data": { "device": "LATE", "sku": "H6076" } } });
                    let _ = lamp.send_to(answer.to_string().as_bytes(), (from.ip(), answers));
                }
            }
        });
        let govee = Govee { group: address.to_string(), answers };
        assert!(govee.find(Duration::from_millis(300)).is_empty(), "one question is not enough for this lamp");
        let found = govee.find(Duration::from_millis(1500));
        assert_eq!(found.iter().map(|lamp| lamp.id.as_str()).collect::<Vec<_>>(), ["LATE"]);
    }

    /// A lamp that does not answer has no state, after a second.
    #[test]
    fn a_lamp_that_does_not_answer_has_no_state() {
        let silent = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let lamp = GoveeLamp { target: silent.local_addr().unwrap(), answers: free_port() };
        let started = Instant::now();
        assert_eq!(lamp.state(), None);
        assert!(started.elapsed() >= Duration::from_millis(900) && started.elapsed() < Duration::from_secs(3));
    }

    /// Two lamps asked at the same moment both say what they show: they
    /// answer to one port, and take their turns at it. And a question
    /// waits for another program that has the port for a moment.
    #[test]
    fn two_lamps_asked_at_once_both_answer() {
        let answers = free_port();
        let lamps: Vec<GoveeLamp> = (0..2).map(|_| GoveeLamp { target: mock_lamp(answers).0, answers }).collect();
        let states: Vec<Option<LampState>> =
            thread::scope(|scope| lamps.iter().map(|lamp| scope.spawn(|| lamp.state())).collect::<Vec<_>>().into_iter().map(|asked| asked.join().unwrap()).collect());
        assert!(states.iter().all(Option::is_some), "a lamp lost its turn at the port: {states:?}");
        // Another program holds the port for half a second.
        let held = UdpSocket::bind(("0.0.0.0", answers)).unwrap();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            drop(held);
        });
        assert!(lamps[0].state().is_some(), "the question did not wait for the port");
    }
}
