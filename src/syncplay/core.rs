// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rules of a SyncPlay member, with no network and no player in them.
//!
//! [`Core::handle`] takes what happened (a message of the server, an event
//! of the player, an action of the user, the passing of time) and returns
//! what to do: requests to the server, commands for the player, notices for
//! the screen. The caller does the work. So every rule here runs in a test
//! with a script of inputs.
//!
//! The server is the authority. Its last command says where the group is:
//! "at the server time `when`, the position is `position`", and for an
//! unpause the position runs on from there. A local action of the user is a
//! request; the player follows the command that comes back.

use super::{
    clock::ServerClock,
    drift::{Correction, Drift},
    protocol::{
        Command, CommandKind, GroupInfo, GroupState, GroupUpdate, PlayQueue, PlayerReport,
        Request, ServerMessage, TICKS_PER_MS, format_time, same_id,
    },
};

/// The player may be this far from a position and count as "at" it. An exact
/// seek lands within a frame.
const AT_POSITION_MS: f64 = 60.;
/// Shortest time between "start the seek" and "start to play" of a catch-up.
const MIN_LEAD_MS: f64 = 500.;
const MAX_LEAD_MS: f64 = 6000.;
/// A deadline closer than this cannot be met; act at once.
const TOO_CLOSE_MS: f64 = 5.;
/// No correction of the position for this long after a start or a seek.
const HOLD_OFF_MS: f64 = 1500.;
/// No measurement of the position for this long after a speed correction.
const AFTER_BURST_MS: f64 = 1500.;
/// A `Ready` with no answer is sent again after this long.
const READY_AGAIN_MS: f64 = 5000.;
/// At most one `Buffering` in this time.
const BUFFERING_EVERY_MS: f64 = 5000.;
/// A local pause the server did not confirm in this time is undone.
const CONFIRM_MS: f64 = 2000.;
/// The group gets this long to name the next item after an item ended.
const ADVANCE_MS: f64 = 10_000.;
/// Seeks of the server to one position before this member steps aside.
const SEEK_LOOP: u32 = 3;
/// A seek of the user ends this far before the end of the item at the
/// latest; a seek to the end would end the item for the whole group.
const END_GUARD_MS: f64 = 1500.;

/// What the player does right now, as far as the caller knows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerView {
    /// Position in milliseconds, measured at `at` on the local clock.
    pub position: f64,
    pub at: f64,
    pub paused: bool,
    /// Length of the item in milliseconds; 0 when not known.
    pub duration: f64,
}

impl PlayerView {
    /// The position now, when the player runs since the measurement.
    fn position_at(&self, now: f64) -> f64 {
        if self.paused {
            self.position
        } else {
            self.position + (now - self.at)
        }
    }
}

/// Something the player reports.
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerEvent {
    /// The item is loaded (paused) and can play.
    Loaded { playlist_item_id: String },
    /// The item could not be loaded.
    LoadFailed { playlist_item_id: String },
    /// A seek the core asked for is done and the player can play.
    Settled { token: u64 },
    /// The player ran out of data while it played.
    Stalled,
    /// The player has data again after a stall.
    Recovered,
    /// A new measurement of the position is in the [`PlayerView`].
    Position,
    /// The item played to its end.
    Ended,
    /// The user closed the player.
    Closed,
}

/// Something the user wants.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    Create { name: String },
    Join { group_id: String },
    Leave,
    TogglePause,
    Pause,
    Unpause,
    /// Seek to a position in milliseconds.
    Seek(f64),
    Next,
    Previous,
    /// Play these items in the group, from one of them.
    Play { item_ids: Vec<String>, index: usize, start: f64 },
    /// Add items to the queue of the group; `next` puts them after the
    /// current item.
    Enqueue { item_ids: Vec<String>, next: bool },
    Remove { playlist_item_ids: Vec<String> },
    Move { playlist_item_id: String, new_index: usize },
    Jump { playlist_item_id: String },
    Repeat(String),
    Shuffle(String),
    /// Stop to follow the group, but stay a member.
    StopFollowing,
    /// Follow the group again.
    Follow,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Server(ServerMessage),
    Player(PlayerEvent),
    User(Intent),
    /// The clock has a new measurement.
    ClockUpdated,
    /// The socket was lost and is open again.
    Reconnected,
    /// Time passed; the caller sends this a few times a second.
    Tick,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Send(Request),
    /// Load an item, paused, at a position in milliseconds. The player
    /// answers with `Loaded` or `LoadFailed`.
    Load { item_id: String, playlist_item_id: String, position: f64 },
    /// Pause if needed and seek exactly; the player answers `Settled`.
    SeekPaused { position: f64, token: u64 },
    /// Start to play at a time on the local clock.
    UnpauseAt(f64),
    /// Pause at a time on the local clock, then go to a position.
    PauseAt { at: f64, position: f64 },
    /// Pause now; a provisional pause, before the server confirms it.
    PauseNow,
    /// Forget a start or pause that waits for its time.
    CancelScheduled,
    /// Speed as a factor of the speed the user chose.
    SetSpeed(f64),
    /// Stop the player and close it.
    Stop,
    Notice(Notice),
}

/// News for the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    Joined(String),
    Left,
    UserJoined(String),
    UserLeft(String),
    /// The state of the group changed, with the reason the server gave.
    State(GroupState, String),
    /// The queue of the group changed.
    Queue,
    /// The server refused something.
    Denied(String),
    /// This member stepped aside: the group does not wait for it any more.
    SteppedAside(&'static str),
}

/// What the player is in the middle of.
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    /// Nothing loaded, or the user does not follow.
    Idle,
    /// An item loads.
    Loading { playlist_item_id: String },
    /// Paused at the place the group is paused at, or waits for a command.
    Parked,
    /// A seek runs; then the player starts at `start` (local clock), or
    /// stays paused when there is no start.
    Seeking { token: u64, asked: f64, start: Option<f64> },
    /// A start waits for its time.
    Starting { at: f64 },
    /// Plays with the group.
    Playing { since: f64 },
    /// The item ended; the group names the next one.
    Advancing { since: f64 },
}

#[derive(Debug)]
pub struct Core {
    group: Option<GroupInfo>,
    following: bool,
    queue: Option<PlayQueue>,
    /// The last command of the server for the item of the queue.
    command: Option<Command>,
    /// A command that came before the clock had a measurement.
    held: Option<Command>,
    /// Playlist item the player has loaded.
    loaded: Option<String>,
    phase: Phase,
    /// The server waits for a `Ready` of this member.
    owes_ready: bool,
    ready_sent: Option<f64>,
    buffering_sent: Option<f64>,
    stalled: bool,
    /// A pause of the user that the server has not confirmed yet.
    provisional_pause: Option<f64>,
    drift: Drift,
    speed: f64,
    /// A speed correction runs until this time. Nothing is measured
    /// meanwhile: the position of the player reads wrong at another speed.
    burst_until: Option<f64>,
    /// No measurement before this time: after a correction the player needs
    /// a moment at normal speed.
    quiet_until: f64,
    /// Time the last seek took, for the lead of the next catch-up.
    seek_cost: f64,
    lead: f64,
    next_token: u64,
    /// Position of the last seek command of the server and how often it
    /// came in a row.
    seek_loop: (i64, u32),
    /// The difference to the group at the last measurement, for the screen.
    pub drift_ms: Option<f64>,
    /// Correct the position when it drifts; the user can switch it off.
    pub correct: bool,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            group: None,
            following: true,
            queue: None,
            command: None,
            held: None,
            loaded: None,
            phase: Phase::Idle,
            owes_ready: false,
            ready_sent: None,
            buffering_sent: None,
            stalled: false,
            provisional_pause: None,
            drift: Drift::default(),
            speed: 1.,
            burst_until: None,
            quiet_until: 0.,
            seek_cost: 200.,
            lead: MIN_LEAD_MS,
            next_token: 0,
            seek_loop: (0, 0),
            drift_ms: None,
            correct: true,
        }
    }
}

