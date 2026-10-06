// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs the rules of [`Core`] against the real player and the real server.
//!
//! It has no part of the user interface in it: a test drives it with the
//! player and a server that plays a script. The app gives it the messages
//! of the socket, the events of the player and the actions of the user, and
//! gets back what to show.

use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use serde::Deserialize;

use super::{
    clock::{Exchange, LocalClock, ServerClock},
    core::{Action, Core, Input, Intent, Notice, PlayerEvent as CoreEvent, PlayerView},
    protocol::{self, Request},
};
use crate::{
    jellyfin::Client,
    player::{Player, PlayerEvent, Sample, Scheduled, ScheduledAction},
    realtime::SocketEvent,
};

/// Tokens of loads start here; the tokens of seeks count up from 1.
const LOAD_TOKENS: u64 = 1 << 40;

/// What the app must show or do after an input.
#[derive(Clone, Debug, PartialEq)]
pub enum Ui {
    Notice(Notice),
    /// The player loads this item for the group.
    NowPlaying { item_id: String },
    /// The player stopped for the group; close its view.
    ClosePlayer,
    /// A socket message that is not SyncPlay's.
    Server { kind: String, data: serde_json::Value },
}

pub struct Session {
    pub core: Core,
    pub clock: ServerClock,
    local: Arc<RwLock<LocalClock>>,
    player: Player,
    client: Client,
    requests: mpsc::Sender<Request>,
    /// Playlist item of each load token.
    loads: HashMap<u64, String>,
    next_load: u64,
    sample: Option<Sample>,
    /// The token of the seek the player works on; its `Settled` voids the
    /// sample (see `player_event`), the `Settled` of an older seek does not.
    seek_token: Option<u64>,
    /// Time between two questions for the server time, for the clock thread.
    poll_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    /// The socket is open.
    pub connected: bool,
    /// How late the last scheduled action ran.
    pub late: Option<Duration>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Session {
    /// Starts the helper threads. The exchanges of the clock come on the
    /// returned channel; give each to [`Session::exchange`].
    pub fn new(client: Client, player: Player) -> (Self, async_channel::Receiver<Exchange>) {
        let local = Arc::new(RwLock::new(LocalClock::new()));
        let poll_ms = Arc::new(AtomicU64::new(1000));
        let stop = Arc::new(AtomicBool::new(false));
        let exchanges = spawn_clock(client.clone(), local.clone(), poll_ms.clone(), stop.clone());
        let mut clock = ServerClock::default();
        clock.reset_for(local.read().unwrap().epoch);
        let session = Self {
            core: Core::default(),
            clock,
            local,
            player,
            requests: spawn_requests(client.clone()),
            client,
            loads: HashMap::new(),
            next_load: LOAD_TOKENS,
            sample: None,
            seek_token: None,
            poll_ms,
            stop,
            connected: false,
            late: None,
        };
        (session, exchanges)
    }

    fn now(&self) -> f64 {
        self.local.read().unwrap().now()
    }

    /// The player as the core sees it: the last measurement of the audio
    /// position when one is fresh, else the status.
    fn view(&self) -> PlayerView {
        let local = self.local.read().unwrap();
        let status = self.player.status();
        match self.sample.filter(|s| s.at.elapsed() < Duration::from_millis(400)) {
            Some(sample) => PlayerView {
                position: sample.position * 1000.,
                at: local.at(sample.at),
                paused: status.paused,
                duration: status.duration * 1000.,
            },
            None => PlayerView {
                position: status.position * 1000.,
                at: status.position_at.map_or_else(|| local.now(), |at| local.at(at)),
                paused: status.paused,
                duration: status.duration * 1000.,
            },
        }
    }

    fn feed(&mut self, input: Input) -> Vec<Ui> {
        let (now, view) = (self.now(), self.view());
        let actions = self.core.handle(input, now, &self.clock, &view);
        let mut ui = Vec::new();
        for action in actions {
            self.run(action, &mut ui);
        }
        // Measurements are only needed while the player follows a group.
        self.player.follow(self.core.following());
        ui
    }

    fn run(&mut self, action: Action, ui: &mut Vec<Ui>) {
        log::debug!("syncplay: {action:?}");
        match action {
            Action::Send(request) => {
                let _ = self.requests.send(request);
            }
            Action::Load { item_id, playlist_item_id, position } => {
                self.next_load += 1;
                self.loads.insert(self.next_load, playlist_item_id);
                self.sample = None;
                // The server says how the item plays (see `stream`).
                crate::stream::start(
                    &self.player,
                    self.client.clone(),
                    crate::stream::Load {
                        item_id: item_id.clone(),
                        title: String::new(),
                        start_secs: (position / 1000.).max(0.),
                        paused: true,
                        token: self.next_load,
                        audio: None,
                        subtitle: None,
                    },
                );
                ui.push(Ui::NowPlaying { item_id });
            }
            Action::SeekPaused { position, token } => {
                self.sample = None;
                self.seek_token = Some(token);
                self.player.seek_exact((position / 1000.).max(0.), true, token);
            }
            Action::UnpauseAt(at) => self.player.schedule(Some(Scheduled {
                at: self.local.read().unwrap().instant(at),
                action: ScheduledAction::Unpause,
            })),
            Action::PauseAt { at, position } => self.player.schedule(Some(Scheduled {
                at: self.local.read().unwrap().instant(at),
                action: ScheduledAction::PauseThenSeek((position / 1000.).max(0.)),
            })),
            Action::PauseNow => self.player.set_paused(true),
            Action::CancelScheduled => self.player.schedule(None),
            Action::SetSpeed(factor) => self.player.set_sync_speed(factor),
            Action::Stop => {
                self.player.stop();
                ui.push(Ui::ClosePlayer);
            }
            Action::Notice(notice) => {
                // The video-sync mode of the player differs in a group.
                match &notice {
                    Notice::Joined(_) => self.player.set_group(true),
                    Notice::Left => self.player.set_group(false),
                    _ => {}
                }
                ui.push(Ui::Notice(notice))
            }
        }
    }

    pub fn socket(&mut self, event: SocketEvent) -> Vec<Ui> {
        match event {
            SocketEvent::Open { again } => {
                self.connected = true;
                if again { self.feed(Input::Reconnected) } else { Vec::new() }
            }
            SocketEvent::Closed => {
                self.connected = false;
                Vec::new()
            }
            SocketEvent::Message { kind, data } => match protocol::parse(&kind, data.clone()) {
                Some(message) => {
                    log::debug!("syncplay: server {message:?}");
                    self.feed(Input::Server(message))
                }
                None => vec![Ui::Server { kind, data }],
            },
        }
    }

    pub fn player_event(&mut self, event: PlayerEvent) -> Vec<Ui> {
        let event = match event {
            PlayerEvent::Loaded { token } => match self.loads.remove(&token) {
                Some(playlist_item_id) => CoreEvent::Loaded { playlist_item_id },
                None => return Vec::new(),
            },
            PlayerEvent::LoadFailed { token } => match self.loads.remove(&token) {
                Some(playlist_item_id) => CoreEvent::LoadFailed { playlist_item_id },
                None => return Vec::new(),
            },
            PlayerEvent::Settled { token } => {
                // The player set its position as the seek landed; a sample
                // from during the seek must not stand in for it. The
                // `Settled` of an older seek says nothing about the sample.
                if self.seek_token == Some(token) {
                    self.seek_token = None;
                    self.sample = None;
                }
                CoreEvent::Settled { token }
            }
            PlayerEvent::Stalled => CoreEvent::Stalled,
            PlayerEvent::Recovered => CoreEvent::Recovered,
            PlayerEvent::Position(sample) => {
                self.sample = Some(sample);
                CoreEvent::Position
            }
            PlayerEvent::Ended => CoreEvent::Ended,
            PlayerEvent::ScheduledFired { late } => {
                self.late = Some(late);
                return Vec::new();
            }
        };
        self.feed(Input::Player(event))
    }

    /// The user closed the player.
    pub fn player_closed(&mut self) -> Vec<Ui> {
        self.feed(Input::Player(CoreEvent::Closed))
    }

    pub fn exchange(&mut self, exchange: Exchange) -> Vec<Ui> {
        self.clock.record(exchange);
        let wait = self.clock.next_poll(self.core.in_group());
        self.poll_ms.store(wait.as_millis() as u64, Ordering::Release);
        self.feed(Input::ClockUpdated)
    }

    pub fn user(&mut self, intent: Intent) -> Vec<Ui> {
        self.feed(Input::User(intent))
    }

    /// Call a few times a second.
    pub fn tick(&mut self) -> Vec<Ui> {
        // After a sleep of the machine the local clock is wrong by the time
        // of the sleep, and with it every measurement.
        if self.local.read().unwrap().slept() {
            log::info!("syncplay: the machine slept; the clock starts again");
            let fresh = LocalClock::new();
            *self.local.write().unwrap() = fresh;
            // A measurement of the clock before, still on its way, is void.
            self.clock.reset_for(fresh.epoch);
            self.poll_ms.store(1000, Ordering::Release);
        }
        self.feed(Input::Tick)
    }

    /// One line about the sync, for the debug channel and the screen.
    pub fn describe(&self) -> String {
        let status = self.player.status();
        let group = match self.core.group() {
            Some(group) => format!(
                "group={:?} state={:?} members={}",
                group.group_name,
                group.state,
                group.participants.join(",")
            ),
            None => "group=none".to_string(),
        };
        // The position with the time it was measured at, on the clock of
        // the server: two players compare these to find how far apart they
        // are (see dev/syncplay-skew).
        let sample = self.sample.map_or("-".to_string(), |sample| {
            let at = self.clock.to_server(self.local.read().unwrap().at(sample.at));
            format!("{:.4}@{at:.1}", sample.position)
        });
        format!(
            "{group} following={} phase={} socket={} offset={:+.1}ms ping={:.0}ms drift={} speed={:.3} late={} pos={:.3} paused={} sample={sample}",
            self.core.following(),
            self.core.phase_name(),
            if self.connected { "open" } else { "closed" },
            self.clock.offset(),
            self.clock.ping(),
            self.core.drift_ms.map_or("-".to_string(), |d| format!("{d:+.1}ms")),
            self.core.speed(),
            self.late.map_or("-".to_string(), |l| format!("{:.1}ms", l.as_secs_f64() * 1000.)),
            status.position,
            status.paused,
        )
    }
}

/// Sends the requests one after another, in the order they were made; on
/// the shared pool two of them could pass each other. A request that a
/// later one makes pointless is left out.
fn spawn_requests(client: Client) -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel::<Request>();
    let _ = thread::Builder::new().name("syncplay-requests".into()).spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut batch = vec![first];
            while let Ok(more) = rx.try_recv() {
                batch.push(more);
            }
            for (n, request) in batch.iter().enumerate() {
                if batch[n + 1..].iter().any(|later| later.replaces(request)) {
                    continue;
                }
                let result = match request.body() {
                    Some(body) => client.post(request.path(), &body).map(drop),
                    None => client.call("POST", request.path(), &[]),
                };
                if let Err(err) = result {
                    log::warn!("syncplay: {} failed: {err:#}", request.path());
                }
            }
        }
    });
    tx
}

