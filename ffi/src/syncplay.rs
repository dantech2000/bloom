//! SyncPlay for the iOS app: the Mac's session driver (src/syncplay/
//! session.rs) around core's rules (`syncplay::core::Core`), with the
//! player behind a foreign trait.
//!
//! One thread holds the core. It reads what comes on its inbox (the
//! server's messages, forwarded by the socket; the player's events; the
//! user's intents; the clock's measurements), ticks a few times a second,
//! and runs a scheduled start or pause at its time by calling the player
//! from this thread: the Swift side carries a timed call to mpv out at once,
//! with no hop through the main actor. A load and a stop are the Swift
//! side's to do (they open or close the player view).

use std::{
    sync::{Arc, Mutex, RwLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use bloom_core::{
    jellyfin::Client,
    realtime::SocketEvent,
    syncplay::{
        clock::{Exchange, LocalClock, ServerClock},
        core::{Action, Core, Input, Intent, Notice, PlayerEvent, PlayerView},
        protocol::{self, GroupInfo, RepeatMode, Request, ShuffleMode},
    },
};
use serde::Deserialize;

use crate::{
    BloomError, Server,
    kinds::{SyncDenial, SyncGroupState, SyncPhase, SyncReason, SyncRepeat},
    lock, read, run, write,
};

/// What the session asks of the player. Called from the session's thread;
/// an implementation must not wait for the main actor.
#[uniffi::export(with_foreign)]
pub trait SyncPlayer: Send + Sync {
    /// Load an item paused at a place, then answer `loaded(token)` or
    /// `load_failed(token)`. Opens the player when it is closed.
    fn load(&self, item_id: String, position_secs: f64, token: u64);
    /// Pause, seek exactly, then answer `settled(token)`.
    fn seek_paused(&self, position_secs: f64, token: u64);
    /// Start to play now (the time came).
    fn unpause(&self);
    /// Pause now.
    fn pause(&self);
    /// Pause now, then go to a place.
    fn pause_and_seek(&self, position_secs: f64);
    /// The speed as a factor of the normal one (drift correction).
    fn set_sync_speed(&self, factor: f64);
    /// Stop and close the player.
    fn stop(&self);
    /// News for the screen.
    fn notice(&self, notice: SyncNotice);
}

/// News for the screen, in the app's terms.
#[derive(uniffi::Enum, Debug, Clone)]
pub enum SyncNotice {
    Joined { group: String },
    Left,
    UserJoined { user: String },
    UserLeft { user: String },
    /// The group's state, with the server's reason.
    State { state: SyncGroupState, reason: SyncReason },
    QueueChanged,
    /// The server refused something.
    Denied { reason: SyncDenial },
    /// This member stepped aside: the group no longer waits for it.
    SteppedAside { why: String },
    /// A request of the user did not reach the server.
    Failed { what: String },
}

impl From<Notice> for SyncNotice {
    fn from(notice: Notice) -> Self {
        match notice {
            Notice::Joined(group) => Self::Joined { group },
            Notice::Left => Self::Left,
            Notice::UserJoined(user) => Self::UserJoined { user },
            Notice::UserLeft(user) => Self::UserLeft { user },
            Notice::State(state, reason) => Self::State { state: state.into(), reason: SyncReason::parse(&reason) },
            Notice::Queue => Self::QueueChanged,
            Notice::Denied(reason) => Self::Denied { reason: SyncDenial::parse(&reason) },
            Notice::SteppedAside(why) => Self::SteppedAside { why: why.into() },
        }
    }
}

/// A group of the server's list.
#[derive(uniffi::Record)]
pub struct SyncGroup {
    pub id: String,
    pub name: String,
    pub state: SyncGroupState,
    pub members: Vec<String>,
}

impl From<&GroupInfo> for SyncGroup {
    fn from(group: &GroupInfo) -> Self {
        Self {
            id: group.group_id.clone(),
            name: group.group_name.clone(),
            state: group.state.into(),
            members: group.participants.clone(),
        }
    }
}

#[derive(uniffi::Record)]
pub struct SyncQueueEntry {
    pub item_id: String,
    pub playlist_item_id: String,
}

/// The session as the screen shows it.
#[derive(uniffi::Record)]
pub struct SyncState {
    pub group: Option<SyncGroup>,
    pub following: bool,
    pub queue: Vec<SyncQueueEntry>,
    /// The index of the item that plays.
    pub playing_index: Option<u32>,
    pub shuffle: bool,
    pub repeat: SyncRepeat,
    pub phase: SyncPhase,
    /// Server time minus local time, with the delay of this player, in
    /// milliseconds (the Mac panel's "Server clock").
    pub clock_ms: f64,
    /// How far this player is from the group, in milliseconds.
    pub drift_ms: Option<f64>,
    pub ping_ms: f64,
    pub socket_open: bool,
}

/// What the user does in a group.
#[derive(uniffi::Enum)]
pub enum SyncIntent {
    Create { name: String },
    Join { group_id: String },
    Leave,
    TogglePause,
    /// Pause the group (a call or Siri took the sound of this player).
    Pause,
    Unpause,
    Seek { position_secs: f64 },
    Next,
    Previous,
    /// Play these items in the group, from one of them.
    Play { item_ids: Vec<String>, index: u32, start_secs: f64 },
    Enqueue { item_ids: Vec<String>, next: bool },
    Remove { playlist_item_ids: Vec<String> },
    Jump { playlist_item_id: String },
    /// Steps the repeat mode: none, all, one.
    CycleRepeat,
    ToggleShuffle,
    StopFollowing,
    Follow,
}

enum Message {
    Socket(SocketEvent),
    Player(PlayerEvent),
    Position { secs: f64, paused: bool, duration_secs: f64, at: Instant },
    Closed,
    User(Intent),
    Exchange(Exchange),
    Failed(&'static str),
    Correct(bool),
    Delay(f64),
    /// The app dropped the session: leave a group and end the threads.
    Shutdown,
}

/// Tokens of loads start here; the tokens of seeks count up from 1.
const LOAD_TOKENS: u64 = 1 << 40;
const TICK: Duration = Duration::from_millis(100);

/// The SyncPlay session of a signed-in server.
#[derive(uniffi::Object)]
pub struct SyncSession {
    inbox: mpsc::Sender<Message>,
    state: Arc<Mutex<Snapshot>>,
}

#[derive(Default)]
struct Snapshot {
    group: Option<GroupInfo>,
    following: bool,
    queue: Option<protocol::PlayQueue>,
    phase: SyncPhase,
    clock_ms: f64,
    drift_ms: Option<f64>,
    ping_ms: f64,
    socket_open: bool,
}

#[uniffi::export]
impl SyncSession {
    pub fn user(&self, intent: SyncIntent) {
        let queue = lock(&self.state).queue.clone();
        let intent = match intent {
            SyncIntent::Create { name } => Intent::Create { name },
            SyncIntent::Join { group_id } => Intent::Join { group_id },
            SyncIntent::Leave => Intent::Leave,
            SyncIntent::TogglePause => Intent::TogglePause,
            SyncIntent::Pause => Intent::Pause,
            SyncIntent::Unpause => Intent::Unpause,
            SyncIntent::Seek { position_secs } => Intent::Seek(position_secs * 1000.),
            SyncIntent::Next => Intent::Next,
            SyncIntent::Previous => Intent::Previous,
            SyncIntent::Play { item_ids, index, start_secs } => {
                Intent::Play { item_ids, index: index as usize, start: start_secs * 1000. }
            }
            SyncIntent::Enqueue { item_ids, next } => Intent::Enqueue { item_ids, next },
            SyncIntent::Remove { playlist_item_ids } => Intent::Remove { playlist_item_ids },
            SyncIntent::Jump { playlist_item_id } => Intent::Jump { playlist_item_id },
            SyncIntent::CycleRepeat => Intent::Repeat(queue.map_or(RepeatMode::RepeatAll, |q| q.repeat_mode.next())),
            SyncIntent::ToggleShuffle => Intent::Shuffle(queue.map_or(ShuffleMode::Shuffle, |q| q.shuffle_mode.next())),
            SyncIntent::StopFollowing => Intent::StopFollowing,
            SyncIntent::Follow => Intent::Follow,
        };
        let _ = self.inbox.send(Message::User(intent));
    }

    /// The player measured its position (from mpv's event thread: the time
    /// of the call is the time of the measurement).
    pub fn position(&self, position_secs: f64, paused: bool, duration_secs: f64) {
        let _ = self.inbox.send(Message::Position { secs: position_secs, paused, duration_secs, at: Instant::now() });
    }

    pub fn loaded(&self, token: u64) {
        let _ = self.inbox.send(Message::Player(PlayerEvent::Loaded { playlist_item_id: token.to_string() }));
    }

    pub fn load_failed(&self, token: u64) {
        let _ = self.inbox.send(Message::Player(PlayerEvent::LoadFailed { playlist_item_id: token.to_string() }));
    }

    pub fn settled(&self, token: u64) {
        let _ = self.inbox.send(Message::Player(PlayerEvent::Settled { token }));
    }

    pub fn stalled(&self) {
        let _ = self.inbox.send(Message::Player(PlayerEvent::Stalled));
    }

    pub fn recovered(&self) {
        let _ = self.inbox.send(Message::Player(PlayerEvent::Recovered));
    }

    pub fn ended(&self) {
        let _ = self.inbox.send(Message::Player(PlayerEvent::Ended));
    }

    /// The user closed the player.
    pub fn closed(&self) {
        let _ = self.inbox.send(Message::Closed);
    }

    /// Keep in step with the group (drift correction); on by default.
    pub fn set_correct(&self, on: bool) {
        let _ = self.inbox.send(Message::Correct(on));
    }

    /// How much later than the group this player plays, in milliseconds;
    /// negative plays earlier (for a sound output with a delay of its own,
    /// such as Bluetooth). As the Mac: the clock of the server is seen
    /// behind by that much.
    pub fn set_delay(&self, ms: f64) {
        let _ = self.inbox.send(Message::Delay(ms));
    }

    pub fn state(&self) -> SyncState {
        let s = lock(&self.state);
        SyncState {
            group: s.group.as_ref().map(SyncGroup::from),
            following: s.following,
            queue: s.queue.as_ref().map_or_else(Vec::new, |q| {
                q.playlist
                    .iter()
                    .map(|i| SyncQueueEntry { item_id: i.item_id.clone(), playlist_item_id: i.playlist_item_id.clone() })
                    .collect()
            }),
            playing_index: s.queue.as_ref().and_then(|q| u32::try_from(q.playing_item_index).ok()),
            shuffle: s.queue.as_ref().is_some_and(|q| q.shuffle_mode == ShuffleMode::Shuffle),
            repeat: s.queue.as_ref().map_or(RepeatMode::RepeatNone, |q| q.repeat_mode).into(),
            phase: s.phase,
            clock_ms: s.clock_ms,
            drift_ms: s.drift_ms,
            ping_ms: s.ping_ms,
            socket_open: s.socket_open,
        }
    }
}

impl Drop for SyncSession {
    fn drop(&mut self) {
        let _ = self.inbox.send(Message::Shutdown);
    }
}

impl SyncSession {
    /// Gives the session an event of the server's socket.
    pub(crate) fn socket(&self, event: &SocketEvent) {
        let _ = self.inbox.send(Message::Socket(event.clone()));
    }
}

#[uniffi::export]
impl Server {
    /// The groups on the server.
    pub async fn syncplay_groups(&self) -> Result<Vec<SyncGroup>, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.syncplay_groups::<Vec<GroupInfo>>()?.iter().map(SyncGroup::from).collect())).await
    }

    /// A SyncPlay session for this server; give it to the socket with
    /// `ServerSocket::attach_syncplay`. Its threads end when it is dropped.
    pub fn syncplay(&self, player: Arc<dyn SyncPlayer>) -> Result<Arc<SyncSession>, BloomError> {
        let client = self.signed_in()?;
        let (inbox, messages) = mpsc::channel();
        let state = Arc::new(Mutex::new(Snapshot { following: true, ..Snapshot::default() }));
        let local = Arc::new(RwLock::new(LocalClock::new()));
        let poll = spawn_clock(client.clone(), local.clone(), inbox.clone());
        let requests = spawn_requests(client, inbox.clone());
        let shared = state.clone();
        let _ = thread::Builder::new().name("syncplay".into()).spawn(move || {
            Worker::new(player, local, poll, requests, shared).run(messages);
        });
        Ok(Arc::new(SyncSession { inbox, state }))
    }
}