impl Core {
    pub fn group(&self) -> Option<&GroupInfo> {
        self.group.as_ref()
    }

    pub fn in_group(&self) -> bool {
        self.group.is_some()
    }

    pub fn following(&self) -> bool {
        self.group.is_some() && self.following
    }

    pub fn queue(&self) -> Option<&PlayQueue> {
        self.queue.as_ref()
    }

    /// The group plays (or is about to), by the last command.
    pub fn group_plays(&self) -> bool {
        self.command
            .as_ref()
            .is_some_and(|c| c.command == CommandKind::Unpause)
    }

    fn current_id(&self) -> Option<String> {
        self.queue
            .as_ref()
            .and_then(PlayQueue::current)
            .map(|item| item.playlist_item_id.clone())
    }

    /// Where the group is at a local time, by the last command.
    fn group_position(&self, at_local: f64, clock: &ServerClock) -> Option<f64> {
        let command = self.command.as_ref()?;
        Some(match command.command {
            CommandKind::Unpause => {
                command.position_ms() + (clock.to_server(at_local) - command.when).max(0.)
            }
            _ => command.position_ms(),
        })
    }

    /// The local time at which a group that plays is at a position. A seek
    /// lands on a frame, a little after the place asked for; a start from
    /// there must wait until the group is at that frame.
    fn time_of(&self, position: f64, clock: &ServerClock) -> Option<f64> {
        let command = self.command.as_ref()?;
        (command.command == CommandKind::Unpause)
            .then(|| clock.to_local(command.when + (position - command.position_ms())))
    }

