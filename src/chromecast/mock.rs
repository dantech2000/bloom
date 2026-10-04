// SPDX-License-Identifier: AGPL-3.0-or-later
//! A cast device that plays a script, for the tests: a TLS server on the
//! loopback with the framing of the real ones. It records every message,
//! answers the way a device does, and does what the test tells it to:
//! refuse an app, hold a seek, go silent, drop the socket.

use std::{
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use rustls::{
    ServerConnection, StreamOwned,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use serde_json::{Value, json};

use super::{
    jellyfin,
    messages::{NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER},
    mdns::Device,
    proto::{CastMessage, Inbox},
};

/// A certificate the device signed itself, like the real ones.
const CERT: &[u8] = include_bytes!("testdata/mock-cert.der");
const KEY: &[u8] = include_bytes!("testdata/mock-key.der");

#[derive(Clone, Debug, Default)]
pub struct MockMedia {
    pub state: String,
    pub position: f64,
    pub content_id: String,
    pub content_type: String,
    pub active_tracks: Vec<i64>,
    pub idle_reason: Option<String>,
}

#[derive(Debug)]
pub struct MockState {
    /// The apps the device has.
    pub available: Vec<String>,
    /// Apps whose launch fails.
    pub fails: Vec<String>,
    /// App id, session id and transport id of the app that runs.
    pub app: Option<(String, String, String)>,
    pub volume: f64,
    pub muted: bool,
    pub media: Option<MockMedia>,
    /// The Jellyfin app: where it is and whether it is paused.
    pub jellyfin_position: f64,
    pub jellyfin_paused: bool,
    /// How long a SEEK takes before its answer.
    pub seek_delay: Duration,
    /// Answers nothing, not even a PING.
    pub silent: bool,
    /// Closes the socket at the next look.
    pub drop_now: bool,
    pub connections: usize,
    pub launches: usize,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            available: vec![jellyfin::APP_STABLE.into(), super::messages::APP_DEFAULT_MEDIA.into()],
            fails: Vec::new(),
            app: None,
            volume: 0.5,
            muted: false,
            media: None,
            jellyfin_position: 0.,
            jellyfin_paused: false,
            seek_delay: Duration::ZERO,
            silent: false,
            drop_now: false,
            connections: 0,
            launches: 0,
        }
    }
}

pub struct Mock {
    pub port: u16,
    pub seen: Arc<Mutex<Vec<CastMessage>>>,
    pub state: Arc<Mutex<MockState>>,
}

impl Mock {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(MockState::default()));
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(CERT.to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec())),
            )
            .unwrap();
        let config = Arc::new(config);
        let (shared_seen, shared_state) = (seen.clone(), state.clone());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (seen, state, config) = (shared_seen.clone(), shared_state.clone(), config.clone());
                thread::spawn(move || serve(stream, config, seen, state));
            }
        });
        Self { port, seen, state }
    }

    pub fn device(&self) -> Device {
        Device {
            id: "mock-1".into(),
            name: "Mock TV".into(),
            model: "Mock".into(),
            address: Ipv4Addr::LOCALHOST,
            port: self.port,
            instance: "Mock-TV".into(),
        }
    }

    /// Namespace and type (or Jellyfin command) of every message, in order.
    pub fn seen(&self) -> Vec<(String, String)> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|message| {
                let payload: Value = serde_json::from_str(message.text_payload()).unwrap_or(Value::Null);
                let kind = payload["type"].as_str().or(payload["command"].as_str()).unwrap_or("").to_string();
                (message.namespace.clone(), kind)
            })
            .collect()
    }

    /// The payloads of one namespace, with their destinations.
    pub fn payloads(&self, namespace: &str) -> Vec<(String, Value)> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|message| message.namespace == namespace)
            .map(|message| (message.destination.clone(), serde_json::from_str(message.text_payload()).unwrap_or(Value::Null)))
            .collect()
    }

    pub fn set(&self, change: impl FnOnce(&mut MockState)) {
        change(&mut self.state.lock().unwrap());
    }
}

