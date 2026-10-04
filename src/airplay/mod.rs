// SPDX-License-Identifier: AGPL-3.0-or-later
//! AirPlay video to an Apple TV. The app hands the server's HLS stream of
//! an item to an `AVPlayer` that allows external playback; when the user
//! picks a receiver in the system route picker, macOS pairs with it and the
//! receiver fetches the stream itself. The app keeps control (pause, seek,
//! stop, volume), reads the position back, and reports playback to the
//! server as the local player does.
//!
//! One engine thread owns the player and runs the commands, so no call
//! into AVFoundation and no report runs on the UI thread. The status is a
//! snapshot the UI reads at any time; the events are the stream of what
//! happened, in the way of `crate::player`.
//!
//! Nothing here starts the engine or the Bonjour browse until a command
//! asks for it, so the app starts as fast as before.

pub mod av;
pub mod control;
mod hook;
pub mod picker;
pub mod routes;
pub mod stream;

use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicPtr, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use av::{ItemStatus, Player, Pool, RouteDetector, TimeControl};
use control::{Action, Control};
pub use stream::{StreamOptions, redact};

use crate::{
    jellyfin::{Client, Progress, TICKS_PER_SECOND},
    macos::Id,
    player::{Report, ReportKind, Sample, spawn_reporter},
};

/// Time between two looks at the player. The position in the status is
/// this old at most; the slider can run it forward from `position_at`.
const POLL: Duration = Duration::from_millis(250);
/// Time between two progress reports to the server.
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// How long a command waits for the engine's player to exist.
const START_WAIT: Duration = Duration::from_millis(500);
/// Time before the second request that ends the transcode.
const ENCODING_STOP_AGAIN: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AirPlayState {
    /// No item.
    #[default]
    Idle,
    /// The item was sent and is not ready.
    Loading,
    Playing,
    Paused,
    /// Ready, playing, and out of data.
    Buffering,
    /// The item ran to its end.
    Ended,
    /// The item could not be loaded or played; `error` says why.
    Failed,
}

impl AirPlayState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Buffering => "buffering",
            Self::Ended => "ended",
            Self::Failed => "failed",
        }
    }
}

/// A picture of the sender at the last look.
#[derive(Clone, Debug, Default)]
pub struct AirPlayStatus {
    pub state: AirPlayState,
    pub item_id: Option<String>,
    pub title: String,
    /// Seconds into the item, as of `position_at`.
    pub position: f64,
    pub position_at: Option<Instant>,
    /// Seconds; 0 while not known.
    pub duration: f64,
    /// The item plays on a receiver, not on this Mac.
    pub external: bool,
    /// Name of the receiver. macOS gives no public call for it; the panel
    /// may fill it from the route the user picked.
    pub route: Option<String>,
    pub error: Option<String>,
    pub play_session_id: Option<String>,
    pub volume: f32,
    pub muted: bool,
    /// The system sees more than one route (an AirPlay receiver beside this
    /// Mac); none before the first ask for routes.
    pub multiple_routes: Option<bool>,
    /// From the send to the first moment the item ran.
    pub load_latency: Option<Duration>,
    /// From the last pause, play or seek to the moment it showed.
    pub control_latency: Option<(&'static str, Duration)>,
}

/// What the engine tells the app, at the moment it happens.
#[derive(Clone, Debug, PartialEq)]
pub enum AirPlayEvent {
    /// The item of a send is ready; the start position is applied.
    Loaded { token: u64 },
    LoadFailed { token: u64, error: String },
    /// A look at the position, every [`POLL`] while an item is loaded.
    Position(Sample),
    /// The state changed.
    State(AirPlayState),
    /// External playback went on or off.
    External(bool),
    /// The item ran to its end.
    Ended,
    /// A stop or disconnect ended the item.
    Stopped,
}

/// An item to send.
pub struct SendRequest {
    pub client: Client,
    pub item_id: String,
    pub title: String,
    pub start_secs: f64,
    pub options: StreamOptions,
}

enum Cmd {
    Send(SendRequest, u64),
    Play,
    Pause,
    Seek(f64),
    Stop,
    SetVolume(f32),
    SetMuted(bool),
    /// Starts route detection; the status shows its result.
    Detect,
    /// Stops and lets the player go.
    Disconnect,
}

struct Shared {
    status: Mutex<AirPlayStatus>,
    events: (async_channel::Sender<AirPlayEvent>, async_channel::Receiver<AirPlayEvent>),
    /// The engine's player, for the route picker; null before the engine
    /// made it and after a disconnect.
    player: AtomicPtr<std::ffi::c_void>,
    tokens: AtomicU64,
}

impl Shared {
    fn emit(&self, event: AirPlayEvent) {
        let _ = self.events.0.try_send(event);
    }
}

/// Handle owned by the UI. Cheap to clone.
#[derive(Clone)]
pub struct AirPlay {
    shared: Arc<Shared>,
    commands: Arc<Mutex<Option<mpsc::Sender<Cmd>>>>,
    routes: Arc<OnceLock<Arc<routes::Routes>>>,
}

impl Default for AirPlay {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                status: Mutex::new(AirPlayStatus { volume: 1., ..Default::default() }),
                events: async_channel::bounded(256),
                player: AtomicPtr::new(std::ptr::null_mut()),
                tokens: AtomicU64::new(0),
            }),
            commands: Arc::new(Mutex::new(None)),
            routes: Arc::new(OnceLock::new()),
        }
    }
}

