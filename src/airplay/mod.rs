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
        atomic::{AtomicU64, Ordering},
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
/// How long a command waits for the engine's player to exist (measured:
/// 6 to 7 ms from cold).
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
    /// A look at the position, every [`POLL`] while an item is loaded and
    /// something changed. The reader gets the latest look, not every one
    /// (see [`Events`]).
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
    /// A URL as it is, with no server behind it: the debug channel plays a
    /// file on disk to see what the engine makes of it.
    SendUrl(String, u64),
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

/// Which engine this is. Each start of the engine thread counts one up;
/// 0 is no engine. An engine told to go keeps its number, so what it
/// does late is told apart from the work of the next one.
type Generation = u64;

/// The pointer of the `AVPlayer` of an engine, in the handoff to the route
/// picker.
#[derive(Clone, Copy)]
struct PlayerId(Id);

// SAFETY: the pointer crosses from the engine thread to the main thread
// inside `Handoff`, under its lock. It is in the handoff only while the
// engine owns the player (the engine takes it out under the same lock
// before it lets the player go), and whoever takes it out retains it
// under that lock before the lock is released: no pointer leaves the
// handoff that is not alive. AVFoundation allows calls to an `AVPlayer`
// from any thread.
unsafe impl Send for PlayerId {}

/// The player for the route picker, and which engine is the one that
/// runs. One lock keeps the two together: an engine publishes only while
/// it is the current one, and a disconnect makes the current one none and
/// takes the player away in one step.
#[derive(Default)]
struct Handoff {
    current: Generation,
    player: Option<(Generation, PlayerId)>,
}

/// The thread of the engine named waits at these points when a test asks
/// it to, so the test can put its own step between two steps of the
/// engine.
#[cfg(test)]
#[derive(Default)]
struct Gates {
    /// Right before the engine publishes its player.
    publish: Option<(Generation, Arc<std::sync::Barrier>)>,
    /// Right before the engine runs the disconnect that was sent to it.
    leave: Option<(Generation, Arc<std::sync::Barrier>)>,
    /// After the loop, right before the player goes.
    end: Option<(Generation, Arc<std::sync::Barrier>)>,
    /// The player is gone and the sender slot is given up: the end.
    gone: Option<(Generation, Arc<std::sync::Barrier>)>,
}

struct Shared {
    status: Mutex<AirPlayStatus>,
    /// The lifecycle events, every one; `Position` goes through `sample`.
    events: (async_channel::Sender<AirPlayEvent>, async_channel::Receiver<AirPlayEvent>),
    /// The latest look at the position, and whether a `Position` event
    /// for it is on the channel and not read yet. Many looks between two
    /// reads make one event; the reader gets the latest look.
    sample: Mutex<(Option<Sample>, bool)>,
    handoff: Mutex<Handoff>,
    tokens: AtomicU64,
    engines: AtomicU64,
    #[cfg(test)]
    gates: Mutex<Gates>,
}

impl Shared {
    fn emit(&self, event: AirPlayEvent) {
        // Unbounded: a send never fails, and the engine never waits for
        // the UI. The events are few; the samples go through `emit_sample`.
        let _ = self.events.0.try_send(event);
    }

    fn emit_sample(&self, sample: Sample) {
        let mut latest = self.sample.lock().unwrap();
        latest.0 = Some(sample);
        if !latest.1 {
            latest.1 = true;
            self.emit(AirPlayEvent::Position(sample));
        }
    }

    /// The event with the latest look in it, when it is a `Position`.
    fn fill(&self, event: AirPlayEvent) -> AirPlayEvent {
        match event {
            AirPlayEvent::Position(sent) => {
                let mut latest = self.sample.lock().unwrap();
                latest.1 = false;
                AirPlayEvent::Position(latest.0.unwrap_or(sent))
            }
            other => other,
        }
    }

    /// Takes the player of engine `generation` out of the handoff.
    fn forget_player(&self, generation: Generation) {
        let mut handoff = self.handoff.lock().unwrap();
        if handoff.player.is_some_and(|(owner, _)| owner == generation) {
            handoff.player = None;
        }
    }