    fn report(&self, now: f64, clock: &ServerClock, view: &PlayerView) -> Option<PlayerReport> {
        Some(PlayerReport {
            when: format_time(clock.to_server(now)),
            position_ticks: (view.position_at(now).max(0.) * TICKS_PER_MS).round() as i64,
            is_playing: !view.paused,
            playlist_item_id: self.loaded.clone()?,
        })
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    pub fn handle(
        &mut self,
        input: Input,
        now: f64,
        clock: &ServerClock,
        view: &PlayerView,
    ) -> Vec<Action> {
        let mut out = Vec::new();
        match input {
            Input::Server(ServerMessage::Group { group_id, update }) => {
                self.on_group(&group_id, update, now, clock, view, &mut out)
            }
            Input::Server(ServerMessage::Command(command)) => {
                self.on_command(command, now, clock, view, &mut out)
            }
            Input::Player(event) => self.on_player(event, now, clock, view, &mut out),
            Input::User(intent) => self.on_user(intent, now, view, &mut out),
            Input::ClockUpdated => {
                if self.group.is_some() {
                    out.push(Action::Send(Request::Ping { ms: clock.ping().round() as i64 }));
                }
                if let Some(command) = self.held.take() {
                    self.on_command(command, now, clock, view, &mut out);
                }
            }
            Input::Reconnected => {
                // The server dropped this member with the socket. A join of
                // the same group gets the state again.
                if let Some(group) = &self.group {
                    let group_id = group.group_id.clone();
                    self.forget_playback(&mut out);
                    out.push(Action::Send(Request::Join { group_id }));
                }
            }
            Input::Tick => self.on_tick(now, clock, view, &mut out),
        }
        out
    }

    /// Back to the normal speed, when a correction runs.
    fn end_burst(&mut self, out: &mut Vec<Action>) {
        self.burst_until = None;
        if self.speed != 1. {
            self.speed = 1.;
            out.push(Action::SetSpeed(1.));
        }
    }

    /// Ends every wait and correction; the next command starts fresh.
    fn forget_playback(&mut self, out: &mut Vec<Action>) {
        out.push(Action::CancelScheduled);
        self.end_burst(out);
        self.command = None;
        self.held = None;
        self.owes_ready = false;
        self.ready_sent = None;
        self.provisional_pause = None;
        self.stalled = false;
        self.drift.reset();
        self.drift_ms = None;
        self.seek_loop = (0, 0);
        if self.loaded.is_some() {
            self.phase = Phase::Parked;
        }
    }

    fn leave(&mut self, out: &mut Vec<Action>) {
        if self.group.take().is_none() {
            return;
        }
        self.forget_playback(out);
        self.queue = None;
        self.following = true;
        self.loaded = None;
        self.phase = Phase::Idle;
        out.push(Action::Notice(Notice::Left));
    }

    fn on_group(
        &mut self,
        group_id: &str,
        update: GroupUpdate,
        now: f64,
        clock: &ServerClock,
        view: &PlayerView,
        out: &mut Vec<Action>,
    ) {
        match update {
            GroupUpdate::Joined(info) => {
                let same = self.group.as_ref().is_some_and(|g| same_id(&g.group_id, &info.group_id));
                if !same {
                    self.leave(out);
                    self.loaded = None;
                    self.phase = Phase::Idle;
                }
                // A join of the group this member is in already (after a
                // lost socket, or to follow again) brings the queue again,
                // with the time it had: it must not count as old.
                if same {
                    self.queue = None;
                } else {
                    out.push(Action::Notice(Notice::Joined(info.group_name.clone())));
                }
                self.group = Some(info);
                self.following = true;
            }
            GroupUpdate::Denied(what) => out.push(Action::Notice(Notice::Denied(what))),
            // News of a group this member is not in: a late message after a
            // leave, or of the group before a switch.
            _ if !self.group.as_ref().is_some_and(|g| same_id(&g.group_id, group_id))
                && !matches!(update, GroupUpdate::NotInGroup) => {}
            GroupUpdate::Info(info) => self.group = Some(info),
            GroupUpdate::UserJoined(name) => {
                if let Some(group) = &mut self.group
                    && !group.participants.contains(&name)
                {
                    group.participants.push(name.clone());
                }
                out.push(Action::Notice(Notice::UserJoined(name)));
            }
            GroupUpdate::UserLeft(name) => {
                if let Some(group) = &mut self.group {
                    group.participants.retain(|n| *n != name);
                }
                out.push(Action::Notice(Notice::UserLeft(name)));
            }
            GroupUpdate::Left | GroupUpdate::NotInGroup => self.leave(out),
            GroupUpdate::State { state, reason } => {
                if let Some(group) = &mut self.group {
                    group.state = state;
                }
                if state != GroupState::Waiting && self.ready_sent.is_some() {
                    self.owes_ready = false;
                }
                out.push(Action::Notice(Notice::State(state, reason)));
            }
            GroupUpdate::Queue(queue) => self.on_queue(queue, now, clock, view, out),
        }
    }

    fn on_queue(
        &mut self,
        queue: PlayQueue,
        now: f64,
        clock: &ServerClock,
        view: &PlayerView,
        out: &mut Vec<Action>,
    ) {
        if self.queue.as_ref().is_some_and(|old| queue.last_update <= old.last_update) {
            return;
        }
        let restart = queue.reason == "NewPlaylist";
        let current = queue.current().cloned();
        // The position of the queue update, run on when the group plays.
        let position = queue.start_position_ticks as f64 / TICKS_PER_MS
            + if queue.is_playing {
                (clock.to_server(now) - queue.last_update).max(0.)
            } else {
                0.
            };
        // A command older than the queue is about the item before.
        if self.command.as_ref().is_some_and(|c| c.emitted_at < queue.last_update) {
            self.command = None;
        }
        self.queue = Some(queue);
        out.push(Action::Notice(Notice::Queue));
        if restart && !self.following {
            // A new queue of the group takes a member back in, as on the web.
            self.following = true;
            out.push(Action::Send(Request::SetIgnoreWait(false)));
        }
        if !self.following {
            return;
        }
        let Some(current) = current else {
            // The queue is empty or nothing plays.
            if self.loaded.take().is_some() || matches!(self.phase, Phase::Loading { .. }) {
                self.forget_playback(out);
                self.phase = Phase::Idle;
                out.push(Action::Stop);
            }
            return;
        };
        let loading = matches!(&self.phase, Phase::Loading { playlist_item_id } if same_id(playlist_item_id, &current.playlist_item_id));
        let loaded = self.loaded.as_ref().is_some_and(|id| same_id(id, &current.playlist_item_id));
        if loading {
            return;
        }
        if loaded && !restart {
            // The same item goes on; an edit of the queue around it.
            if matches!(self.phase, Phase::Advancing { .. }) {
                self.phase = Phase::Parked;
            }
            return;
        }
        let position = self.group_position(now, clock).unwrap_or(position);
        out.push(Action::CancelScheduled);
        self.drift.reset();
        self.owes_ready = true;
        self.ready_sent = None;
        self.provisional_pause = None;
        if loaded {
            // The group starts the item again: go to its place and report.
            let token = self.token();
            self.phase = Phase::Seeking { token, asked: now, start: None };
            out.push(Action::SeekPaused { position, token });
        } else {
            self.loaded = None;
            self.phase = Phase::Loading { playlist_item_id: current.playlist_item_id.clone() };
            out.push(Action::Load {
                item_id: current.item_id,
                playlist_item_id: current.playlist_item_id,
                position,
            });
        }
        let _ = view;
    }

    fn on_command(
        &mut self,
        command: Command,
        now: f64,
        clock: &ServerClock,
        view: &PlayerView,
        out: &mut Vec<Action>,
    ) {
        let Some(group) = &self.group else { return };
        if !same_id(&group.group_id, &command.group_id) || command.emitted_at < group.last_updated_at {
            return;
        }
        if !clock.ready() {
            self.held = Some(command);
            return;
        }
        if command.command == CommandKind::Stop {
            self.forget_playback(out);
            self.loaded = None;
            self.phase = Phase::Idle;
            out.push(Action::Stop);
            return;
        }
        let duplicate = self.command.as_ref().is_some_and(|last| last.same_as(&command));
        // The seek loop: the server sends a seek to one member whose `Ready`
        // was too far from the group.
        if command.command == CommandKind::Seek {
            let ticks = command.position_ticks.unwrap_or(0);
            self.seek_loop = if self.seek_loop.0 == ticks {
                (ticks, self.seek_loop.1 + 1)
            } else {
                (ticks, 1)
            };
        } else {
            self.seek_loop = (0, 0);
        }
        self.provisional_pause = None;
        // A start or a pause after a `Ready` shows the server took it.
        if command.command != CommandKind::Seek && self.ready_sent.is_some() {
            self.owes_ready = false;
        }
        self.command = Some(command.clone());
        if !self.following {
            return;
        }
        // A command for an item that is not loaded yet waits in
        // `self.command`; the load applies it.
        if !self.loaded.as_ref().is_some_and(|id| same_id(id, &command.playlist_item_id)) {
            return;
        }
        if self.seek_loop.1 >= SEEK_LOOP {
            self.step_aside("The player cannot reach the position of the group.", out);
            return;
        }
        if duplicate && self.consistent(now, clock, view) {
            return;
        }
        self.apply(now, clock, view, out);
    }

    /// True when the player already does what the last command says.
    fn consistent(&self, now: f64, clock: &ServerClock, view: &PlayerView) -> bool {
        let Some(command) = &self.command else { return true };
        match command.command {
            CommandKind::Unpause => matches!(
                self.phase,
                Phase::Playing { .. } | Phase::Starting { .. } | Phase::Seeking { start: Some(_), .. }
            ),
            CommandKind::Pause | CommandKind::Seek => {
                let _ = clock;
                view.paused
                    && (view.position_at(now) - command.position_ms()).abs() <= AT_POSITION_MS
                    && !matches!(self.phase, Phase::Seeking { .. })
                    && !self.owes_ready
            }
            CommandKind::Stop => true,
        }
    }

    /// Makes the player do what the last command says.
    fn apply(&mut self, now: f64, clock: &ServerClock, view: &PlayerView, out: &mut Vec<Action>) {
        let Some(command) = self.command.clone() else { return };
        let when = clock.to_local(command.when);
        out.push(Action::CancelScheduled);
        self.drift.reset();
        self.drift_ms = None;
        self.end_burst(out);
        match command.command {
            CommandKind::Unpause => {
                let at_start = view.paused
                    && (view.position - command.position_ms()).abs() <= AT_POSITION_MS;
                let when = if at_start {
                    self.time_of(view.position, clock).unwrap_or(when)
                } else {
                    when
                };
                if at_start && when > now + TOO_CLOSE_MS {
                    self.phase = Phase::Starting { at: when };
                    out.push(Action::UnpauseAt(when));
                } else {
                    self.catch_up(now, clock, out);
                }
            }
            CommandKind::Pause => {
                self.phase = Phase::Parked;
                out.push(Action::PauseAt { at: when.max(now), position: command.position_ms() });
            }
            CommandKind::Seek => {
                let token = self.token();
                self.owes_ready = true;
                self.ready_sent = None;
                self.phase = Phase::Seeking { token, asked: now, start: None };
                out.push(Action::SeekPaused { position: command.position_ms(), token });
            }
            CommandKind::Stop => {}
        }
    }

    /// Joins a group that plays: seek, paused, to the place the group will
    /// be at a little later, and start to play at that time. A seek to "the
    /// place the group is at now" would arrive late by the time of the seek.
    fn catch_up(&mut self, now: f64, clock: &ServerClock, out: &mut Vec<Action>) {
        let Some(command) = &self.command else { return };
        let lead = self.lead.max(self.seek_cost * 1.5).clamp(MIN_LEAD_MS, MAX_LEAD_MS);
        let start = (now + lead).max(clock.to_local(command.when));
        let Some(position) = self.group_position(start, clock) else { return };
        let token = self.token();
        self.phase = Phase::Seeking { token, asked: now, start: Some(start) };
        out.push(Action::SeekPaused { position, token });
    }

    fn step_aside(&mut self, why: &'static str, out: &mut Vec<Action>) {
        self.following = false;
        self.forget_playback(out);
        out.push(Action::Send(Request::SetIgnoreWait(true)));
        out.push(Action::Notice(Notice::SteppedAside(why)));
    }

    fn send_ready(&mut self, now: f64, clock: &ServerClock, view: &PlayerView, out: &mut Vec<Action>) {
        if let Some(report) = self.report(now, clock, view) {
            self.ready_sent = Some(now);
            out.push(Action::Send(Request::Ready(report)));
        }
    }

    fn on_player(
        &mut self,
        event: PlayerEvent,
        now: f64,
        clock: &ServerClock,
        view: &PlayerView,
        out: &mut Vec<Action>,
    ) {
        if self.group.is_none() {
            return;
        }
        match event {
            PlayerEvent::Loaded { playlist_item_id } => {
                // A load the queue moved on from: the newer load answers.
                if !matches!(&self.phase, Phase::Loading { playlist_item_id: wanted } if same_id(wanted, &playlist_item_id))
                {
                    return;
                }
                self.loaded = Some(playlist_item_id.clone());
                self.phase = Phase::Parked;
                self.send_ready(now, clock, view, out);
                // A command that came during the load, such as the start
                // the server forces after its wait.
                let waiting = self
                    .command
                    .as_ref()
                    .is_some_and(|c| same_id(&c.playlist_item_id, &playlist_item_id));
                if waiting && !self.consistent(now, clock, view) {
                    self.apply(now, clock, view, out);
                }
            }
            PlayerEvent::LoadFailed { playlist_item_id } => {
                if matches!(&self.phase, Phase::Loading { playlist_item_id: wanted } if same_id(wanted, &playlist_item_id))
                {
                    self.phase = Phase::Idle;
                    self.step_aside("The item could not be played.", out);
                }
            }
            PlayerEvent::Settled { token } => {
                let Phase::Seeking { token: wanted, asked, start } = self.phase.clone() else {
                    return;
                };
                if wanted != token {
                    return;
                }
                self.seek_cost = (now - asked).max(0.);
                match start {
                    None => {
                        self.phase = Phase::Parked;
                        if self.owes_ready {
                            self.send_ready(now, clock, view, out);
                        }
                        // An unpause that came during the seek.
                        if self.group_plays() && !self.owes_ready {
                            self.apply(now, clock, view, out);
                        }
                    }
                    Some(at)
                        if self.time_of(view.position, clock).unwrap_or(at) > now + TOO_CLOSE_MS =>
                    {
                        // The time the group is at the frame the seek
                        // landed on.
                        let at = self.time_of(view.position, clock).unwrap_or(at);
                        self.lead = MIN_LEAD_MS;
                        self.phase = Phase::Starting { at };
                        out.push(Action::UnpauseAt(at));
                    }
                    // The seek took longer than its lead: again, with more.
                    Some(_) => {
                        self.lead = (self.lead * 2.).min(MAX_LEAD_MS);
                        self.catch_up(now, clock, out);
                    }
                }
            }
            PlayerEvent::Stalled => {
                self.stalled = true;
                let in_step = matches!(self.phase, Phase::Playing { since } if now - since >= HOLD_OFF_MS);
                let recent = self.buffering_sent.is_some_and(|at| now - at < BUFFERING_EVERY_MS);
                if in_step && self.following && !recent {
                    if let Some(report) = self.report(now, clock, view) {
                        self.buffering_sent = Some(now);
                        self.owes_ready = true;
                        self.ready_sent = None;
                        out.push(Action::Send(Request::Buffering(report)));
                    }
                }
            }
            PlayerEvent::Recovered => {
                self.stalled = false;
                self.drift.reset();
                if self.owes_ready && self.following {
                    self.send_ready(now, clock, view, out);
                }
            }
            PlayerEvent::Position => self.on_position(now, clock, view, out),
            PlayerEvent::Ended => {
                if !self.following {
                    return;
                }
                if let Some(playlist_item_id) = self.loaded.clone() {
                    self.forget_playback(out);
                    // As the web client: with no next item it asks nothing
                    // and stops at once. The member stays in the group and
                    // follows it.
                    if self.queue.as_ref().is_some_and(|q| !q.has_next()) {
                        self.loaded = None;
                        self.phase = Phase::Idle;
                        out.push(Action::Stop);
                        return;
                    }
                    self.phase = Phase::Advancing { since: now };
                    out.push(Action::Send(Request::NextItem { playlist_item_id }));
                }
            }
            PlayerEvent::Closed => {
                // The user closed the player: stay in the group, but do not
                // hold it up.
                if self.following && self.loaded.is_some() {
                    self.following = false;
                    self.forget_playback(out);
                    out.push(Action::Send(Request::SetIgnoreWait(true)));
                }
                self.loaded = None;
                self.phase = Phase::Idle;
            }
        }
    }

    fn on_position(&mut self, now: f64, clock: &ServerClock, view: &PlayerView, out: &mut Vec<Action>) {
        // A start that waited for its time has happened.
        if let Phase::Starting { at } = self.phase
            && !view.paused
            && now >= at
        {
            self.phase = Phase::Playing { since: at };
        }
        let Phase::Playing { since } = self.phase else { return };
        if !self.following || !self.group_plays() || self.stalled || view.paused {
            return;
        }
        // A correction runs its time; then the measurements start again.
        if let Some(until) = self.burst_until {
            if now >= until {
                self.end_burst(out);
                self.drift.reset();
                self.quiet_until = now + AFTER_BURST_MS;
            }
            return;
        }
        if now < self.quiet_until {
            return;
        }
        let Some(group) = self.group_position(view.at, clock) else { return };
        let diff = group - view.position;
        self.drift_ms = Some(diff);
        if now - since < HOLD_OFF_MS || !self.correct {
            return;
        }
        match self.drift.measure(diff) {
            Some(Correction::Speed { factor, for_ms }) => {
                self.speed = factor;
                self.burst_until = Some(now + for_ms);
                // The number is of no use until it is measured again.
                self.drift_ms = None;
                out.push(Action::SetSpeed(factor));
            }
            Some(Correction::Jump) => {
                out.push(Action::CancelScheduled);
                self.catch_up(now, clock, out);
            }
            None => {}
        }
    }

    fn on_user(&mut self, intent: Intent, now: f64, view: &PlayerView, out: &mut Vec<Action>) {
        let send = |out: &mut Vec<Action>, request| out.push(Action::Send(request));
        match intent {
            Intent::Create { name } => return send(out, Request::New { name }),
            Intent::Join { group_id } => return send(out, Request::Join { group_id }),
            _ if self.group.is_none() => return,
            Intent::Leave => send(out, Request::Leave),
            Intent::TogglePause => {
                let intent = if self.group_plays() { Intent::Pause } else { Intent::Unpause };
                self.on_user(intent, now, view, out);
            }
            Intent::Pause => {
                send(out, Request::Pause);
                // The user sees the pause at once; the command of the
                // server then puts every member at one position.
                if self.following && !view.paused {
                    self.provisional_pause = Some(now);
                    out.push(Action::CancelScheduled);
                    out.push(Action::PauseNow);
                }
            }
            Intent::Unpause => send(out, Request::Unpause),
            Intent::Seek(position) => {
                let last = if view.duration > END_GUARD_MS { view.duration - END_GUARD_MS } else { f64::MAX };
                let ticks = (position.clamp(0., last) * TICKS_PER_MS).round() as i64;
                send(out, Request::Seek { position_ticks: ticks });
            }
            Intent::Next | Intent::Previous => {
                // The server needs the item this member is at; with another
                // id it leaves the group in a wait.
                let Some(playlist_item_id) = self.current_id() else { return };
                send(
                    out,
                    if intent == Intent::Next {
                        Request::NextItem { playlist_item_id }
                    } else {
                        Request::PreviousItem { playlist_item_id }
                    },
                );
            }
            Intent::Play { item_ids, index, start } => {
                if item_ids.is_empty() || index >= item_ids.len() {
                    return;
                }
                send(
                    out,
                    Request::SetNewQueue {
                        item_ids,
                        index,
                        start_ticks: (start.max(0.) * TICKS_PER_MS).round() as i64,
                    },
                );
            }
            Intent::Enqueue { item_ids, next } => {
                if !item_ids.is_empty() {
                    send(out, Request::Queue { item_ids, next });
                }
            }
            Intent::Remove { playlist_item_ids } => {
                send(out, Request::RemoveFromPlaylist { playlist_item_ids })
            }
            Intent::Move { playlist_item_id, new_index } => {
                send(out, Request::MovePlaylistItem { playlist_item_id, new_index })
            }
            Intent::Jump { playlist_item_id } => {
                send(out, Request::SetPlaylistItem { playlist_item_id })
            }
            Intent::Repeat(mode) => send(out, Request::SetRepeatMode(mode)),
            Intent::Shuffle(mode) => send(out, Request::SetShuffleMode(mode)),
            Intent::StopFollowing => {
                if self.following {
                    self.following = false;
                    self.forget_playback(out);
                    self.loaded = None;
                    self.phase = Phase::Idle;
                    send(out, Request::SetIgnoreWait(true));
                    out.push(Action::Stop);
                }
            }
            Intent::Follow => {
                if self.following {
                    return;
                }
                self.following = true;
                send(out, Request::SetIgnoreWait(false));
                // The server sends the state again for a join of the group
                // this member is in.
                if let Some(group) = &self.group {
                    send(out, Request::Join { group_id: group.group_id.clone() });
                }
            }
        }
    }

    fn on_tick(&mut self, now: f64, clock: &ServerClock, view: &PlayerView, out: &mut Vec<Action>) {
        if self.group.is_none() {
            return;
        }
        // A pause of the user that the server did not confirm: the request
        // was lost or refused, so the player goes back to the group.
        if self.provisional_pause.is_some_and(|at| now - at > CONFIRM_MS) {
            self.provisional_pause = None;
            if self.following && self.group_plays() && self.loaded.is_some() {
                self.apply(now, clock, view, out);
            }
        }
        // The group still waits though this member said it is ready: the
        // request was lost.
        let waits = self.group.as_ref().is_some_and(|g| g.state == GroupState::Waiting);
        if self.owes_ready
            && self.following
            && waits
            && matches!(self.phase, Phase::Parked)
            && self.ready_sent.is_some_and(|at| now - at > READY_AGAIN_MS)
        {
            self.send_ready(now, clock, view, out);
        }
        if let Phase::Advancing { since } = self.phase
            && now - since > ADVANCE_MS
        {
            // The group named no next item: the queue is at its end.
            self.loaded = None;
            self.phase = Phase::Idle;
            out.push(Action::Stop);
        }
    }

    /// The server took the `Ready`: a command or a state after it shows the
    /// group moved on.
    #[cfg(test)]
    fn ready_taken(&mut self) {
        self.owes_ready = false;
    }

    /// Short text of what the player is in the middle of, for the debug
    /// channel.
    pub fn phase_name(&self) -> &'static str {
        match self.phase {
            Phase::Idle => "idle",
            Phase::Loading { .. } => "loading",
            Phase::Parked => "parked",
            Phase::Seeking { start: None, .. } => "seeking",
            Phase::Seeking { start: Some(_), .. } => "catching-up",
            Phase::Starting { .. } => "starting",
            Phase::Playing { .. } => "playing",
            Phase::Advancing { .. } => "advancing",
        }
    }