impl AirPlay {
    pub fn status(&self) -> AirPlayStatus {
        self.shared.status.lock().unwrap().clone()
    }

    /// The stream of events, for the panel; the debug channel reads the
    /// status instead.
    #[allow(dead_code)]
    pub fn events(&self) -> async_channel::Receiver<AirPlayEvent> {
        self.shared.events.1.clone()
    }

    /// An item is loaded or loading.
    pub fn active(&self) -> bool {
        matches!(
            self.status().state,
            AirPlayState::Loading | AirPlayState::Playing | AirPlayState::Paused | AirPlayState::Buffering
        )
    }

    /// Names of the receivers on the network. The first call starts the
    /// browse; the names come in over the next seconds.
    pub fn routes(&self) -> Vec<String> {
        self.browse().names()
    }

    /// Why the browse gives no names, when it failed.
    pub fn routes_error(&self) -> Option<String> {
        self.browse().error()
    }

    fn browse(&self) -> &routes::Routes {
        self.routes.get_or_init(routes::browse)
    }

    /// Starts the engine when it does not run. The player exists a moment
    /// later; [`Self::player_id`] waits for it.
    pub fn start(&self) {
        let mut commands = self.commands.lock().unwrap();
        if commands.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        *commands = Some(tx);
        let shared = self.shared.clone();
        let slot = self.commands.clone();
        thread::Builder::new()
            .name("airplay".into())
            .spawn(move || {
                Engine::new(shared.clone()).run(rx);
                shared.player.store(std::ptr::null_mut(), Ordering::SeqCst);
                *slot.lock().unwrap() = None;
            })
            .expect("spawn airplay thread");
    }