    #[cfg(test)]
    fn gate(&self, generation: Generation, pick: impl FnOnce(&mut Gates) -> &mut Option<(Generation, Arc<std::sync::Barrier>)>) {
        let mut gates = self.gates.lock().unwrap();
        let gate = pick(&mut gates).take_if(|(wanted, _)| *wanted == generation);
        drop(gates);
        if let Some((_, gate)) = gate {
            gate.wait();
        }
    }
}

/// A retain on the engine's `AVPlayer`, for the route picker. The retain
/// goes back when it is dropped; the object lives at least that long.
pub struct PlayerRef(Id);

impl PlayerRef {
    pub fn id(&self) -> Id {
        self.0
    }
}

impl Drop for PlayerRef {
    fn drop(&mut self) {
        crate::macos::send!((), self.0, c"release");
    }
}

/// The events of the engine, for one reader. A `Position` carries the
/// latest look at the time of the read: the engine looks four times a
/// second, and a reader that fell behind gets one event, not the backlog.
pub struct Events {
    rx: async_channel::Receiver<AirPlayEvent>,
    shared: Arc<Shared>,
}

impl Events {
    pub async fn recv(&self) -> Result<AirPlayEvent, async_channel::RecvError> {
        Ok(self.shared.fill(self.rx.recv().await?))
    }

    #[cfg(test)]
    fn try_recv(&self) -> Option<AirPlayEvent> {
        self.rx.try_recv().ok().map(|event| self.shared.fill(event))
    }
}

/// The sender of the engine that runs, with its number; none between a
/// disconnect and the next command.
type CommandSlot = Mutex<Option<(Generation, mpsc::Sender<Cmd>)>>;

/// Handle owned by the UI. Cheap to clone.
#[derive(Clone)]
pub struct AirPlay {
    shared: Arc<Shared>,
    commands: Arc<CommandSlot>,
    routes: Arc<OnceLock<Arc<routes::Routes>>>,
}