    /// Playlist item the player has loaded.
    pub fn loaded(&self) -> Option<&str> {
        self.loaded.as_deref()
    }

    /// The speed factor of the correction that runs.
    pub fn speed(&self) -> f64 {
        self.speed
    }
}

#[cfg(test)]
mod tests {
    use super::super::clock::Exchange;
    use super::*;

    const GROUP: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f9";
    /// The server clock is this far ahead of the local one in the tests.
    const OFFSET: f64 = 37_000.;

    struct Rig {
        core: Core,
        clock: ServerClock,
        view: PlayerView,
        now: f64,
    }

    impl Rig {
        fn new() -> Self {
            let mut clock = ServerClock::default();
            clock.record(Exchange {
                sent: 0.,
                server_received: OFFSET + 5.,
                server_sent: OFFSET + 5.,
                received: 10.,
            });
            Self {
                core: Core::default(),
                clock,
                view: PlayerView { paused: true, duration: 3_600_000., ..Default::default() },
                now: 100_000.,
            }
        }

        fn server_now(&self) -> f64 {
            self.clock.to_server(self.now)
        }

        fn feed(&mut self, input: Input) -> Vec<Action> {
            self.view.at = self.now;
            let out = self.core.handle(input, self.now, &self.clock, &self.view);
            // The rig plays the part of the player for the simple commands.
            for action in &out {
                match action {
                    Action::PauseNow => self.view.paused = true,
                    Action::PauseAt { position, .. } => {
                        self.view.paused = true;
                        self.view.position = *position;
                    }
                    Action::SeekPaused { position, .. } => {
                        self.view.paused = true;
                        self.view.position = *position;
                    }
                    Action::Load { position, .. } => {
                        self.view.paused = true;
                        self.view.position = *position;
                    }
                    _ => {}
                }
            }
            out
        }