fn serve(stream: TcpStream, config: Arc<rustls::ServerConfig>, seen: Arc<Mutex<Vec<CastMessage>>>, state: Arc<Mutex<MockState>>) {
    let Ok(connection) = ServerConnection::new(config) else { return };
    let mut stream = StreamOwned::new(connection, stream);
    while stream.conn.is_handshaking() {
        if stream.conn.complete_io(&mut stream.sock).is_err() {
            return;
        }
    }
    state.lock().unwrap().connections += 1;
    stream.sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    let mut inbox = Inbox::default();
    loop {
        if std::mem::take(&mut state.lock().unwrap().drop_now) {
            return;
        }
        match inbox.fill(&mut stream) {
            Ok(true) => {}
            Ok(false) => return,
            Err(err) if matches!(err.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(_) => return,
        }
        while let Ok(Some(message)) = inbox.next() {
            seen.lock().unwrap().push(message.clone());
            if state.lock().unwrap().silent {
                continue;
            }
            for (namespace, payload) in answer(&message, &state) {
                let reply = CastMessage::text(&message.destination, &message.source, &namespace, payload.to_string());
                use std::io::Write as _;
                if stream.write_all(&reply.frame()).is_err() {
                    return;
                }
            }
        }
    }
}

const IDLE_APP: &str = "E8C28D3C";

fn receiver_status(id: u64, state: &MockState) -> Value {
    let app = match &state.app {
        Some((app_id, session_id, transport_id)) => json!({
            "appId": app_id, "displayName": format!("App {app_id}"), "sessionId": session_id,
            "transportId": transport_id, "statusText": "Ready", "isIdleScreen": false,
            "namespaces": [{"name": NS_MEDIA}, {"name": jellyfin::NAMESPACE}],
        }),
        None => json!({
            "appId": IDLE_APP, "displayName": "Backdrop", "sessionId": "idle", "transportId": "idle",
            "statusText": "", "isIdleScreen": true, "namespaces": [],
        }),
    };
    json!({
        "type": "RECEIVER_STATUS", "requestId": id,
        "status": {
            "applications": [app],
            "volume": {"controlType": "attenuation", "level": state.volume, "muted": state.muted, "stepInterval": 0.05},
        },
    })
}

fn media_status(id: u64, state: &MockState) -> Value {
    let entries = match &state.media {
        Some(media) => vec![json!({
            "mediaSessionId": 1, "playbackRate": 1, "playerState": media.state, "currentTime": media.position,
            "supportedMediaCommands": 12303, "volume": {"level": 1, "muted": false},
            "media": {"contentId": media.content_id, "contentType": media.content_type, "duration": 600.0},
            "activeTrackIds": media.active_tracks, "idleReason": media.idle_reason,
        })],
        None => Vec::new(),
    };
    json!({ "type": "MEDIA_STATUS", "requestId": id, "status": entries })
}

fn jellyfin_report(kind: &str, state: &MockState) -> Value {
    json!({
        "type": kind,
        "data": {
            "ItemId": "i1",
            "PlayState": {
                "PositionTicks": (state.jellyfin_position * 10_000_000.) as i64,
                "IsPaused": state.jellyfin_paused, "VolumeLevel": 100, "IsMuted": false, "CanSeek": true,
            },
            "NowPlayingItem": {"Id": "i1", "RunTimeTicks": 6_000_000_000_i64, "Name": "Film"},
        },
    })
}

/// What the device answers, as (namespace, payload) pairs.
fn answer(message: &CastMessage, state: &Arc<Mutex<MockState>>) -> Vec<(String, Value)> {
    let payload: Value = serde_json::from_str(message.text_payload()).unwrap_or(Value::Null);
    let id = payload["requestId"].as_u64().unwrap_or(0);
    let kind = payload["type"].as_str().unwrap_or("");
    let mut out = Vec::new();
    match message.namespace.as_str() {
        NS_HEARTBEAT if kind == "PING" => out.push((NS_HEARTBEAT.into(), json!({"type": "PONG"}))),
        NS_CONNECTION => {}
        NS_RECEIVER => {
            let mut state = state.lock().unwrap();
            match kind {
                "GET_STATUS" => out.push((NS_RECEIVER.into(), receiver_status(id, &state))),
                "GET_APP_AVAILABILITY" => {
                    let map: serde_json::Map<String, Value> = payload["appId"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str())
                        .map(|app| {
                            let have = state.available.iter().any(|a| a == app);
                            (app.to_string(), json!(if have { "APP_AVAILABLE" } else { "APP_UNAVAILABLE" }))
                        })
                        .collect();
                    out.push((NS_RECEIVER.into(), json!({"responseType": "GET_APP_AVAILABILITY", "requestId": id, "availability": map})));
                }
                "LAUNCH" => {
                    state.launches += 1;
                    let app = payload["appId"].as_str().unwrap_or("").to_string();
                    if state.available.contains(&app) && !state.fails.contains(&app) {
                        let n = state.launches;
                        state.app = Some((app, format!("session-{n}"), format!("transport-{n}")));
                        state.media = None;
                        out.push((NS_RECEIVER.into(), receiver_status(id, &state)));
                        out.push((NS_RECEIVER.into(), receiver_status(0, &state)));
                    } else {
                        out.push((NS_RECEIVER.into(), json!({"type": "LAUNCH_ERROR", "requestId": id, "reason": "NOT_FOUND"})));
                    }
                }
                "STOP" => {
                    state.app = None;
                    state.media = None;
                    out.push((NS_RECEIVER.into(), receiver_status(id, &state)));
                }
                "SET_VOLUME" => {
                    if let Some(level) = payload["volume"]["level"].as_f64() {
                        state.volume = level;
                    }
                    if let Some(muted) = payload["volume"]["muted"].as_bool() {
                        state.muted = muted;
                    }
                    out.push((NS_RECEIVER.into(), receiver_status(id, &state)));
                }
                _ => out.push((NS_RECEIVER.into(), json!({"type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_COMMAND"}))),
            }
        }
        NS_MEDIA => {
            let transport = state.lock().unwrap().app.as_ref().map(|(_, _, t)| t.clone());
            if transport.as_deref() != Some(&message.destination) {
                return vec![(NS_MEDIA.into(), json!({"type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_MEDIA_SESSION_ID"}))];
            }
            let delay = if kind == "SEEK" { state.lock().unwrap().seek_delay } else { Duration::ZERO };
            thread::sleep(delay);
            let mut state = state.lock().unwrap();
            match kind {
                "GET_STATUS" => {}
                "LOAD" => {
                    state.media = Some(MockMedia {
                        state: "PLAYING".into(),
                        position: payload["currentTime"].as_f64().unwrap_or(0.),
                        content_id: payload["media"]["contentId"].as_str().unwrap_or("").into(),
                        content_type: payload["media"]["contentType"].as_str().unwrap_or("").into(),
                        active_tracks: Vec::new(),
                        idle_reason: None,
                    });
                }
                "PAUSE" => {
                    if let Some(media) = &mut state.media {
                        media.state = "PAUSED".into();
                    }
                }
                "PLAY" => {
                    if let Some(media) = &mut state.media {
                        media.state = "PLAYING".into();
                    }
                }
                "SEEK" => {
                    if let Some(media) = &mut state.media {
                        media.position = payload["currentTime"].as_f64().unwrap_or(0.);
                    }
                }
                "STOP" => {
                    if let Some(media) = &mut state.media {
                        media.state = "IDLE".into();
                        media.idle_reason = Some("CANCELLED".into());
                    }
                }
                "EDIT_TRACKS_INFO" => {
                    if let Some(media) = &mut state.media {
                        media.active_tracks = payload["activeTrackIds"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v.as_i64())
                            .collect();
                    }
                }
                _ => return vec![(NS_MEDIA.into(), json!({"type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_COMMAND"}))],
            }
            out.push((NS_MEDIA.into(), media_status(id, &state)));
        }
        jellyfin::NAMESPACE => {
            let mut state = state.lock().unwrap();
            let command = payload["command"].as_str().unwrap_or("");
            let report = match command {
                "PlayNow" => {
                    state.jellyfin_position = payload["options"]["startPositionTicks"].as_f64().unwrap_or(0.) / 10_000_000.;
                    state.jellyfin_paused = false;
                    "playbackstart"
                }
                "Pause" => {
                    state.jellyfin_paused = true;
                    "playbackprogress"
                }
                "Unpause" => {
                    state.jellyfin_paused = false;
                    "playbackprogress"
                }
                "Seek" => {
                    state.jellyfin_position = payload["options"]["position"].as_f64().unwrap_or(0.);
                    "playbackprogress"
                }
                "Stop" => "playbackstop",
                _ => return out,
            };
            out.push((jellyfin::NAMESPACE.into(), jellyfin_report(report, &state)));
        }
        _ => {}
    }
    out
}