impl Default for AirPlay {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                status: Mutex::new(AirPlayStatus { volume: 1., ..Default::default() }),
                events: async_channel::unbounded(),
                sample: Mutex::new((None, false)),
                handoff: Mutex::new(Handoff::default()),
                tokens: AtomicU64::new(0),
                engines: AtomicU64::new(0),
                #[cfg(test)]
                gates: Mutex::new(Gates::default()),
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
    pub fn events(&self) -> Events {
        Events { rx: self.shared.events.1.clone(), shared: self.shared.clone() }
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
    /// later; [`Self::player`] waits for it.
    pub fn start(&self) {
        self.commands.lock().unwrap().get_or_insert_with(|| self.spawn());
    }

    /// Starts an engine thread. The caller holds the lock of the command
    /// slot and puts the result in it, so no two engines start at once and
    /// a disconnect sees either none or this one.
    fn spawn(&self) -> (Generation, mpsc::Sender<Cmd>) {
        let generation = self.shared.engines.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = mpsc::channel();
        // The current engine, before its thread exists: what the thread
        // publishes is checked against this.
        self.shared.handoff.lock().unwrap().current = generation;
        let shared = self.shared.clone();
        let slot = self.commands.clone();
        thread::Builder::new()
            .name("airplay".into())
            .spawn(move || {
                // Around the making of the player and the tail after the
                // loop; each turn of the loop has a pool of its own.
                let _pool = Pool::new();
                Engine::new(shared.clone(), generation).run(rx);
                // A newer engine may hold the slot by now (a disconnect
                // takes it at once): only this engine's own sender goes.
                let mut slot = slot.lock().unwrap();
                if slot.as_ref().is_some_and(|(owner, _)| *owner == generation) {
                    *slot = None;
                }
                drop(slot);
                #[cfg(test)]
                shared.gate(generation, |g| &mut g.gone);
            })
            .expect("spawn airplay thread");
        (generation, tx)
    }

    /// To the engine that runs, or to a new one when none does: under one
    /// lock, so a disconnect cannot come between the start and the send.
    fn send_cmd(&self, cmd: Cmd) {
        let mut commands = self.commands.lock().unwrap();
        let (_, tx) = commands.get_or_insert_with(|| self.spawn());
        let _ = tx.send(cmd);
    }

    /// The engine's `AVPlayer`, retained, for the route picker. Starts the
    /// engine; none when its player is not there within [`START_WAIT`].
    pub fn player(&self) -> Option<PlayerRef> {
        self.start();
        let until = Instant::now() + START_WAIT;
        loop {
            {
                let handoff = self.shared.handoff.lock().unwrap();
                // Retained under the lock that takes it away: what goes
                // out lives.
                if let Some((owner, PlayerId(id))) = handoff.player
                    && owner == handoff.current
                {
                    return Some(PlayerRef(crate::macos::send!(Id, id, c"retain")));
                }
            }
            if Instant::now() > until {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn next_token(&self) -> u64 {
        self.shared.tokens.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Sends an item. Returns the token of its `Loaded` or `LoadFailed`.
    pub fn send(&self, request: SendRequest) -> u64 {
        let token = self.next_token();
        self.send_cmd(Cmd::Send(request, token));
        token
    }

    /// Plays a URL as it is, with no server: for the debug channel.
    pub fn send_url(&self, url: String) -> u64 {
        let token = self.next_token();
        self.send_cmd(Cmd::SendUrl(url, token));
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

    /// Stops and ends the engine; the next command starts a new one. The
    /// engine's sender goes with this call, so a command that follows at
    /// once goes to the new engine, not into the one on its way out; and
    /// so does its player, so the next picker waits for the next engine's.
    ///
    /// The engine that leaves says nothing from here on (`Engine::speaks`):
    /// the next one may run before it got to its disconnect, and what it
    /// said then would undo what the next one set. So the end of its item
    /// is told here, in the same step that withdraws it.
    pub fn disconnect(&self) {
        picker::hide();
        // Held to the end: no engine starts between the steps.
        let mut commands = self.commands.lock().unwrap();
        let Some((_, tx)) = commands.take() else {
            return;
        };
        {
            let mut handoff = self.shared.handoff.lock().unwrap();
            handoff.current = 0;
            handoff.player = None;
            let was = {
                let mut status = self.shared.status.lock().unwrap();
                clear_item(&mut status);
                std::mem::replace(&mut status.state, AirPlayState::Idle)
            };
            if was != AirPlayState::Idle {
                self.shared.emit(AirPlayEvent::State(AirPlayState::Idle));
            }
            self.shared.emit(AirPlayEvent::Stopped);
        }
        // The engine ends its session (the stop report, the transcode) and
        // lets its player go.
        let _ = tx.send(Cmd::Disconnect);
    }
}

/// The status with no item in it.
fn clear_item(status: &mut AirPlayStatus) {
    status.item_id = None;
    status.title.clear();
    status.position = 0.;
    status.position_at = None;
    status.duration = 0.;
    status.external = false;
    status.play_session_id = None;
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
    generation: Generation,
    player: Player,
    control: Control,
    session: Option<Session>,
    reports: mpsc::Sender<Report>,
    detector: Option<RouteDetector>,
    expect: Option<(&'static str, Instant, Expect)>,
    /// A sent item has not run yet.
    await_first_run: bool,
    /// The look last told (position, paused, duration); a paused item that
    /// stays as it was is not told again.
    told: Option<(f64, bool, f64)>,
    /// Where the item is and whether it stands paused, as this engine saw
    /// it last, for the reports. The status is not the source: after a
    /// disconnect it is cleared, and the next engine writes it, before
    /// this one sends its stop report.
    seen: (f64, bool),
}

impl Engine {
    fn new(shared: Arc<Shared>, generation: Generation) -> Self {
        let player = Player::new();
        // The volume and the mute are the app's and outlive an engine: a
        // new player starts at full volume with the sound on, and the
        // status would say otherwise.
        let (volume, muted) = {
            let status = shared.status.lock().unwrap();
            (status.volume, status.muted)
        };
        player.set_volume(volume);
        player.set_muted(muted);
        #[cfg(test)]
        shared.gate(generation, |g| &mut g.publish);
        {
            // An engine that was told to go before it got here (a fast
            // disconnect) publishes nothing: its player is not the one
            // the picker must get.
            let mut handoff = shared.handoff.lock().unwrap();
            if handoff.current == generation {
                handoff.player = Some((generation, PlayerId(player.id())));
            }
        }
        Self {
            shared,
            generation,
            player,
            control: Control::default(),
            session: None,
            reports: spawn_reporter(),
            detector: None,
            expect: None,
            await_first_run: false,
            told: None,
            seen: (0., false),
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
                if quit {
                    // Behind a disconnect. Nothing can be here: the sender
                    // goes with the disconnect, and what comes after it
                    // starts the next engine.
                    continue;
                }
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
        #[cfg(test)]
        self.shared.gate(self.generation, |g| &mut g.end);
        // Before the player goes with this engine.
        self.shared.forget_player(self.generation);
    }

    /// This engine may write the status and tell events: it is the one
    /// that runs. An engine that was told to go says nothing more; the
    /// disconnect said the end of its item for it (`AirPlay::disconnect`),
    /// and what it did late would undo what the next engine set.
    fn speaks(&self, handoff: &Handoff) -> bool {
        handoff.current == self.generation
    }

    fn set_status(&self, change: impl FnOnce(&mut AirPlayStatus)) {
        let handoff = self.shared.handoff.lock().unwrap();
        if self.speaks(&handoff) {
            change(&mut self.shared.status.lock().unwrap());
        }
    }

    fn emit(&self, event: AirPlayEvent) {
        let handoff = self.shared.handoff.lock().unwrap();
        if self.speaks(&handoff) {
            self.shared.emit(event);
        }
    }

    fn set_state(&self, state: AirPlayState) {
        let handoff = self.shared.handoff.lock().unwrap();
        if !self.speaks(&handoff) {
            return;
        }
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
            Cmd::SendUrl(url, token) => {
                // No server: the reports go to a port with nobody on it.
                let client = Client::new("http://127.0.0.1:9", "airplay-url").with_session("none", "none");
                let stream = stream::Stream { url, play_session_id: "url".into() };
                self.send_stream(client, "url".into(), "A URL".into(), 0., stream, token);
            }
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
                self.emit(AirPlayEvent::Stopped);
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
                #[cfg(test)]
                self.shared.gate(self.generation, |g| &mut g.leave);
                // The handoff and the status were cleared by the
                // disconnect, which told the stop as well; the picker that
                // holds a retain keeps it. What is left is the session.
                self.finish(true);
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
                self.emit(AirPlayEvent::LoadFailed { token, error });
                return;
            }
        };
        log::info!("airplay: send {} from {:.1} s: {}", request.item_id, request.start_secs, redact(&stream.url));
        self.send_stream(request.client, request.item_id, request.title, request.start_secs, stream, token);
    }

    /// Hands the stream to the player and opens the session of the item.
    fn send_stream(&mut self, client: Client, item_id: String, title: String, start_secs: f64, stream: stream::Stream, token: u64) {
        let now = Instant::now();
        self.player.load(&stream.url);
        self.control.send(token, start_secs, true);
        self.await_first_run = true;
        self.expect = None;
        self.told = None;
        self.seen = (start_secs, false);
        self.set_status(|s| {
            s.item_id = Some(item_id.clone());
            s.title = title;
            s.position = start_secs;
            s.position_at = None;
            s.duration = 0.;
            s.error = None;
            s.play_session_id = Some(stream.play_session_id.clone());
            s.load_latency = None;
            s.control_latency = None;
        });
        self.set_state(AirPlayState::Loading);
        self.session = Some(Session {
            client,
            item_id,
            play_session_id: stream.play_session_id,
            token,
            start_secs,
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
                    self.emit(AirPlayEvent::Loaded { token });
                    self.report(ReportKind::Start);
                }
                ItemStatus::Failed => {
                    let error = self.player.error().unwrap_or_else(|| "the item could not be loaded".into());
                    log::warn!("airplay: load failed: {error}");
                    self.set_status(|s| s.error = Some(error.clone()));
                    self.finish(false);
                    self.set_state(AirPlayState::Failed);
                    self.emit(AirPlayEvent::LoadFailed { token, error });
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
        // A player that stands at the end wanted to play: the item ran
        // out. One the user paused there is paused, and plays on from
        // there (the last half second) when asked.
        let ended = self.player.reached_end() && self.control.wants_play();
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
        if let Some(position) = position {
            self.seen.0 = position;
        }
        self.set_status(|status| {
            if let Some(position) = position {
                status.position = position;
                status.position_at = Some(now);
            }
            status.duration = duration;
        });
        self.note_external(external);
        // A paused item as it was at the look told before has nothing new
        // to say; every event is a frame of the window.
        let look = position.map(|position| (position, paused, duration));
        if let Some(look) = look.filter(|look| !paused || self.told != Some(*look)) {
            self.told = Some(look);
            let (position, paused, duration) = look;
            self.emit_sample(Sample { position, at: now, paused, duration });
        }
        if ended {
            self.finish(false);
            self.set_state(AirPlayState::Ended);
            self.emit(AirPlayEvent::Ended);
            return;
        }
        self.set_state(state);
        self.seen.1 = state == AirPlayState::Paused;
        let due = self.session.as_ref().is_some_and(|s| now.duration_since(s.last_report) >= REPORT_EVERY);
        if due {
            self.report(ReportKind::Progress);
        }
    }

    fn emit_sample(&self, sample: Sample) {
        let handoff = self.shared.handoff.lock().unwrap();
        if self.speaks(&handoff) {
            self.shared.emit_sample(sample);
        }
    }

    /// Keeps the status in step with the player's route, and tells the app
    /// when it changes.
    fn note_external(&self, external: bool) {
        let handoff = self.shared.handoff.lock().unwrap();
        if !self.speaks(&handoff) {
            return;
        }
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
        let (position, paused) = self.seen;
        // The volume is the app's, the same for every engine.
        let status = self.shared.status.lock().unwrap();
        let progress = Progress {
            item_id: session.item_id.clone(),
            play_session_id: session.play_session_id.clone(),
            position_ticks: (position * TICKS_PER_SECOND as f64) as i64,
            paused,
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
        self.told = None;
        if unload {
            for action in self.control.stop() {
                self.apply(Some(action));
            }
            self.set_status(clear_item);
            self.set_state(AirPlayState::Idle);
        } else {
            self.control.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    //! The handoff of the player and the commands around a disconnect. An
    //! `AVPlayer` with an item needs the main run loop to load, which a
    //! test has not: what the engine makes of an item runs through the
    //! app (`dev/jctl airplay file <path>`).

    use std::sync::Barrier;

    use super::*;

    /// A barrier for the test and the thread of engine `generation`, at
    /// one of its gates.
    fn gate(airplay: &AirPlay, generation: Generation, pick: impl FnOnce(&mut Gates) -> &mut Option<(Generation, Arc<Barrier>)>) -> Arc<Barrier> {
        let barrier = Arc::new(Barrier::new(2));
        *pick(&mut airplay.shared.gates.lock().unwrap()) = Some((generation, barrier.clone()));
        barrier
    }

    /// The player answers: `allowsExternalPlayback` is set at its making.
    fn alive(player: &PlayerRef) -> bool {
        crate::macos::send!(bool, player.id(), c"allowsExternalPlayback")
    }

    /// How long the first command waits for the player of a new engine:
    /// the time `place_picker` can hold the UI thread.
    #[test]
    fn the_wait_for_a_new_player() {
        let mut waits = Vec::new();
        for _ in 0..3 {
            let airplay = AirPlay::default();
            let gone = gate(&airplay, 1, |g| &mut g.gone);
            let started = Instant::now();
            let player = airplay.player();
            waits.push(started.elapsed());
            assert!(player.is_some());
            airplay.disconnect();
            gone.wait();
        }
        eprintln!("player from cold: {waits:?}");
        assert!(waits.iter().all(|w| *w < START_WAIT));
    }

    /// The player handed to the picker lives for as long as the picker
    /// holds it, past the end of its engine; and after a disconnect the
    /// picker never gets the player of the engine that leaves.
    #[test]
    fn the_picker_keeps_a_live_player_past_the_end_of_its_engine() {
        let airplay = AirPlay::default();
        let gone = gate(&airplay, 1, |g| &mut g.gone);
        let first = airplay.player().expect("a player");
        assert!(alive(&first));
        airplay.disconnect();
        // Out of the handoff at once, before the engine got to it.
        assert!(airplay.shared.handoff.lock().unwrap().player.is_none());
        // The first engine is over: its own retain on the player is gone.
        gone.wait();
        assert!(alive(&first), "the player of the picker was released with its engine");
        // The next engine has a player of its own.
        let second = airplay.player().expect("a player of the next engine");
        assert_ne!(second.id(), first.id());
        drop(second);
        let gone = gate(&airplay, 2, |g| &mut g.gone);
        airplay.disconnect();
        gone.wait();
        drop(first);
    }

    /// An engine that finishes its start after a disconnect withdrew it
    /// publishes nothing, not over the player of the engine after it; and
    /// its end takes nothing from that engine.
    #[test]
    fn a_withdrawn_engine_does_not_publish_over_the_next_one() {
        let airplay = AirPlay::default();
        // The first engine stops right before it would publish.
        let publish = gate(&airplay, 1, |g| &mut g.publish);
        airplay.start();
        airplay.disconnect();
        // The second engine runs through; the picker gets its player.
        airplay.detect_routes();
        let second = airplay.player().expect("the player of the second engine");
        let second_id = second.id();
        // The first engine goes on: publishes (must not), then handles its
        // disconnect and ends (must not take the second's player away).
        let gone = gate(&airplay, 1, |g| &mut g.gone);
        publish.wait();
        gone.wait();
        let handed = airplay.player().expect("a player");
        assert_eq!(handed.id(), second_id, "the player of a withdrawn engine was published");
        assert_eq!(airplay.status().state, AirPlayState::Idle);
        drop(handed);
        drop(second);
        let gone = gate(&airplay, 2, |g| &mut g.gone);
        airplay.disconnect();
        gone.wait();
    }

    /// A command right after a disconnect goes to a new engine, not into
    /// the one that is on its way out.
    #[test]
    fn a_command_after_a_disconnect_starts_a_new_engine() {
        let airplay = AirPlay::default();
        airplay.start();
        assert!(airplay.player().is_some());
        let gone = gate(&airplay, 1, |g| &mut g.gone);
        airplay.disconnect();
        airplay.detect_routes();
        let until = Instant::now() + Duration::from_secs(2);
        while airplay.status().multiple_routes.is_none() {
            assert!(Instant::now() < until, "the detect after the disconnect was lost: {:?}", airplay.status());
            thread::sleep(Duration::from_millis(20));
        }
        gone.wait();
        // The first engine ended; the second runs, with its player.
        assert!(airplay.commands.lock().unwrap().is_some());
        assert!(airplay.player().is_some());
        let gone = gate(&airplay, 2, |g| &mut g.gone);
        airplay.disconnect();
        gone.wait();
    }

    /// The next engine starts before the one that leaves got to its
    /// disconnect (the panel asks for the picker's player in the frame
    /// after a disconnect). The item of the engine that leaves still ends
    /// for the app: the status is clear at once and stays so, and one
    /// `Stopped` comes. What the old engine does late changes nothing.
    #[test]
    fn a_disconnect_ends_the_item_also_when_the_next_engine_runs_first() {
        let airplay = AirPlay::default();
        let events = airplay.events();
        // An item: a file that is not there. Whether the player fails it
        // or never loads it, the status has it from the send on.
        airplay.send_url("file:///nonexistent/bloom-test.mp4".into());
        let until = Instant::now() + Duration::from_secs(2);
        while airplay.status().item_id.is_none() {
            assert!(Instant::now() < until, "the send did not show: {:?}", airplay.status());
            thread::sleep(Duration::from_millis(5));
        }
        // The first engine stops right before it runs its disconnect.
        let leave = gate(&airplay, 1, |g| &mut g.leave);
        let gone = gate(&airplay, 1, |g| &mut g.gone);
        airplay.disconnect();
        let status = airplay.status();
        assert_eq!((status.state, status.item_id, status.play_session_id), (AirPlayState::Idle, None, None));
        // The second engine runs and writes the status.
        airplay.set_volume(0.25);
        let until = Instant::now() + Duration::from_secs(2);
        while airplay.status().volume != 0.25 {
            assert!(Instant::now() < until, "the second engine does not run: {:?}", airplay.status());
            thread::sleep(Duration::from_millis(5));
        }
        // The first engine goes on to its end.
        leave.wait();
        gone.wait();
        let status = airplay.status();
        assert_eq!((status.state, status.item_id, status.volume), (AirPlayState::Idle, None, 0.25));
        let mut got = Vec::new();
        while let Some(event) = events.try_recv() {
            got.push(event);
        }
        assert_eq!(got.iter().filter(|e| **e == AirPlayEvent::Stopped).count(), 1, "{got:?}");
        assert_eq!(got.last(), Some(&AirPlayEvent::Stopped), "something was told after the stop: {got:?}");
        let gone = gate(&airplay, 2, |g| &mut g.gone);
        airplay.disconnect();
        gone.wait();
    }

    /// The player of the engine after a disconnect has the mute and the
    /// volume the app set, as the status says: a new `AVPlayer` starts
    /// with the sound on at full volume.
    #[test]
    fn a_new_engine_takes_the_volume_and_the_mute_of_the_app() {
        let airplay = AirPlay::default();
        airplay.set_muted(true);
        airplay.set_volume(0.25);
        let until = Instant::now() + Duration::from_secs(2);
        while !(airplay.status().muted && airplay.status().volume == 0.25) {
            assert!(Instant::now() < until, "{:?}", airplay.status());
            thread::sleep(Duration::from_millis(5));
        }
        let gone = gate(&airplay, 1, |g| &mut g.gone);
        airplay.disconnect();
        gone.wait();
        let player = airplay.player().expect("the player of the next engine");
        assert!(crate::macos::send!(bool, player.id(), c"isMuted"), "the next engine plays with sound");
        assert_eq!(crate::macos::send!(f32, player.id(), c"volume"), 0.25);
        drop(player);
        let gone = gate(&airplay, 2, |g| &mut g.gone);
        airplay.disconnect();
        gone.wait();
    }

    /// Many looks at the position between two reads make one event with
    /// the latest look; the events around them all come through.
    #[test]
    fn lifecycle_events_survive_a_burst_of_samples() {
        let airplay = AirPlay::default();
        let events = airplay.events();
        let shared = &airplay.shared;
        let at = Instant::now();
        let look = |position: f64| Sample { position, at, paused: false, duration: 600. };
        shared.emit(AirPlayEvent::State(AirPlayState::Playing));
        for n in 0..10_000 {
            shared.emit_sample(look(n as f64));
        }
        shared.emit(AirPlayEvent::Stopped);
        let mut got = Vec::new();
        while let Some(event) = events.try_recv() {
            got.push(event);
        }
        assert_eq!(
            got,
            [AirPlayEvent::State(AirPlayState::Playing), AirPlayEvent::Position(look(9_999.)), AirPlayEvent::Stopped]
        );
        // The next look is an event again.
        shared.emit_sample(look(10_000.));
        assert_eq!(events.try_recv(), Some(AirPlayEvent::Position(look(10_000.))));
        assert_eq!(events.try_recv(), None);
    }
}