/// A start or a pause that waits for its time.
#[derive(Clone, Copy)]
enum Scheduled {
    Unpause,
    PauseThenSeek(f64),
}

struct Worker {
    core: Core,
    clock: ServerClock,
    local: Arc<RwLock<LocalClock>>,
    player: Arc<dyn SyncPlayer>,
    poll: mpsc::Sender<Duration>,
    requests: mpsc::Sender<Request>,
    state: Arc<Mutex<Snapshot>>,
    /// The token and the playlist item of the load the group waits for.
    load: Option<(u64, String)>,
    next_load: u64,
    view: PlayerView,
    scheduled: Option<(Instant, Scheduled)>,
    connected: bool,
    /// A socket opened before. The app closes the socket in the
    /// background and opens a new one at the return, whose first open is
    /// not "again" for the socket, but is for the group.
    opened: bool,
}

impl Worker {
    fn new(
        player: Arc<dyn SyncPlayer>,
        local: Arc<RwLock<LocalClock>>,
        poll: mpsc::Sender<Duration>,
        requests: mpsc::Sender<Request>,
        state: Arc<Mutex<Snapshot>>,
    ) -> Self {
        let mut clock = ServerClock::default();
        clock.reset_for(read(&local).epoch);
        Self {
            core: Core::default(),
            clock,
            local,
            player,
            poll,
            requests,
            state,
            load: None,
            next_load: LOAD_TOKENS,
            view: PlayerView { paused: true, ..PlayerView::default() },
            scheduled: None,
            connected: true,
            opened: false,
        }
    }