        fn group(&mut self, update: GroupUpdate) -> Vec<Action> {
            self.feed(Input::Server(ServerMessage::Group { group_id: GROUP.into(), update }))
        }

        fn join(&mut self) -> Vec<Action> {
            let info = GroupInfo {
                group_id: GROUP.into(),
                group_name: "Film night".into(),
                state: GroupState::Paused,
                participants: vec!["ana".into()],
                last_updated_at: self.server_now() - 1.,
            };
            self.group(GroupUpdate::Joined(info))
        }

        fn queue(&mut self, reason: &str, items: &[&str], index: i64, position: f64, playing: bool) -> Vec<Action> {
            let queue = PlayQueue {
                reason: reason.into(),
                last_update: self.server_now(),
                playlist: items
                    .iter()
                    .map(|id| super::super::protocol::QueueItem {
                        item_id: format!("item-{id}"),
                        playlist_item_id: (*id).to_string(),
                    })
                    .collect(),
                playing_item_index: index,
                start_position_ticks: (position * TICKS_PER_MS) as i64,
                is_playing: playing,
                shuffle_mode: "Sorted".into(),
                repeat_mode: "RepeatNone".into(),
            };
            self.group(GroupUpdate::Queue(queue))
        }

        /// A command of the server for the playlist item, to run `in_ms`
        /// from now.
        fn command(&mut self, kind: CommandKind, item: &str, position: f64, in_ms: f64) -> Vec<Action> {
            let command = Command {
                group_id: GROUP.into(),
                playlist_item_id: item.into(),
                when: self.server_now() + in_ms,
                position_ticks: Some((position * TICKS_PER_MS) as i64),
                command: kind,
                emitted_at: self.server_now(),
            };
            self.feed(Input::Server(ServerMessage::Command(command)))
        }

        fn player(&mut self, event: PlayerEvent) -> Vec<Action> {
            self.feed(Input::Player(event))
        }

        fn user(&mut self, intent: Intent) -> Vec<Action> {
            self.feed(Input::User(intent))
        }

        fn pass(&mut self, ms: f64) {
            if !self.view.paused {
                self.view.position += ms;
            }
            self.now += ms;
        }

        /// Joins, takes the queue with one item paused at a position, and
        /// loads it.
        fn parked(position: f64) -> Self {
            Self::parked_with(&["p1", "p2"], position)
        }

        fn parked_with(items: &[&str], position: f64) -> Self {
            let mut rig = Self::new();
            rig.join();
            rig.queue("NewPlaylist", items, 0, position, false);
            rig.player(PlayerEvent::Loaded { playlist_item_id: "p1".into() });
            rig.core.ready_taken();
            rig
        }

        /// A member that plays in step with the group.
        fn playing(position: f64) -> Self {
            Self::playing_with(&["p1", "p2"], position)
        }

        fn playing_with(items: &[&str], position: f64) -> Self {
            let mut rig = Self::parked_with(items, position);
            let out = rig.command(CommandKind::Unpause, "p1", position, 600.);
            assert_eq!(out.last(), Some(&Action::UnpauseAt(rig.now + 600.)));
            rig.pass(600.);
            rig.view.paused = false;
            rig.player(PlayerEvent::Position);
            rig
        }
    }