    fn send_cmd(&self, cmd: Cmd) {
        self.start();
        if let Some(tx) = self.commands.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
        }
    }

    /// The engine's `AVPlayer`, for the route picker. Starts the engine.
    pub fn player_id(&self) -> Id {
        self.start();
        let until = Instant::now() + START_WAIT;
        loop {
            let player = self.shared.player.load(Ordering::SeqCst);
            if !player.is_null() || Instant::now() > until {
                return player;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Sends an item. Returns the token of its `Loaded` or `LoadFailed`.
    pub fn send(&self, request: SendRequest) -> u64 {
        let token = self.shared.tokens.fetch_add(1, Ordering::SeqCst) + 1;
        self.send_cmd(Cmd::Send(request, token));
        token
    }

    pub fn play(&self) {
        self.send_cmd(Cmd::Play);
    }

    pub fn pause(&self) {
        self.send_cmd(Cmd::Pause);
    }

    pub fn seek(&self, secs: f64) {
        self.send_cmd(Cmd::Seek(secs));
    }

    pub fn stop(&self) {
        self.send_cmd(Cmd::Stop);
    }

    /// 0 to 1. A receiver may keep its own volume and ignore this.
    pub fn set_volume(&self, volume: f32) {
        self.send_cmd(Cmd::SetVolume(volume.clamp(0., 1.)));
    }

    pub fn set_muted(&self, muted: bool) {
        self.send_cmd(Cmd::SetMuted(muted));
    }

    pub fn detect_routes(&self) {
        self.send_cmd(Cmd::Detect);
    }

    /// Stops and ends the engine; the next command starts a new one.
    pub fn disconnect(&self) {
        picker::hide();
        if self.commands.lock().unwrap().is_some() {
            self.send_cmd(Cmd::Disconnect);
        }
    }
}

// ----- engine thread ---------------------------------------------------------

/// The item that plays, for the reports and the end of its transcode.
struct Session {
    client: Client,
    item_id: String,
    play_session_id: String,
    token: u64,
    start_secs: f64,
    last_report: Instant,
    sent_at: Instant,
}

/// What the last control waits to see, for the latency measurement.
enum Expect {
    Paused,
    Playing,
    Near(f64),
}

struct Engine {
    shared: Arc<Shared>,
    player: Player,
    control: Control,
    session: Option<Session>,
    reports: mpsc::Sender<Report>,
    detector: Option<RouteDetector>,
    expect: Option<(&'static str, Instant, Expect)>,
    /// A sent item has not run yet.
    await_first_run: bool,
}

impl Engine {
    fn new(shared: Arc<Shared>) -> Self {
        let player = Player::new();
        shared.player.store(player.id(), Ordering::SeqCst);
        Self {
            shared,
            player,
            control: Control::default(),
            session: None,
            reports: spawn_reporter(),
            detector: None,
            expect: None,
            await_first_run: false,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        loop {
            let _pool = Pool::new();
            let first = match rx.recv_timeout(POLL) {
                Ok(cmd) => Some(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let mut quit = false;
            for cmd in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
                if !self.handle(cmd) {
                    quit = true;
                }
            }
            if quit {
                break;
            }
            self.poll();
        }
        self.finish(false);
    }

    fn set_status(&self, change: impl FnOnce(&mut AirPlayStatus)) {
        change(&mut self.shared.status.lock().unwrap());
    }

    fn set_state(&self, state: AirPlayState) {
        let changed = {
            let mut status = self.shared.status.lock().unwrap();
            let changed = status.state != state;
            status.state = state;
            changed
        };
        if changed {
            self.shared.emit(AirPlayEvent::State(state));
        }
    }

    /// Runs one command. False when the engine must end.
    fn handle(&mut self, cmd: Cmd) -> bool {
        let now = Instant::now();
        match cmd {
            Cmd::Send(request, token) => self.send(request, token),
            Cmd::Play => {
                self.expect = Some(("play", now, Expect::Playing));
                let action = self.control.play();
                self.apply(action);
            }
            Cmd::Pause => {
                self.expect = Some(("pause", now, Expect::Paused));
                let action = self.control.pause();
                self.apply(action);
            }
            Cmd::Seek(secs) => {
                self.expect = Some(("seek", now, Expect::Near(secs)));
                let action = self.control.seek(secs, now);
                self.apply(action);
            }
            Cmd::Stop => {
                self.finish(true);
                self.shared.emit(AirPlayEvent::Stopped);
            }
            Cmd::SetVolume(volume) => {
                self.player.set_volume(volume);
                self.set_status(|s| s.volume = volume);
            }
            Cmd::SetMuted(muted) => {
                self.player.set_muted(muted);
                self.set_status(|s| s.muted = muted);
            }
            Cmd::Detect => {
                if self.detector.is_none() {
                    self.detector = Some(RouteDetector::new());
                }
            }
            Cmd::Disconnect => {
                self.finish(true);
                self.shared.emit(AirPlayEvent::Stopped);
                return false;
            }
        }
        true
    }

    fn send(&mut self, request: SendRequest, token: u64) {
        self.finish(true);
        let stream = match request.client.airplay_stream(&request.item_id, &request.options) {
            Ok(stream) => stream,
            Err(err) => {
                let error = format!("{err:#}");
                self.set_status(|s| s.error = Some(error.clone()));
                self.set_state(AirPlayState::Failed);
                self.shared.emit(AirPlayEvent::LoadFailed { token, error });
                return;
            }
        };
        log::info!("airplay: send {} from {:.1} s: {}", request.item_id, request.start_secs, redact(&stream.url));
        let now = Instant::now();
        self.player.load(&stream.url);
        self.control.send(token, request.start_secs, true);
        self.await_first_run = true;
        self.expect = None;
        self.set_status(|s| {
            s.item_id = Some(request.item_id.clone());
            s.title = request.title.clone();
            s.position = request.start_secs;
            s.position_at = None;
            s.duration = 0.;
            s.error = None;
            s.play_session_id = Some(stream.play_session_id.clone());
            s.load_latency = None;
            s.control_latency = None;
        });
        self.set_state(AirPlayState::Loading);
        self.session = Some(Session {
            client: request.client,
            item_id: request.item_id,
            play_session_id: stream.play_session_id,
            token,
            start_secs: request.start_secs,
            last_report: now,
            sent_at: now,
        });
    }

    fn apply(&mut self, action: Option<Action>) {
        match action {
            Some(Action::Play) => self.player.play(),
            Some(Action::Pause) => self.player.pause(),
            Some(Action::Seek(secs)) => self.player.seek(secs),
            Some(Action::Unload) => self.player.unload(),
            None => {}
        }
    }

    /// A look at the player: the status, the events, and the reports.
    fn poll(&mut self) {
        let now = Instant::now();
        if let Some(detector) = &self.detector {
            let seen = detector.multiple_routes();
            self.set_status(|s| s.multiple_routes = Some(seen));
        }
        let Some(token) = self.session.as_ref().map(|s| s.token) else {
            // No item: the route alone. A receiver the user picked in the
            // route picker shows as external playback, and the panel makes
            // AirPlay the target from it.
            self.note_external(self.player.external_playback_active());
            return;
        };
        if self.control.is_loading(token) {
            match self.player.status() {
                ItemStatus::ReadyToPlay => {
                    if let Some(actions) = self.control.ready(token, now) {
                        for action in actions {
                            self.apply(Some(action));
                        }
                    }
                    self.shared.emit(AirPlayEvent::Loaded { token });
                    self.report(ReportKind::Start);
                }
                ItemStatus::Failed => {
                    let error = self.player.error().unwrap_or_else(|| "the item could not be loaded".into());
                    log::warn!("airplay: load failed: {error}");
                    self.set_status(|s| s.error = Some(error.clone()));
                    self.finish(false);
                    self.set_state(AirPlayState::Failed);
                    self.shared.emit(AirPlayEvent::LoadFailed { token, error });
                    return;
                }
                ItemStatus::Unknown => return,
            }
        }
        if self.player.status() == ItemStatus::Failed {
            let error = self.player.error().unwrap_or_else(|| "playback failed".into());
            log::warn!("airplay: playback failed: {error}");
            self.set_status(|s| s.error = Some(error.clone()));
            self.finish(false);
            self.set_state(AirPlayState::Failed);
            return;
        }
        let position = self.player.position();
        let duration = self.player.duration().unwrap_or(0.);
        let time_control = self.player.time_control();
        let external = self.player.external_playback_active();
        let ended = self.player.reached_end();
        let action = self.control.tick(now, position);
        self.apply(action);
        let paused = time_control == TimeControl::Paused;
        let state = match time_control {
            TimeControl::Playing => AirPlayState::Playing,
            TimeControl::Waiting => AirPlayState::Buffering,
            TimeControl::Paused if ended => AirPlayState::Ended,
            TimeControl::Paused => AirPlayState::Paused,
        };
        // The position while a seek is in the air is the old one.
        let position = position.filter(|_| !self.control.seeking());
        if let Some(position) = position {
            self.measure(now, position, time_control);
        }
        {
            let mut status = self.shared.status.lock().unwrap();
            if let Some(position) = position {
                status.position = position;
                status.position_at = Some(now);
            }
            status.duration = duration;
        }
        self.note_external(external);
        if let Some(position) = position {
            self.shared.emit(AirPlayEvent::Position(Sample { position, at: now, paused, duration }));
        }
        if ended {
            self.finish(false);
            self.set_state(AirPlayState::Ended);
            self.shared.emit(AirPlayEvent::Ended);
            return;
        }
        self.set_state(state);
        let due = self.session.as_ref().is_some_and(|s| now.duration_since(s.last_report) >= REPORT_EVERY);
        if due {
            self.report(ReportKind::Progress);
        }
    }

    /// Keeps the status in step with the player's route, and tells the app
    /// when it changes.
    fn note_external(&self, external: bool) {
        let changed = {
            let mut status = self.shared.status.lock().unwrap();
            let changed = status.external != external;
            status.external = external;
            changed
        };
        if changed {
            log::info!("airplay: external playback {}", if external { "on" } else { "off" });
            self.shared.emit(AirPlayEvent::External(external));
        }
    }

    /// Records how long the last send or control took to show.
    fn measure(&mut self, now: Instant, position: f64, time_control: TimeControl) {
        if self.await_first_run
            && time_control == TimeControl::Playing
            && let Some(session) = &self.session
            && position > session.start_secs + 0.05
        {
            self.await_first_run = false;
            let took = now.duration_since(session.sent_at);
            log::info!("airplay: first run {} ms after send", took.as_millis());
            self.set_status(|s| s.load_latency = Some(took));
        }
        let Some((name, at, expect)) = &self.expect else {
            return;
        };
        let seen = match expect {
            Expect::Paused => time_control == TimeControl::Paused,
            Expect::Playing => time_control == TimeControl::Playing,
            Expect::Near(target) => (position - target).abs() < 1.,
        };
        if seen {
            let took = now.duration_since(*at);
            let name = *name;
            self.set_status(|s| s.control_latency = Some((name, took)));
            self.expect = None;
        }
    }

    fn report(&mut self, kind: ReportKind) {
        let Some(session) = &mut self.session else { return };
        let status = self.shared.status.lock().unwrap();
        let progress = Progress {
            item_id: session.item_id.clone(),
            play_session_id: session.play_session_id.clone(),
            position_ticks: (status.position * TICKS_PER_SECOND as f64) as i64,
            paused: status.state == AirPlayState::Paused,
            volume: (status.volume * 100.).round() as i64,
            muted: status.muted,
            play_method: "Transcode".to_string(),
            media_source_id: String::new(),
        };
        drop(status);
        session.last_report = Instant::now();
        let _ = self.reports.send(Report { client: session.client.clone(), kind, progress });
    }

    /// Ends the session: the stop report, the end of the transcode, and
    /// the item taken from the player when `unload`.
    fn finish(&mut self, unload: bool) {
        if self.session.is_some() {
            self.report(ReportKind::Stopped);
        }
        if let Some(session) = self.session.take() {
            let client = session.client;
            let id = session.play_session_id;
            // The request blocks; the engine must not wait on the server.
            // The server makes the job when the first playlist request
            // comes, which can be after a stop that follows a send at once;
            // a second request a moment later ends such a job.
            let _ = thread::Builder::new().name("airplay-encoding-stop".into()).spawn(move || {
                for wait in [Duration::ZERO, ENCODING_STOP_AGAIN] {
                    thread::sleep(wait);
                    match client.stop_encoding(&id) {
                        Ok(()) => log::debug!("airplay: encoding of session {} stopped", &id[..8]),
                        Err(err) => log::warn!("airplay: stop encoding failed: {err:#}"),
                    }
                }
            });
        }
        self.await_first_run = false;
        self.expect = None;
        if unload {
            for action in self.control.stop() {
                self.apply(Some(action));
            }
            self.set_status(|s| {
                s.item_id = None;
                s.title.clear();
                s.position = 0.;
                s.position_at = None;
                s.duration = 0.;
                s.external = false;
                s.play_session_id = None;
            });
            self.set_state(AirPlayState::Idle);
        } else {
            self.control.stop();
        }
    }
}