    fn now(&self) -> f64 {
        read(&self.local).now()
    }

    fn run(mut self, messages: mpsc::Receiver<Message>) {
        let mut next_tick = Instant::now() + TICK;
        loop {
            let due = self.scheduled.map_or(next_tick, |(at, _)| at.min(next_tick));
            match messages.recv_timeout(due.saturating_duration_since(Instant::now())) {
                Ok(Message::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Leave a group on the way out; dropping `poll` and
                    // `requests` ends the helper threads (after the leave).
                    if self.core.in_group() {
                        self.feed(Input::User(Intent::Leave));
                        self.player.set_sync_speed(1.);
                    }
                    return;
                }
                Ok(message) => self.message(message),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some((at, action)) = self.scheduled
                && Instant::now() >= at
            {
                self.scheduled = None;
                match action {
                    Scheduled::Unpause => self.player.unpause(),
                    Scheduled::PauseThenSeek(secs) => self.player.pause_and_seek(secs),
                }
            }
            if Instant::now() >= next_tick {
                next_tick = Instant::now() + TICK;
                self.tick();
            }
        }
    }

    fn message(&mut self, message: Message) {
        match message {
            Message::Socket(SocketEvent::Open { again }) => {
                self.connected = true;
                if again || self.opened {
                    self.feed(Input::Reconnected);
                }
                self.opened = true;
            }
            Message::Socket(SocketEvent::Closed) => self.connected = false,
            Message::Socket(SocketEvent::Message { kind, data }) => {
                if let Some(message) = protocol::parse(&kind, &data) {
                    log::debug!("syncplay: server {message:?}");
                    self.feed(Input::Server(message));
                }
            }
            Message::Player(PlayerEvent::Loaded { playlist_item_id: token }) => {
                if let Some(id) = self.loaded(&token) {
                    self.feed(Input::Player(PlayerEvent::Loaded { playlist_item_id: id }));
                }
            }
            Message::Player(PlayerEvent::LoadFailed { playlist_item_id: token }) => {
                if let Some(id) = self.loaded(&token) {
                    self.feed(Input::Player(PlayerEvent::LoadFailed { playlist_item_id: id }));
                }
            }
            Message::Player(event) => self.feed(Input::Player(event)),
            Message::Position { secs, paused, duration_secs, at } => {
                let at = read(&self.local).at(at);
                self.view = PlayerView { position: secs * 1000., at, paused, duration: duration_secs * 1000. };
                self.feed(Input::Player(PlayerEvent::Position));
            }
            Message::Closed => self.feed(Input::Player(PlayerEvent::Closed)),
            Message::User(intent) => self.feed(Input::User(intent)),
            Message::Exchange(exchange) => {
                self.clock.record(exchange);
                self.set_poll();
                self.feed(Input::ClockUpdated);
            }
            Message::Failed(what) => self.player.notice(SyncNotice::Failed { what: what.into() }),
            Message::Correct(on) => self.core.correct = on,
            // The drift correction sees the new clock at the next tick.
            Message::Delay(ms) => self.clock.extra_offset = -ms,
            Message::Shutdown => {}
        }
    }

    /// The playlist item of a load the player answers, when it is the load
    /// the group waits for (a replaced load's late answer does not count).
    fn loaded(&mut self, token: &str) -> Option<String> {
        let token: u64 = token.parse().ok()?;
        self.load.take_if(|(wanted, _)| *wanted == token).map(|(_, id)| id)
    }

    fn tick(&mut self) {
        // After a sleep of the device the local clock is off by the sleep.
        let slept = read(&self.local).slept();
        if slept {
            log::info!("syncplay: the device slept; the clock starts again");
            let fresh = LocalClock::new();
            *write(&self.local) = fresh;
            self.clock.reset_for(fresh.epoch);
            self.set_poll();
        }
        self.feed(Input::Tick);
    }

    fn set_poll(&self) {
        let _ = self.poll.send(self.clock.next_poll(self.core.in_group()));
    }

    fn feed(&mut self, input: Input) {
        let now = self.now();
        let actions = self.core.handle(input, now, &self.clock, &self.view);
        for action in actions {
            self.act(action);
        }
        {
            let mut s = lock(&self.state);
            s.group = self.core.group().cloned();
            s.following = self.core.following();
            s.queue = self.core.queue().cloned();
            s.phase = self.core.phase().into();
            s.clock_ms = self.clock.offset();
            s.drift_ms = self.core.drift_ms;
            s.ping_ms = self.clock.ping();
            s.socket_open = self.connected;
        }
    }

    fn instant(&self, local_ms: f64) -> Instant {
        read(&self.local).instant(local_ms)
    }

    fn act(&mut self, action: Action) {
        log::debug!("syncplay: {action:?}");
        match action {
            Action::Send(request) => {
                let _ = self.requests.send(request);
            }
            Action::Load { item_id, playlist_item_id, position } => {
                self.next_load += 1;
                self.load = Some((self.next_load, playlist_item_id));
                self.player.load(item_id, (position / 1000.).max(0.), self.next_load);
            }
            Action::SeekPaused { position, token } => self.player.seek_paused((position / 1000.).max(0.), token),
            Action::UnpauseAt(at) => self.scheduled = Some((self.instant(at), Scheduled::Unpause)),
            Action::PauseAt { at, position } => {
                self.scheduled = Some((self.instant(at), Scheduled::PauseThenSeek((position / 1000.).max(0.))));
            }
            Action::PauseNow => self.player.pause(),
            Action::CancelScheduled => self.scheduled = None,
            Action::SetSpeed(factor) => self.player.set_sync_speed(factor),
            Action::Stop => {
                self.scheduled = None;
                self.player.stop();
            }
            Action::Notice(notice) => {
                if matches!(notice, Notice::Joined(_) | Notice::Left) {
                    self.set_poll();
                }
                if matches!(notice, Notice::Left) {
                    self.scheduled = None;
                    self.player.set_sync_speed(1.);
                }
                self.player.notice(notice.into());
            }
        }
    }
}

/// Sends the requests one after another, in order; a request a later one
/// makes pointless is left out (the Mac's `spawn_requests`).
fn spawn_requests(client: Client, failed: mpsc::Sender<Message>) -> mpsc::Sender<Request> {
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
                if let Err(err) = client.syncplay_request(request.path(), request.body().as_ref()) {
                    log::warn!("syncplay: {} failed: {err:#}", request.path());
                    if let Some(what) = request.asked()
                        && failed.send(Message::Failed(what)).is_err()
                    {
                        return;
                    }
                }
            }
        }
    });
    tx
}