    fn sent(out: &[Action]) -> Vec<&Request> {
        out.iter()
            .filter_map(|a| match a {
                Action::Send(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    fn seek_of(out: &[Action]) -> Option<(f64, u64)> {
        out.iter().find_map(|a| match a {
            Action::SeekPaused { position, token } => Some((*position, *token)),
            _ => None,
        })
    }

    #[test]
    fn a_new_queue_loads_the_item_paused_and_reports_ready() {
        let mut rig = Rig::new();
        rig.join();
        let out = rig.queue("NewPlaylist", &["p1", "p2"], 1, 90_000., false);
        assert!(out.contains(&Action::Load {
            item_id: "item-p2".into(),
            playlist_item_id: "p2".into(),
            position: 90_000.,
        }));
        let out = rig.player(PlayerEvent::Loaded { playlist_item_id: "p2".into() });
        let [Request::Ready(report)] = sent(&out)[..] else { panic!("{out:?}") };
        assert_eq!(report.playlist_item_id, "p2");
        assert_eq!(report.position_ticks, 900_000_000);
        assert!(!report.is_playing);
        // The time of the report is on the server clock.
        assert_eq!(report.when, format_time(rig.server_now()));
    }

    #[test]
    fn an_unpause_ahead_is_scheduled_on_the_local_clock() {
        let mut rig = Rig::parked(5_000.);
        let out = rig.command(CommandKind::Unpause, "p1", 5_000., 800.);
        assert_eq!(out.last(), Some(&Action::UnpauseAt(rig.now + 800.)));
        assert!(seek_of(&out).is_none());
    }

    #[test]
    fn joining_a_group_that_plays_seeks_ahead_and_starts_on_time() {
        let mut rig = Rig::new();
        rig.join();
        // The group plays; it was at 60 s when the queue update was made.
        rig.queue("NewPlaylist", &["p1"], 0, 60_000., true);
        rig.pass(300.);
        rig.player(PlayerEvent::Loaded { playlist_item_id: "p1".into() });
        rig.core.ready_taken();
        // The server names a start that is already 2 s in the past.
        let out = rig.command(CommandKind::Unpause, "p1", 61_000., -2_000.);
        let (position, token) = seek_of(&out).expect("a seek");
        // The group is at 63 s now; the seek goes to where it is after the
        // lead, and the start is at that time.
        assert!((position - (63_000. + 500.)).abs() < 1e-6, "{position}");
        rig.pass(200.);
        let out = rig.player(PlayerEvent::Settled { token });
        assert_eq!(out, vec![Action::UnpauseAt(rig.now + 300.)]);
    }

    #[test]
    fn the_start_waits_for_the_frame_the_seek_landed_on() {
        let mut rig = Rig::parked(0.);
        let out = rig.command(CommandKind::Unpause, "p1", 10_000., -1_000.);
        let (position, token) = seek_of(&out).unwrap();
        rig.pass(200.);
        // The seek landed 30 ms after the place asked for.
        rig.view.position = position + 30.;
        let out = rig.player(PlayerEvent::Settled { token });
        assert_eq!(out, vec![Action::UnpauseAt(rig.now + 300. + 30.)]);
    }

    #[test]
    fn a_seek_slower_than_its_lead_is_tried_again_with_more_lead() {
        let mut rig = Rig::parked(0.);
        let out = rig.command(CommandKind::Unpause, "p1", 10_000., -1_000.);
        let (first, token) = seek_of(&out).unwrap();
        rig.pass(900.);
        let out = rig.player(PlayerEvent::Settled { token });
        let (second, _) = seek_of(&out).expect("a second seek");
        // 900 ms later, and with a lead of 1.5 times the cost of the seek.
        assert!((second - first - (900. - 500. + 1350.)).abs() < 1e-6, "{first} {second}");
    }

    #[test]
    fn a_pause_runs_at_its_time_and_parks_at_the_position() {
        let mut rig = Rig::playing(5_000.);
        rig.pass(3_000.);
        let out = rig.command(CommandKind::Pause, "p1", 8_000., 0.);
        assert!(out.contains(&Action::PauseAt { at: rig.now, position: 8_000. }));
        // The same command again changes nothing.
        let out = rig.command(CommandKind::Pause, "p1", 8_000., 0.);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_seek_command_parks_and_reports_ready_when_settled() {
        let mut rig = Rig::playing(5_000.);
        let out = rig.command(CommandKind::Seek, "p1", 600_000., 0.);
        let (position, token) = seek_of(&out).unwrap();
        assert_eq!(position, 600_000.);
        rig.pass(250.);
        let out = rig.player(PlayerEvent::Settled { token });
        let [Request::Ready(report)] = sent(&out)[..] else { panic!("{out:?}") };
        assert_eq!(report.position_ticks, 6_000_000_000);
        assert!(!report.is_playing);
    }

    #[test]
    fn commands_from_before_the_join_and_of_other_groups_are_dropped() {
        let mut rig = Rig::parked(0.);
        let mut old = Command {
            group_id: GROUP.into(),
            playlist_item_id: "p1".into(),
            when: rig.server_now(),
            position_ticks: Some(0),
            command: CommandKind::Unpause,
            emitted_at: rig.server_now() - 60_000.,
        };
        assert!(rig.feed(Input::Server(ServerMessage::Command(old.clone()))).is_empty());
        old.emitted_at = rig.server_now();
        old.group_id = "another".into();
        assert!(rig.feed(Input::Server(ServerMessage::Command(old))).is_empty());
        assert!(!rig.core.group_plays());
    }

    #[test]
    fn a_command_for_an_item_that_still_loads_runs_after_the_load() {
        let mut rig = Rig::new();
        rig.join();
        rig.queue("NewPlaylist", &["p1"], 0, 0., false);
        // The server gave up its wait and started the group.
        rig.pass(31_000.);
        let out = rig.command(CommandKind::Unpause, "p1", 0., 500.);
        assert!(out.is_empty(), "{out:?}");
        rig.pass(4_000.);
        let out = rig.player(PlayerEvent::Loaded { playlist_item_id: "p1".into() });
        assert!(matches!(sent(&out)[..], [Request::Ready(_)]));
        // The group is 3.5 s in; the member seeks ahead of it.
        let (position, _) = seek_of(&out).expect("a catch-up");
        assert!((position - 4_000.).abs() < 1e-6, "{position}");
    }

    #[test]
    fn a_second_queue_change_during_a_load_wins() {
        let mut rig = Rig::new();
        rig.join();
        rig.queue("NewPlaylist", &["p1", "p2"], 0, 0., false);
        rig.pass(50.);
        let out = rig.queue("NextItem", &["p1", "p2"], 1, 0., false);
        assert!(out.iter().any(|a| matches!(a, Action::Load { playlist_item_id, .. } if playlist_item_id == "p2")));
        // The first load ends late; its answer is not for the current item.
        let out = rig.player(PlayerEvent::Loaded { playlist_item_id: "p1".into() });
        assert!(out.is_empty(), "{out:?}");
        let out = rig.player(PlayerEvent::Loaded { playlist_item_id: "p2".into() });
        let [Request::Ready(report)] = sent(&out)[..] else { panic!("{out:?}") };
        assert_eq!(report.playlist_item_id, "p2");
    }

    #[test]
    fn an_older_queue_update_is_ignored() {
        let mut rig = Rig::parked(0.);
        rig.now -= 5_000.;
        let out = rig.queue("NextItem", &["p1", "p2"], 1, 0., false);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn the_user_pauses_at_once_and_the_request_goes_out() {
        let mut rig = Rig::playing(0.);
        let out = rig.user(Intent::TogglePause);
        assert_eq!(sent(&out), [&Request::Pause]);
        assert!(out.contains(&Action::PauseNow));
        // The toggle follows the group, not the player: paused locally, but
        // the group still plays until the server says otherwise.
        let out = rig.user(Intent::TogglePause);
        assert_eq!(sent(&out), [&Request::Pause]);
    }

    #[test]
    fn a_pause_the_server_does_not_confirm_is_undone() {
        let mut rig = Rig::playing(0.);
        rig.pass(5_000.);
        rig.user(Intent::Pause);
        rig.pass(2_100.);
        let out = rig.feed(Input::Tick);
        // Back to the group: it is 2.1 s on, so a seek ahead and a start.
        let (position, _) = seek_of(&out).expect("a catch-up");
        assert!((position - (7_100. + 500.)).abs() < 1e-6, "{position}");
    }

    #[test]
    fn user_actions_become_requests_with_the_current_item() {
        let mut rig = Rig::playing(0.);
        assert_eq!(
            sent(&rig.user(Intent::Seek(42_000.))),
            [&Request::Seek { position_ticks: 420_000_000 }]
        );
        // A seek to the end stops short of it.
        assert_eq!(
            sent(&rig.user(Intent::Seek(9_999_999.))),
            [&Request::Seek { position_ticks: (3_600_000. - 1_500.) as i64 * 10_000 }]
        );
        assert_eq!(
            sent(&rig.user(Intent::Next)),
            [&Request::NextItem { playlist_item_id: "p1".into() }]
        );
        assert_eq!(
            sent(&rig.user(Intent::Play { item_ids: vec!["a".into(), "b".into()], index: 1, start: 1_000. })),
            [&Request::SetNewQueue { item_ids: vec!["a".into(), "b".into()], index: 1, start_ticks: 10_000_000 }]
        );
        // Outside a group only create and join do something.
        let mut alone = Rig::new();
        assert!(alone.user(Intent::Pause).is_empty());
        assert_eq!(sent(&alone.user(Intent::Create { name: "x".into() })).len(), 1);
    }

    #[test]
    fn drift_is_closed_by_a_time_at_another_speed() {
        let mut rig = Rig::playing(0.);
        rig.pass(2_000.);
        // The player fell 90 ms behind the group.
        rig.view.position -= 90.;
        let mut speeds = Vec::new();
        let mut step = |rig: &mut Rig, speeds: &mut Vec<(f64, f64)>| {
            rig.pass(100.);
            // The player runs at the speed of the correction.
            rig.view.position += 100. * (rig.core.speed() - 1.);
            for action in rig.player(PlayerEvent::Position) {
                if let Action::SetSpeed(speed) = action {
                    speeds.push((speed, rig.now));
                }
            }
        };
        for _ in 0..40 {
            step(&mut rig, &mut speeds);
        }
        // One correction: 4.5% faster for 2 s closes 90 ms, then normal speed.
        let [(up, started), (down, ended)] = speeds[..] else { panic!("{speeds:?}") };
        assert_eq!((up, down), (1.045, 1.0));
        assert!((ended - started - 2_000.).abs() <= 100., "{}", ended - started);
        // Measured again after the correction: in step.
        for _ in 0..20 {
            step(&mut rig, &mut speeds);
        }
        assert_eq!(speeds.len(), 2);
        assert!(rig.core.drift_ms.unwrap().abs() < 10., "{:?}", rig.core.drift_ms);
    }

    #[test]
    fn the_position_is_not_measured_during_a_correction() {
        let mut rig = Rig::playing(0.);
        rig.pass(2_000.);
        rig.view.position -= 90.;
        for _ in 0..12 {
            rig.pass(100.);
            rig.player(PlayerEvent::Position);
        }
        assert_eq!(rig.core.speed(), 1.045);
        // The player reads 150 ms off at this speed; that is no drift.
        rig.view.position += 150.;
        rig.pass(100.);
        assert!(rig.player(PlayerEvent::Position).is_empty());
        assert_eq!(rig.core.speed(), 1.045);
    }

    #[test]
    fn no_correction_when_the_user_switched_it_off() {
        let mut rig = Rig::playing(0.);
        rig.core.correct = false;
        rig.pass(2_000.);
        rig.view.position -= 300.;
        for _ in 0..20 {
            rig.pass(100.);
            assert!(rig.player(PlayerEvent::Position).is_empty());
        }
        // The difference still shows.
        assert!((rig.core.drift_ms.unwrap() - 300.).abs() < 1e-6);
    }

    #[test]
    fn no_correction_right_after_a_start() {
        let mut rig = Rig::playing(0.);
        rig.view.position -= 200.;
        rig.pass(500.);
        assert!(rig.player(PlayerEvent::Position).is_empty());
    }

    #[test]
    fn a_large_drift_is_a_catch_up() {
        let mut rig = Rig::playing(0.);
        rig.pass(2_000.);
        rig.view.position -= 5_000.;
        let mut out = Vec::new();
        for _ in 0..4 {
            rig.pass(250.);
            out.extend(rig.player(PlayerEvent::Position));
        }
        assert!(seek_of(&out).is_some(), "{out:?}");
    }

    #[test]
    fn a_stall_reports_buffering_once_and_ready_after_it() {
        let mut rig = Rig::playing(0.);
        rig.pass(3_000.);
        let out = rig.player(PlayerEvent::Stalled);
        let [Request::Buffering(report)] = sent(&out)[..] else { panic!("{out:?}") };
        assert_eq!(report.playlist_item_id, "p1");
        // A second stall soon after stays quiet.
        rig.pass(1_000.);
        assert!(sent(&rig.player(PlayerEvent::Stalled)).is_empty());
        let out = rig.player(PlayerEvent::Recovered);
        assert!(matches!(sent(&out)[..], [Request::Ready(_)]));
    }

    #[test]
    fn no_buffering_report_during_a_load_or_right_after_a_start() {
        let mut rig = Rig::new();
        rig.join();
        rig.queue("NewPlaylist", &["p1"], 0, 0., false);
        assert!(rig.player(PlayerEvent::Stalled).is_empty());
        let mut rig = Rig::playing(0.);
        rig.pass(300.);
        assert!(rig.player(PlayerEvent::Stalled).is_empty());
    }

    #[test]
    fn the_end_of_an_item_asks_for_the_next_and_waits() {
        let mut rig = Rig::playing(0.);
        let out = rig.player(PlayerEvent::Ended);
        assert_eq!(sent(&out), [&Request::NextItem { playlist_item_id: "p1".into() }]);
        assert!(!out.contains(&Action::Stop));
        rig.pass(500.);
        let out = rig.queue("NextItem", &["p1", "p2"], 1, 0., false);
        assert!(out.iter().any(|a| matches!(a, Action::Load { playlist_item_id, .. } if playlist_item_id == "p2")));
    }

    #[test]
    fn the_end_of_the_last_item_stops_after_the_wait() {
        let mut rig = Rig::playing(0.);
        rig.player(PlayerEvent::Ended);
        rig.pass(10_500.);
        assert_eq!(rig.feed(Input::Tick), vec![Action::Stop]);
    }

    #[test]
    fn the_end_of_the_last_item_stops_at_once_and_asks_nothing() {
        // The web client does the same: no request, the player stops, the
        // member stays in the group and follows it.
        let mut rig = Rig::playing_with(&["p1"], 0.);
        let out = rig.player(PlayerEvent::Ended);
        assert_eq!(out.last(), Some(&Action::Stop));
        assert!(sent(&out).is_empty(), "{out:?}");
        assert!(rig.core.in_group() && rig.core.following());
        assert_eq!(rig.core.phase_name(), "idle");
    }

    #[test]
    fn a_repeat_mode_keeps_the_end_of_the_last_item_going() {
        let mut rig = Rig::playing_with(&["p1"], 0.);
        rig.core.queue.as_mut().unwrap().repeat_mode = "RepeatAll".into();
        let out = rig.player(PlayerEvent::Ended);
        assert_eq!(sent(&out), [&Request::NextItem { playlist_item_id: "p1".into() }]);
        assert!(!out.contains(&Action::Stop));
    }

    #[test]
    fn closing_the_player_does_not_stop_the_group() {
        // The web client's stop button and its close button only stop the
        // local player. The group goes on for the other members, and
        // nothing asks the server to stop, pause or leave.
        let mut rig = Rig::playing(0.);
        let out = rig.player(PlayerEvent::Closed);
        assert_eq!(sent(&out), [&Request::SetIgnoreWait(true)]);
        assert!(!out.contains(&Action::Stop));
        assert!(rig.core.in_group());
        assert_eq!(rig.core.phase_name(), "idle");
        // The group plays on without this member and does not wait for it.
        assert!(rig.command(CommandKind::Pause, "p1", 1_000., 0.).is_empty());
        assert!(rig.command(CommandKind::Unpause, "p1", 1_000., 0.).is_empty());
    }

    #[test]
    fn no_action_of_the_user_asks_the_group_to_stop() {
        // `/SyncPlay/Stop` stops every member; the web client never sends it.
        let mut rig = Rig::playing(0.);
        let mut every = Vec::new();
        for intent in [Intent::Pause, Intent::Unpause, Intent::Next, Intent::Previous, Intent::StopFollowing, Intent::Follow, Intent::Leave] {
            every.extend(rig.user(intent));
        }
        every.extend(rig.player(PlayerEvent::Closed));
        every.extend(rig.player(PlayerEvent::Ended));
        assert!(!sent(&every).contains(&&Request::Stop), "{every:?}");
    }

    #[test]
    fn three_seeks_of_the_server_to_one_place_make_the_member_step_aside() {
        let mut rig = Rig::parked(0.);
        for _ in 0..2 {
            let out = rig.command(CommandKind::Seek, "p1", 30_000., 0.);
            let (_, token) = seek_of(&out).unwrap();
            rig.pass(100.);
            rig.player(PlayerEvent::Settled { token });
            rig.pass(100.);
        }
        let out = rig.command(CommandKind::Seek, "p1", 30_000., 0.);
        assert!(sent(&out).contains(&&Request::SetIgnoreWait(true)));
        assert!(!rig.core.following());
    }

    #[test]
    fn a_failed_load_steps_aside_and_a_new_queue_takes_the_member_back() {
        let mut rig = Rig::new();
        rig.join();
        rig.queue("NewPlaylist", &["p1"], 0, 0., false);
        let out = rig.player(PlayerEvent::LoadFailed { playlist_item_id: "p1".into() });
        assert!(sent(&out).contains(&&Request::SetIgnoreWait(true)));
        rig.pass(1_000.);
        let out = rig.queue("NewPlaylist", &["p9"], 0, 0., false);
        assert!(sent(&out).contains(&&Request::SetIgnoreWait(false)));
        assert!(out.iter().any(|a| matches!(a, Action::Load { .. })));
    }

    #[test]
    fn a_lost_ready_is_sent_again_while_the_group_waits() {
        let mut rig = Rig::new();
        rig.join();
        rig.queue("NewPlaylist", &["p1"], 0, 0., false);
        rig.player(PlayerEvent::Loaded { playlist_item_id: "p1".into() });
        rig.group(GroupUpdate::State { state: GroupState::Waiting, reason: "Buffer".into() });
        rig.pass(3_000.);
        assert!(rig.feed(Input::Tick).is_empty());
        rig.pass(2_500.);
        assert!(matches!(sent(&rig.feed(Input::Tick))[..], [Request::Ready(_)]));
    }

    #[test]
    fn a_command_before_the_first_clock_measurement_waits_for_it() {
        let mut rig = Rig::parked(0.);
        let command = Command {
            group_id: GROUP.into(),
            playlist_item_id: "p1".into(),
            when: rig.server_now() + 2_000.,
            position_ticks: Some(0),
            command: CommandKind::Unpause,
            emitted_at: rig.server_now(),
        };
        // The socket was faster than the first answer of the time request.
        let ready = std::mem::take(&mut rig.clock);
        let out = rig.feed(Input::Server(ServerMessage::Command(command)));
        assert!(out.is_empty());
        rig.clock = ready;
        let out = rig.feed(Input::ClockUpdated);
        assert!(matches!(sent(&out)[..], [Request::Ping { ms: 5 }]));
        assert!(out.iter().any(|a| matches!(a, Action::UnpauseAt(_))), "{out:?}");
    }

    #[test]
    fn a_reconnect_joins_the_group_again() {
        let mut rig = Rig::playing(0.);
        let out = rig.feed(Input::Reconnected);
        assert!(sent(&out).contains(&&Request::Join { group_id: GROUP.into() }));
        assert!(out.contains(&Action::CancelScheduled));
    }

    #[test]
    fn leaving_ends_the_corrections_and_keeps_the_player() {
        let mut rig = Rig::playing(0.);
        assert_eq!(sent(&rig.user(Intent::Leave)), [&Request::Leave]);
        let out = rig.group(GroupUpdate::Left);
        assert!(out.contains(&Action::Notice(Notice::Left)));
        assert!(!out.contains(&Action::Stop));
        assert!(!rig.core.in_group());
        // News of the old group after that does nothing.
        assert!(rig.command(CommandKind::Pause, "p1", 0., 0.).is_empty());
    }

    #[test]
    fn stop_following_and_follow_again() {
        let mut rig = Rig::playing(0.);
        let out = rig.user(Intent::StopFollowing);
        assert!(sent(&out).contains(&&Request::SetIgnoreWait(true)));
        assert!(out.contains(&Action::Stop));
        // Commands still come, and do nothing to the player.
        assert!(rig.command(CommandKind::Pause, "p1", 1_000., 0.).is_empty());
        let out = rig.user(Intent::Follow);
        assert_eq!(
            sent(&out),
            [&Request::SetIgnoreWait(false), &Request::Join { group_id: GROUP.into() }]
        );
    }

    #[test]
    fn following_again_loads_what_the_group_plays() {
        let mut rig = Rig::playing(0.);
        rig.user(Intent::StopFollowing);
        rig.pass(5_000.);
        rig.user(Intent::Follow);
        // The server answers the join with the group and the queue it has;
        // the queue has the time of its last change, which is not new.
        let info = rig.core.group().unwrap().clone();
        let queue = rig.core.queue().unwrap().clone();
        assert!(rig.group(GroupUpdate::Joined(info)).iter().all(|a| !matches!(a, Action::Notice(_))));
        let out = rig.group(GroupUpdate::Queue(queue));
        assert!(
            out.iter().any(|a| matches!(a, Action::Load { playlist_item_id, .. } if playlist_item_id == "p1")),
            "{out:?}"
        );
    }

    #[test]
    fn a_stop_command_stops_the_player() {
        let mut rig = Rig::playing(0.);
        let out = rig.command(CommandKind::Stop, "00000000000000000000000000000000", 0., 0.);
        assert!(out.contains(&Action::Stop));
    }

    #[test]
    fn members_come_and_go() {
        let mut rig = Rig::new();
        rig.join();
        rig.group(GroupUpdate::UserJoined("ben".into()));
        assert_eq!(rig.core.group().unwrap().participants, ["ana", "ben"]);
        rig.group(GroupUpdate::UserLeft("ana".into()));
        assert_eq!(rig.core.group().unwrap().participants, ["ben"]);
    }
}