/// Asks the server for its time, again and again.
fn spawn_clock(
    client: Client,
    local: Arc<RwLock<LocalClock>>,
    poll_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) -> async_channel::Receiver<Exchange> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct UtcTime {
        request_reception_time: String,
        response_transmission_time: String,
    }
    let (tx, rx) = async_channel::unbounded();
    let _ = thread::Builder::new().name("syncplay-clock".into()).spawn(move || {
        while !stop.load(Ordering::Acquire) {
            // Both local times on one clock: a reset during the request
            // must not mix two origins.
            let clock = *local.read().unwrap();
            let sent = clock.now();
            let answer = client.get::<UtcTime>("/GetUtcTime", &[]);
            let received = clock.now();
            match answer {
                Ok(time) => {
                    let times = protocol::parse_time(&time.request_reception_time)
                        .zip(protocol::parse_time(&time.response_transmission_time));
                    if let Some((server_received, server_sent)) = times {
                        let exchange = Exchange { sent, server_received, server_sent, received, epoch: clock.epoch };
                        if tx.send_blocking(exchange).is_err() {
                            return;
                        }
                    }
                }
                Err(err) => log::debug!("syncplay: server time: {err:#}"),
            }
            // In small steps, so a shorter wait takes effect soon.
            let mut waited = 0;
            while waited < poll_ms.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(100));
                waited += 100;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    //! The session against a server that plays a script: a local HTTP and
    //! WebSocket server that serves a clip, answers the time request with a
    //! clock 37 seconds ahead, records every request, and sends what the
    //! test tells it to. The player is the real one.

    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::Mutex,
        time::Instant,
    };

    use serde_json::{Value, json};

    use super::*;
    use crate::{realtime, syncplay::protocol::format_time};

    const AHEAD_MS: f64 = 37_000.;
    const GROUP: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f9";

    #[derive(Default)]
    struct Seen {
        /// Path, body and Authorization header of each request, in order.
        requests: Vec<(String, Value, String)>,
        /// Authorization header of each socket, and how many requests the
        /// server had seen when it opened.
        sockets: Vec<(String, usize)>,
        push: Option<mpsc::Sender<String>>,
        drop_socket: bool,
    }

    struct Mock {
        port: u16,
        seen: Arc<Mutex<Seen>>,
    }

    fn wall_ms() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            * 1000.
    }

    impl Mock {
        fn start(clip: std::path::PathBuf) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = Arc::new(Mutex::new(Seen::default()));
            let shared = seen.clone();
            let clip = Arc::new(std::fs::read(clip).unwrap());
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (seen, clip) = (shared.clone(), clip.clone());
                    thread::spawn(move || serve(stream, seen, clip));
                }
            });
            Self { port, seen }
        }

        /// The time the clients must take for the server time.
        fn now(&self) -> f64 {
            wall_ms() + AHEAD_MS
        }

        fn push(&self, kind: &str, data: Value) {
            let message = json!({ "MessageType": kind, "MessageId": "m", "Data": data });
            let seen = self.seen.lock().unwrap();
            seen.push.as_ref().expect("no socket").send(message.to_string()).unwrap();
        }

        fn group(&self, kind: &str, data: Value) {
            self.push("SyncPlayGroupUpdate", json!({ "GroupId": GROUP, "Type": kind, "Data": data }));
        }

        fn command(&self, kind: &str, position_secs: f64, in_ms: f64) {
            self.push(
                "SyncPlayCommand",
                json!({
                    "GroupId": GROUP, "PlaylistItemId": "p1",
                    "When": format_time(self.now() + in_ms),
                    "PositionTicks": (position_secs * 10_000_000.) as i64,
                    "Command": kind, "EmittedAt": format_time(self.now()),
                }),
            );
        }

        fn requests(&self, path: &str) -> Vec<Value> {
            let seen = self.seen.lock().unwrap();
            seen.requests.iter().filter(|r| r.0 == path).map(|r| r.1.clone()).collect()
        }
    }

    fn serve(mut stream: TcpStream, seen: Arc<Mutex<Seen>>, clip: Arc<Vec<u8>>) {
        let mut peeked = [0u8; 2048];
        let n = stream.peek(&mut peeked).unwrap_or(0);
        let head = String::from_utf8_lossy(&peeked[..n]).to_lowercase();
        if head.contains("upgrade: websocket") {
            return serve_socket(stream, seen);
        }
        let mut data = Vec::new();
        let mut buffer = [0u8; 8192];
        let (head, mut body) = loop {
            let Ok(n) = stream.read(&mut buffer) else { return };
            if n == 0 {
                return;
            }
            data.extend_from_slice(&buffer[..n]);
            if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break (String::from_utf8_lossy(&data[..end]).to_string(), data[end + 4..].to_vec());
            }
        };
        let header = |name: &str| {
            head.lines()
                .find_map(|line| line.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(name)))
                .map(|(_, v)| v.trim().to_string())
        };
        let length: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        while body.len() < length {
            let Ok(n) = stream.read(&mut buffer) else { return };
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buffer[..n]);
        }
        let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        let path = target.split('?').next().unwrap_or("/").to_string();
        let respond = |stream: &mut TcpStream, status: &str, kind: &str, extra: &str, body: &[u8]| {
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(body);
        };
        if path.starts_with("/Videos/") {
            // The clip, with ranges, so the player can seek.
            let range = header("range").and_then(|v| {
                let (from, to) = v.strip_prefix("bytes=")?.split_once('-')?;
                Some((from.parse::<usize>().ok()?, to.parse::<usize>().ok()))
            });
            return match range {
                Some((from, to)) if from < clip.len() => {
                    let to = to.unwrap_or(clip.len() - 1).min(clip.len() - 1);
                    let extra = format!(
                        "Accept-Ranges: bytes\r\nContent-Range: bytes {from}-{to}/{}\r\n",
                        clip.len()
                    );
                    respond(&mut stream, "206 Partial Content", "video/mp4", &extra, &clip[from..=to])
                }
                _ => respond(&mut stream, "200 OK", "video/mp4", "Accept-Ranges: bytes\r\n", &clip),
            };
        }
        // The player asks how to play an item: the file itself.
        if let Some(id) = path
            .strip_prefix("/Items/")
            .and_then(|rest| rest.strip_suffix("/PlaybackInfo"))
        {
            let body = json!({
                "MediaSources": [{ "Id": id, "SupportsDirectPlay": true }],
                "PlaySessionId": "mock-play-session",
            });
            return respond(&mut stream, "200 OK", "application/json", "", body.to_string().as_bytes());
        }
        if path == "/GetUtcTime" {
            let now = format_time(wall_ms() + AHEAD_MS);
            let body = json!({ "RequestReceptionTime": now, "ResponseTransmissionTime": now });
            return respond(&mut stream, "200 OK", "application/json", "", body.to_string().as_bytes());
        }
        let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
        seen.lock().unwrap().requests.push((path, body, header("authorization").unwrap_or_default()));
        respond(&mut stream, "204 No Content", "text/plain", "", b"");
    }

    fn serve_socket(stream: TcpStream, seen: Arc<Mutex<Seen>>) {
        let mut auth = String::new();
        let callback = |request: &tungstenite::handshake::server::Request,
                        response: tungstenite::handshake::server::Response| {
            auth = request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            Ok(response)
        };
        let Ok(mut socket) = tungstenite::accept_hdr(stream, callback) else { return };
        let (tx, rx) = mpsc::channel::<String>();
        {
            let mut seen = seen.lock().unwrap();
            let before = seen.requests.len();
            seen.sockets.push((auth, before));
            seen.push = Some(tx);
            seen.drop_socket = false;
        }
        socket.get_ref().set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let _ = socket.send(tungstenite::Message::text(
            json!({ "MessageType": "ForceKeepAlive", "MessageId": "k", "Data": 60 }).to_string(),
        ));
        loop {
            if seen.lock().unwrap().drop_socket {
                return;
            }
            while let Ok(text) = rx.try_recv() {
                if socket.send(tungstenite::Message::text(text)).is_err() {
                    return;
                }
            }
            match socket.read() {
                Ok(tungstenite::Message::Text(text)) if text.contains("KeepAlive") => {
                    let _ = socket.send(tungstenite::Message::text(
                        json!({ "MessageType": "KeepAlive", "MessageId": "k" }).to_string(),
                    ));
                }
                Ok(tungstenite::Message::Close(_)) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(_)) => {}
                Err(_) => return,
            }
        }
    }

    /// The session with everything that feeds it.
    struct Rig {
        session: Session,
        socket: async_channel::Receiver<SocketEvent>,
        exchanges: async_channel::Receiver<Exchange>,
        events: async_channel::Receiver<PlayerEvent>,
        player: Player,
        _realtime: realtime::Realtime,
        ui: Vec<Ui>,
        last_tick: Instant,
    }

    impl Rig {
        /// Gives the session what came in, until the test is content or the
        /// time is up.
        fn until(&mut self, what: &str, secs: f64, mut done: impl FnMut(&mut Rig) -> bool) {
            let deadline = Instant::now() + Duration::from_secs_f64(secs);
            loop {
                while let Ok(event) = self.socket.try_recv() {
                    let ui = self.session.socket(event);
                    self.ui.extend(ui);
                }
                while let Ok(exchange) = self.exchanges.try_recv() {
                    let ui = self.session.exchange(exchange);
                    self.ui.extend(ui);
                }
                while let Ok(event) = self.events.try_recv() {
                    let ui = self.session.player_event(event);
                    self.ui.extend(ui);
                }
                if self.last_tick.elapsed() >= Duration::from_millis(250) {
                    self.last_tick = Instant::now();
                    let ui = self.session.tick();
                    self.ui.extend(ui);
                }
                if done(self) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "{what} timed out: {} | {:?}",
                    self.session.describe(),
                    self.player.status()
                );
                thread::sleep(Duration::from_millis(2));
            }
        }

        fn run(&mut self, secs: f64) {
            let end = Instant::now() + Duration::from_secs_f64(secs);
            self.until("run", secs + 1., |_| Instant::now() >= end);
        }
    }

    fn ticks(value: &Value) -> f64 {
        value["PositionTicks"].as_f64().unwrap() / 10_000_000.
    }

    /// Where the group is now, for a start at `position` that ran at `when`.
    fn group_position(position: f64, when_ms: f64, mock: &Mock) -> f64 {
        position + (mock.now() - when_ms) / 1000.
    }

    #[test]
    fn follows_a_scripted_group_with_the_real_player() {
        let Some(_one) = crate::player::real_player_turn() else { return };
        let clip = std::env::temp_dir().join("bloom-syncplay-test-4min.mp4");
        let encoded = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=320x240:r=24:d=240"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=240"])
            .args(["-c:v", "libx264", "-g", "48", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .args(["-movflags", "+faststart"])
            .arg(&clip)
            .status();
        // No ffmpeg is a skip (`dev/test` refuses to start without it); an
        // ffmpeg that runs and fails must not let the test pass unplayed.
        match encoded {
            Ok(status) => assert!(status.success(), "ffmpeg could not make the test clip: {status}"),
            Err(_) => {
                eprintln!("ffmpeg not available; skipping");
                return;
            }
        }
        let mock = Mock::start(clip);
        let client = Client::new(&format!("http://127.0.0.1:{}", mock.port), "device-under-test")
            .with_session("token", "user");
        let player = Player::default();
        let _closer = crate::player::Closer(player.clone());
        player.set_target_size(320, 240);
        let (realtime, socket) = realtime::start(client.clone());
        let (session, exchanges) = Session::new(client, player.clone());
        let mut rig = Rig {
            session,
            socket,
            exchanges,
            events: player.events(),
            player: player.clone(),
            _realtime: realtime,
            ui: Vec::new(),
            last_tick: Instant::now(),
        };

        // The socket opens, with the header of the requests, and the clock
        // finds the server 37 s ahead.
        rig.until("socket and clock", 10., |rig| rig.session.connected && rig.session.clock.ready());
        assert!(
            (rig.session.clock.offset() - AHEAD_MS).abs() < 30.,
            "offset {}",
            rig.session.clock.offset()
        );

        // Join: the request goes out with the socket open, under one identity.
        rig.ui.extend(rig.session.user(Intent::Join { group_id: GROUP.into() }));
        rig.until("join request", 5., |_| !mock.requests("/SyncPlay/Join").is_empty());
        {
            let seen = mock.seen.lock().unwrap();
            let join = seen.requests.iter().find(|r| r.0 == "/SyncPlay/Join").unwrap();
            assert_eq!(join.1["GroupId"], GROUP);
            assert_eq!(seen.sockets.len(), 1);
            assert_eq!(seen.sockets[0].1, 0, "the socket must be open before the first request");
            assert!(join.2.contains("DeviceId=\"device-under-test\""), "{}", join.2);
            assert_eq!(seen.sockets[0].0, join.2, "socket and request must be one session");
        }

        // The group is paused at 5 s in one item.
        mock.group(
            "GroupJoined",
            json!({
                "GroupId": GROUP, "GroupName": "Film night", "State": "Paused",
                "Participants": ["ana"], "LastUpdatedAt": format_time(mock.now() - 50.),
            }),
        );
        mock.group(
            "PlayQueue",
            json!({
                "Reason": "NewPlaylist", "LastUpdate": format_time(mock.now()),
                "Playlist": [{ "ItemId": "clip", "PlaylistItemId": "p1" }],
                "PlayingItemIndex": 0, "StartPositionTicks": 50_000_000_i64, "IsPlaying": false,
                "ShuffleMode": "Sorted", "RepeatMode": "RepeatNone",
            }),
        );
        rig.until("ready after the load", 15., |_| !mock.requests("/SyncPlay/Ready").is_empty());
        player.set_muted(true);
        let ready = &mock.requests("/SyncPlay/Ready")[0];
        assert_eq!(ready["PlaylistItemId"], "p1");
        assert_eq!(ready["IsPlaying"], false);
        assert!((ticks(ready) - 5.).abs() < 0.1, "{ready}");
        assert!(rig.ui.contains(&Ui::NowPlaying { item_id: "clip".into() }));
        assert!(mock.requests("/SyncPlay/Buffering").is_empty(), "no buffering during a load");
        assert!(player.status().paused);

        // A start 800 ms ahead runs at its time.
        let when = mock.now() + 800.;
        mock.command("Unpause", 5., 800.);
        rig.until("start", 5., |rig| rig.session.late.is_some() && !rig.player.status().paused);
        let late = rig.session.late.unwrap();
        assert!(late < Duration::from_millis(20), "the start ran {late:?} late");
        // In step with the group a few seconds on.
        rig.run(4.);
        let drift = rig.session.core.drift_ms.expect("a drift measurement");
        eprintln!("drift after a scheduled start: {drift:+.1} ms; start {late:?} late");
        assert!(drift.abs() < 80., "drift {drift}");
        let position = rig.session.sample.unwrap().position;
        let group = group_position(5., when, &mock);
        assert!((position - group).abs() < 0.3, "player {position} group {group}");

        // A pause parks the player at the position of the command.
        mock.command("Pause", 9.5, 0.);
        rig.until("pause", 5., |rig| {
            let status = rig.player.status();
            status.paused && (status.position - 9.5).abs() < 0.1
        });
        // The same command again does nothing.
        let before = mock.seen.lock().unwrap().requests.len();
        mock.command("Pause", 9.5, 0.);
        rig.run(0.5);
        assert!((rig.player.status().position - 9.5).abs() < 0.1);

        // A seek of the group: the player goes there and says it is ready.
        let readies = mock.requests("/SyncPlay/Ready").len();
        mock.command("Seek", 30.25, 0.);
        rig.until("ready after the seek", 10., |_| mock.requests("/SyncPlay/Ready").len() > readies);
        let ready = mock.requests("/SyncPlay/Ready").last().unwrap().clone();
        assert!((ticks(&ready) - 30.25).abs() < 0.1, "{ready}");
        assert_eq!(ready["IsPlaying"], false);
        assert!(mock.seen.lock().unwrap().requests.len() > before);

        // The group plays since 2 s: the player seeks ahead of it and starts
        // at the right moment.
        let when = mock.now() - 2_000.;
        mock.command("Unpause", 30.25, -2_000.);
        rig.until("catch-up", 10., |rig| rig.session.core.phase_name() == "playing");
        // A start after a seek can be some tens of milliseconds off; the
        // correction closes that.
        rig.run(2.5);
        rig.until("in step after the catch-up", 15., |rig| {
            rig.session.core.speed() == 1.
                && rig.session.core.drift_ms.is_some_and(|drift| drift.abs() < 20.)
        });
        let drift = rig.session.core.drift_ms.expect("a drift measurement");
        eprintln!("drift after a catch-up: {drift:+.1} ms");
        assert!(drift.abs() < 25., "drift {drift}");
        let position = rig.session.sample.unwrap().position;
        let group = group_position(30.25, when, &mock);
        assert!((position - group).abs() < 0.3, "player {position} group {group}");

        // The player gets ahead of the group. The correction slows it for a
        // time and it is in step again.
        rig.run(2.5);
        player.set_sync_speed(1.25);
        thread::sleep(Duration::from_millis(1000));
        player.set_sync_speed(1.0);
        let mut worst: f64 = 0.;
        let mut factor: f64 = 1.;
        for _ in 0..30 {
            rig.run(0.3);
            let drift = rig.session.core.drift_ms.unwrap_or(0.);
            let speed = rig.session.core.speed();
            if speed == 1. && drift.abs() > worst.abs() {
                worst = drift;
            }
            if speed != 1. {
                factor = speed;
            }
        }
        // A second, small correction can follow the first; let it end.
        rig.until("corrections end", 30., |rig| {
            rig.session.core.speed() == 1.
                && rig.session.core.drift_ms.is_some_and(|drift| drift.abs() < 20.)
        });
        let drift = rig.session.core.drift_ms.expect("a drift measurement");
        eprintln!("pushed {worst:+.1} ms off, corrected at speed {factor:.3}, then {drift:+.1} ms");
        // The proof that the player was ahead is the slower speed. `worst`
        // is only for the log: the correction can start before this loop
        // sees the drift at normal speed.
        assert!(factor < 1., "no correction ran (worst drift seen {worst})");
        assert!(drift.abs() < 25., "drift after the correction {drift}");

        // The user pauses: the request goes out, and the player pauses at once.
        rig.ui.extend(rig.session.user(Intent::TogglePause));
        rig.until("pause request", 5., |rig| {
            !mock.requests("/SyncPlay/Pause").is_empty() && rig.player.status().paused
        });

        // The socket is lost: it opens again and the group is joined again.
        mock.seen.lock().unwrap().drop_socket = true;
        rig.until("rejoin", 15., |_| mock.requests("/SyncPlay/Join").len() == 2);
        assert_eq!(mock.seen.lock().unwrap().sockets.len(), 2);

        player.stop();
    }

    /// A session with no threads and no server: the requests come back on
    /// the returned channel, and the player is a handle with no worker,
    /// whose status stays at its defaults (position 0).
    fn bare_session() -> (Session, mpsc::Receiver<Request>) {
        let (requests, sent) = mpsc::channel();
        let mut clock = ServerClock::default();
        let local = LocalClock::new();
        let now = local.now();
        clock.record(Exchange {
            sent: now,
            server_received: now + AHEAD_MS + 5.,
            server_sent: now + AHEAD_MS + 5.,
            received: now + 10.,
            epoch: local.epoch,
        });
        let session = Session {
            core: Core::default(),
            clock,
            local: Arc::new(RwLock::new(local)),
            player: Player::default(),
            client: Client::new("http://127.0.0.1:9", "bare").with_session("token", "user"),
            requests,
            loads: HashMap::new(),
            next_load: LOAD_TOKENS,
            sample: None,
            seek_token: None,
            poll_ms: Arc::new(AtomicU64::new(1000)),
            stop: Arc::new(AtomicBool::new(false)),
            connected: true,
            late: None,
        };
        (session, sent)
    }

    /// The `Ready` after a seek while paused carries the position the
    /// player settled at, not a sample of the audio clock from before or
    /// during the seek (the audio clock stands still while paused; seen
    /// in the test above as a `Ready` at the position of the pause). The
    /// `Settled` of an older seek does not throw a sample away.
    #[test]
    fn the_ready_after_a_paused_seek_takes_the_settled_position() {
        let (mut session, sent) = bare_session();
        let server_now = |session: &Session| session.clock.to_server(session.now());
        // Joined, with one item parked at 5 s, loaded. The group messages
        // go to the core alone: the load of the session would start the
        // player.
        let joined = protocol::parse(
            "SyncPlayGroupUpdate",
            json!({
                "GroupId": GROUP, "Type": "GroupJoined",
                "Data": {
                    "GroupId": GROUP, "GroupName": "Film night", "State": "Paused",
                    "Participants": ["ana"], "LastUpdatedAt": format_time(server_now(&session) - 50.),
                },
            }),
        )
        .unwrap();
        let queue = protocol::parse(
            "SyncPlayGroupUpdate",
            json!({
                "GroupId": GROUP, "Type": "PlayQueue",
                "Data": {
                    "Reason": "NewPlaylist", "LastUpdate": format_time(server_now(&session)),
                    "Playlist": [{ "ItemId": "clip", "PlaylistItemId": "p1" }],
                    "PlayingItemIndex": 0, "StartPositionTicks": 50_000_000_i64, "IsPlaying": false,
                    "ShuffleMode": "Sorted", "RepeatMode": "RepeatNone",
                },
            }),
        )
        .unwrap();
        for message in [joined, queue] {
            let (now, view) = (session.now(), session.view());
            session.core.handle(Input::Server(message), now, &session.clock, &view);
        }
        session.loads.insert(LOAD_TOKENS + 1, "p1".into());
        session.player_event(PlayerEvent::Loaded { token: LOAD_TOKENS + 1 });
        let at = Instant::now();
        let sample = |position: f64| PlayerEvent::Position(Sample { position, at, paused: true, duration: 240. });
        // The player paused at 9.5 s says so.
        session.player_event(sample(9.5));
        assert_eq!(session.view().position, 9_500.);

        // The group seeks to 30.25 s: the player is told, and the sample
        // of before the seek is void.
        let command = json!({
            "GroupId": GROUP, "PlaylistItemId": "p1",
            "When": format_time(server_now(&session)), "PositionTicks": 302_500_000_i64,
            "Command": "Seek", "EmittedAt": format_time(server_now(&session)),
        });
        session.socket(SocketEvent::Message { kind: "SyncPlayCommand".into(), data: command });
        let token = session.seek_token.expect("a seek was asked of the player");
        assert_eq!(session.sample, None);
        // A sample of the audio clock during the seek: still at 9.5 s.
        session.player_event(sample(9.5));
        assert_eq!(session.view().position, 9_500.);
        // The seek landed. The player's status has the position now (0
        // for the player of this test); the sample from during the seek
        // must not stand in for it.
        // The requests so far (the Ready of the load among them) are not
        // the subject.
        while sent.try_recv().is_ok() {}
        session.player_event(PlayerEvent::Settled { token });
        let ready = loop {
            match sent.try_recv().expect("a Ready after the seek") {
                Request::Ready(report) => break report,
                _ => continue,
            }
        };
        assert_eq!(ready.position_ticks, 0, "the Ready carries the sample of before the seek: {ready:?}");
        assert_eq!(session.sample, None);

        // A fresh sample after the settle stands; the Settled of a seek
        // that is not the current one does not throw it away.
        session.player_event(sample(30.25));
        session.player_event(PlayerEvent::Settled { token: token - 1 });
        assert_eq!(session.view().position, 30_250., "an obsolete Settled voided the sample");
    }
}