/// Asks the server for its time again and again (the Mac's `spawn_clock`).
fn spawn_clock(client: Client, local: Arc<RwLock<LocalClock>>, found: mpsc::Sender<Message>) -> mpsc::Sender<Duration> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct UtcTime {
        request_reception_time: String,
        response_transmission_time: String,
    }
    let (poll, polls) = mpsc::channel::<Duration>();
    let _ = thread::Builder::new().name("syncplay-clock".into()).spawn(move || {
        let mut wait = Duration::from_secs(1);
        loop {
            let clock = *read(&local);
            let sent = clock.now();
            let answer = client.utc_time::<UtcTime>();
            let received = clock.now();
            let asked = Instant::now();
            if let Ok(time) = answer {
                let times = protocol::parse_time(&time.request_reception_time)
                    .zip(protocol::parse_time(&time.response_transmission_time));
                if let Some((server_received, server_sent)) = times {
                    let exchange = Exchange { sent, server_received, server_sent, received, epoch: clock.epoch };
                    if found.send(Message::Exchange(exchange)).is_err() {
                        return;
                    }
                }
            }
            loop {
                match polls.recv_timeout(wait.saturating_sub(asked.elapsed())) {
                    Ok(new) => wait = new,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    });
    poll
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Silent;

    impl SyncPlayer for Silent {
        fn load(&self, _: String, _: f64, _: u64) {}
        fn seek_paused(&self, _: f64, _: u64) {}
        fn unpause(&self) {}
        fn pause(&self) {}
        fn pause_and_seek(&self, _: f64) {}
        fn set_sync_speed(&self, _: f64) {}
        fn stop(&self) {}
        fn notice(&self, _: SyncNotice) {}
    }

    fn worker() -> (Worker, mpsc::Receiver<Request>) {
        let (requests, sent) = mpsc::channel();
        let (poll, _) = mpsc::channel();
        let local = Arc::new(RwLock::new(LocalClock::new()));
        let worker = Worker::new(Arc::new(Silent), local, poll, requests, Arc::default());
        (worker, sent)
    }

    fn joined(worker: &mut Worker) {
        let data = json!({
            "GroupId": "g1", "Type": "GroupJoined",
            "Data": {
                "GroupId": "g1", "GroupName": "Film night", "State": "Paused",
                "Participants": ["ana"], "LastUpdatedAt": "2026-11-14T20:00:00Z",
            },
        });
        worker.message(Message::Socket(SocketEvent::Message { kind: "SyncPlayGroupUpdate".into(), data }));
        assert!(worker.core.in_group());
    }

    fn joins(sent: &mpsc::Receiver<Request>) -> usize {
        sent.try_iter().filter(|r| matches!(r, Request::Join { .. })).count()
    }

    /// The app's socket rests in the background and a new one opens at the
    /// return: its first open joins the group again.
    #[test]
    fn a_new_socket_joins_the_group_again() {
        let (mut worker, sent) = worker();
        worker.message(Message::Socket(SocketEvent::Open { again: false }));
        joined(&mut worker);
        assert_eq!(joins(&sent), 0);
        worker.message(Message::Socket(SocketEvent::Open { again: false }));
        assert_eq!(joins(&sent), 1);
        // A socket that opens again (the same one) joins once more.
        worker.message(Message::Socket(SocketEvent::Open { again: true }));
        assert_eq!(joins(&sent), 1);
    }

    #[test]
    fn the_delay_shows_in_the_clock() {
        let (mut worker, _sent) = worker();
        worker.message(Message::Delay(150.));
        worker.feed(Input::Tick);
        assert_eq!(worker.state.lock().unwrap().clock_ms, -150.);
    }
}
