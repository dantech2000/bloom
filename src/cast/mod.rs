// SPDX-License-Identifier: AGPL-3.0-or-later
//! "Play on": where playback goes, and remote control both ways.
//!
//! As a controller: the user picks a target in the panel: another session
//! of the server, a cast device, or an AirPlay receiver (`target.rs` has
//! the one enum of them). While one is set, every play of the app goes to
//! it, and the panel steers it. A Jellyfin session is told through the
//! server, and its state comes back in its playback reports, which the
//! server pushes over the socket (a `Sessions` subscription) or which a
//! slow poll reads when there is no socket. A cast device is driven by
//! `crate::chromecast`, an AirPlay receiver by `crate::airplay`; each
//! reports on a channel that a task of this module drains.
//!
//! As a target: the app posts its capabilities, and the server sends it
//! `Play`, `Playstate` and `GeneralCommand` messages. Each one becomes a
//! call of the playback facade (`src/playback.rs`), so a remote pause in a
//! SyncPlay group is a request to the group, as a click is.

pub mod debug;
/// The shapes on the wire are in `bloom-core`, where their tests run
/// without the app.
pub use bloom_core::cast::protocol;
pub mod target;
pub mod ui;

use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::{Bounds, Context, Pixels, Task, Window};
use serde_json::Value;

use self::{
    protocol::{Action, Local, PlayCommand, RemoteCommand, SessionInfo},
    target::{CastTracks, Kind, Leave, Target, TargetView},
};
use crate::{
    airplay::{AirPlayEvent, picker},
    app::{Bloom, Panel},
    chromecast::{
        jellyfin as cast_jellyfin,
        mdns::{self, Device},
        session::{Event as CastEvent, Session},
    },
    jellyfin::{Client, Item, ItemsResult},
    realtime::SocketEvent,
};

/// Time between two reads of the sessions while a target is set and the
/// socket does not push them.
const POLL_WITHOUT_SOCKET: Duration = Duration::from_secs(2);
/// With the socket, a read now and then catches what a push missed.
const POLL_WITH_SOCKET: Duration = Duration::from_secs(15);
/// The subscription: the first push after 100 ms, then one a second.
const SUBSCRIPTION: &str = "100,1000";
/// How long the panel says "Searching…" for cast devices once the search
/// starts; after that a list with nobody in it says nothing.
const SEARCH_NOTE: Duration = Duration::from_secs(6);
/// Time between two looks of the panel and the chip at what moves with no
/// event: the position of a target that plays, and the cast devices the
/// search found.
const TICK: Duration = Duration::from_secs(1);
/// The first looks after the panel opens come sooner: the devices answer
/// the search within a moment.
const TICK_AT_FIRST: Duration = Duration::from_millis(300);
/// How long after the AirPlay engine gave no player the picker asks again.
const PICKER_RETRY: Duration = Duration::from_secs(2);
/// The ids of one request for the items of a remote `Play`: a request line
/// has a longest length, and a hundred ids stay well under it.
const IDS_PER_REQUEST: usize = 100;

/// Where a poll of the sessions stands: the session that asked, and the
/// choice of the target at the time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PollTicket {
    epoch: crate::app::SessionEpoch,
    target: crate::app::Revision,
}

impl PollTicket {
    /// Whether the snapshot of a poll that started under `self` may act on
    /// the target that is set under `now`: only when it is the same choice.
    /// A snapshot taken before the choice can lack the target without the
    /// target being gone, and must not turn it off.
    fn sees_target(self, now: PollTicket) -> bool {
        self == now
    }
}

/// Remote control as the app holds it.
#[derive(Default)]
pub struct CastState {
    /// The sessions this user can control, for the panel.
    pub sessions: Vec<SessionInfo>,
    pub loading: bool,
    /// Where playback goes. A Jellyfin session carries its last known
    /// state; a cast device has its connection in `Bloom::chromecast`,
    /// AirPlay its engine in `Bloom::airplay`.
    pub target: Target,
    /// When the state of a Jellyfin session came, so the position can
    /// move between reports.
    pub target_at: Option<Instant>,
    /// The choice of the target, among the choices made: a poll of the
    /// sessions that started before the choice says nothing about it.
    target_chosen: crate::app::Revision,
    /// The socket pushes the sessions.
    pub subscribed: bool,
    /// The server has this app's capabilities with media control.
    pub capabilities_posted: bool,
    /// What the server last said about this session, for `cast caps`.
    pub own: Option<String>,
    /// Why the last request failed, for the debug channel.
    pub error: Option<String>,
    poll: Option<Task<()>>,
    /// Where the seek bar and the volume bar of the panel were painted.
    pub seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// What a remote command asks of the window; the next frame does it.
    pending: Vec<Pending>,
    /// The item this app loaded on the cast device: the device reports
    /// only its id.
    pub cast_item: Option<Item>,
    /// The number the connection gave the load of that item; the end of
    /// the item comes with it (`CastEvent::Ended`).
    cast_load: u64,
    /// The cast devices the panel lists, as of the last look (`TICK`).
    pub devices: Vec<Device>,
    /// Looks at what moves with no event while the panel is open or a
    /// target is set (`start_cast_tick`); `ticking` until it ends.
    tick: Option<Task<()>>,
    ticking: bool,
    /// The streams the Jellyfin receiver app says it plays.
    pub cast_tracks: CastTracks,
    /// When the search for cast devices started, for "Searching…".
    pub search_started: Option<Instant>,
    search_timer: Option<Task<()>>,
    /// Drains the events of the AirPlay engine.
    airplay_task: Option<Task<()>>,
    /// Where the button of the AirPlay row was painted: the system route
    /// picker sits there.
    pub picker_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Where the picker is in the window now.
    pub picker_place: Option<picker::Place>,
    /// When the AirPlay engine last gave no player for the picker: the
    /// ask waits on the UI thread, so it is not made again at every frame.
    picker_failed: Option<Instant>,
    /// The fake device of `BLOOM_CHROMECAST_MOCK`.
    mock: Option<crate::chromecast::mock::Mock>,
}

enum Pending {
    Volume(f32),
    Fullscreen,
}

impl CastState {
    /// A target is set: plays go to it.
    pub fn active(&self) -> bool {
        !self.target.is_local()
    }

    pub fn kind(&self) -> Kind {
        self.target.kind()
    }

    pub fn device_name(&self) -> String {
        self.target.name()
    }

    /// The Jellyfin session that is the target, when one is.
    pub fn session(&self) -> Option<&SessionInfo> {
        match &self.target {
            Target::JellyfinSession(session) => Some(session),
            _ => None,
        }
    }

    pub fn session_mut(&mut self) -> Option<&mut SessionInfo> {
        match &mut self.target {
            Target::JellyfinSession(session) => Some(session),
            _ => None,
        }
    }

    /// The search for cast devices runs and found nobody yet.
    pub fn searching(&self) -> bool {
        self.search_started.is_some_and(|at| at.elapsed() < SEARCH_NOTE)
    }
}

#[easy_ext::ext(CastApi)]
impl Client {
    /// Items by their ids, a hundred to a request, in the order the server
    /// gives them. An id the server does not know is not in the answer.
    fn items_by_ids(&self, ids: &[String]) -> anyhow::Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let mut items = Vec::new();
        for ids in ids.chunks(IDS_PER_REQUEST) {
            let result: ItemsResult = self.get(
                "/Items",
                &[
                    ("userId", user.clone()),
                    ("ids", ids.join(",")),
                    ("fields", "Overview,PrimaryImageAspectRatio,ProductionYear,ParentId,MediaSources".to_string()),
                    ("enableImageTypes", "Primary,Backdrop,Thumb".to_string()),
                ],
            )?;
            items.extend(result.items);
        }
        Ok(items)
    }
}

/// The items of a remote `Play`, in the order of its ids. The first one
/// is read whole, as a page reads the item it plays; the rest come in one
/// request (they were one request each: a season was a hundred round
/// trips before anything played). An item of the rest that the server
/// does not give is left out.
fn play_items(client: &Client, ids: &[String]) -> anyhow::Result<Vec<Item>> {
    let Some((first, rest)) = ids.split_first() else {
        return Ok(Vec::new());
    };
    let mut items = vec![client.item(first)?];
    if !rest.is_empty() {
        items.extend(protocol::in_order(client.items_by_ids(rest)?, rest));
    }
    Ok(items)
}

impl Bloom {
    // ----- this app as a target -----------------------------------------

    /// The setting: other devices may control this app.
    pub fn remote_control_allowed(&self) -> bool {
        self.config.remote_control.unwrap_or(true)
    }

    pub fn toggle_remote_control(&mut self, cx: &mut Context<Self>) {
        let on = !self.remote_control_allowed();
        self.config.remote_control = Some(on);
        self.save_config(cx);
        self.post_capabilities(cx);
        cx.notify();
    }

    /// Tells the server what this session can do, so it is listed as a
    /// target. Without the socket of this app (a test instance on the
    /// user's config) the session is the user's own, and it stays as it is.
    pub fn post_capabilities(&mut self, cx: &mut Context<Self>) {
        if self.sync.session.is_none() {
            return;
        }
        let control = self.remote_control_allowed();
        let body = protocol::capabilities(control);
        self.fetch(
            cx,
            move |client| client.post("/Sessions/Capabilities/Full", &body).map(drop),
            move |this, result, _| match result {
                Ok(()) => {
                    log::info!("remote control: capabilities posted (control={control})");
                    this.cast.capabilities_posted = control;
                }
                Err(err) => log::warn!("remote control: capabilities: {err:#}"),
            },
        );
    }

    /// Every event of the socket passes here, before SyncPlay reads it.
    pub fn cast_socket(&mut self, event: &SocketEvent, cx: &mut Context<Self>) {
        match event {
            SocketEvent::Open { .. } => {
                // A new socket is a new session for the server.
                self.cast.capabilities_posted = false;
                self.cast.subscribed = false;
                self.post_capabilities(cx);
                if self.cast.session().is_some() {
                    self.cast_subscribe();
                }
            }
            SocketEvent::Closed => self.cast.subscribed = false,
            SocketEvent::Message { kind, data } => {
                if kind == "Sessions" {
                    self.cast_sessions_message(data, cx);
                } else if let Some(command) = protocol::parse(kind, data) {
                    if self.remote_control_allowed() {
                        log::info!("remote control: {command:?}");
                        self.cast_run(command, cx);
                    } else {
                        log::debug!("remote control is off; {command:?} ignored");
                    }
                }
            }
        }
    }

    /// Does what a command asks, through the playback facade.
    fn cast_run(&mut self, command: RemoteCommand, cx: &mut Context<Self>) {
        if let RemoteCommand::Play {
            item_ids,
            command,
            start_secs,
            start_index,
            audio_index,
            subtitle_index,
        } = command
        {
            // The list starts at the index the sender named.
            let skip = start_index.filter(|n| *n < item_ids.len()).unwrap_or(0);
            // The items come off the UI thread; then the queue takes them.
            self.fetch(
                cx,
                move |client| play_items(&client, &item_ids[skip..]),
                move |this, result, cx| match result {
                    Ok(items) => this.cast_play_local(items, command, start_secs, audio_index, subtitle_index, cx),
                    Err(err) => log::warn!("remote control: items of a Play: {err:#}"),
                },
            );
            return;
        }
        let local = Local {
            player_open: self.player_open,
            paused: self.player_status.paused,
            volume: self.volume,
            muted: self.muted,
        };
        match protocol::plan(&command, local) {
            Action::TogglePause => self.request_toggle_pause(cx),
            Action::SeekTo(secs) => self.request_seek_to(secs, cx),
            Action::SeekBy(secs) => self.request_seek_by(secs, cx),
            Action::Next => {
                self.request_next(cx);
            }
            Action::Previous => self.request_previous(cx),
            Action::Stop => self.request_stop(cx),
            Action::SetVolume(volume) => {
                self.set_volume(volume, cx);
                // The slider of the player needs the window to move.
                self.cast.pending.push(Pending::Volume(volume));
            }
            Action::SetMuted(muted) => {
                if self.muted != muted {
                    self.toggle_mute(cx);
                }
            }
            Action::ToggleMute => self.toggle_mute(cx),
            Action::SetAudioStream(index) => self.cast_select_track("Audio", index),
            Action::SetSubtitleStream(index) => self.cast_select_track("Subtitle", index),
            Action::Toast { header, text } => self.toast(header, text, cx),
            Action::ToggleFullscreen => self.cast.pending.push(Pending::Fullscreen),
            Action::Nothing => log::debug!("remote control: nothing to do for {command:?}"),
        }
        cx.notify();
    }

    /// Chooses a track of the item in the player by its stream index on
    /// the server. -1 turns subtitles off. The choice goes the way of the
    /// menus (`stream.rs`): in a transcode the server picks the track.
    fn cast_select_track(&mut self, kind: &str, stream_index: i64) {
        if kind == "Audio" {
            self.stream_audio(stream_index);
        } else {
            self.stream_subtitle(stream_index);
        }
    }

    /// Plays or queues the items of a `Play`, as the user would from a
    /// page: in the group when in one, else in the queue of this app.
    fn cast_play_local(
        &mut self,
        mut items: Vec<Item>,
        command: PlayCommand,
        start_secs: Option<f64>,
        audio_index: Option<i64>,
        subtitle_index: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let playing = self.player_open && self.playing.is_some();
        match command {
            PlayCommand::PlayNext if playing => {
                self.queue.upcoming.splice(0..0, items);
                cx.notify();
                return;
            }
            PlayCommand::PlayLast if playing => {
                self.queue.upcoming.extend(items);
                cx.notify();
                return;
            }
            _ => {}
        }
        // The first item starts where the sender asked; the queue takes
        // that from the resume point of the item.
        if let Some(secs) = start_secs {
            items[0].user_data.playback_position_ticks = protocol::secs_to_ticks(secs);
        }
        let resume = start_secs.is_some_and(|secs| secs > 0.);
        if self.sync_play(&items, resume, cx) {
            return;
        }
        let single = items.len() == 1;
        let first = items[0].clone();
        self.enhanced_remote_started();
        self.clear_queue();
        self.queue.upcoming = items;
        self.queue.resume_first = true;
        if !self.play_next(cx) {
            return;
        }
        // The player gets the focus at the next frame; a socket task has
        // no window.
        self.sync.focus_player = true;
        self.start_player_poll(cx);
        if single {
            self.queue_followers(&first, cx);
        }
        if let Some(index) = audio_index {
            self.cast_select_track("Audio", index);
        }
        if let Some(index) = subtitle_index {
            self.cast_select_track("Subtitle", index);
        }
        cx.notify();
    }

    /// Runs what a remote command left for the window, and keeps the
    /// system route picker where the AirPlay row of the panel is. Called
    /// at the start of each frame.
    pub fn prepare_cast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The AirPlay engine may get an item from any path (the debug
        // channel, the queue); its events must reach the target.
        self.watch_airplay(cx);
        for pending in std::mem::take(&mut self.cast.pending) {
            match pending {
                Pending::Volume(volume) => self
                    .volume_slider
                    .update(cx, |slider, cx| slider.set_value(volume, window, cx)),
                Pending::Fullscreen => window.toggle_fullscreen(),
            }
        }
        self.place_picker(window);
    }

    /// The picker is an AppKit view above the gpui surface: it goes where
    /// the AirPlay row of the panel painted its button at the last frame,
    /// and away when the panel closes or shows a target.
    fn place_picker(&mut self, window: &Window) {
        let wanted = self.panel_is(Panel::Cast) && self.cast.target.is_local() && self.airplay_row_shown();
        if !wanted {
            if self.cast.picker_place.take().is_some() || picker::shown() {
                picker::hide();
            }
            return;
        }
        let painted = self.cast.picker_bounds.get();
        let place: picker::Place = [
            f32::from(painted.origin.x),
            f32::from(painted.origin.y),
            f32::from(painted.size.width),
            f32::from(painted.size.height),
        ];
        if place[2] <= 0. || place[3] <= 0. || self.cast.picker_place == Some(place) {
            return;
        }
        // The ask waits for the engine on this thread: after one that
        // gave nothing, the next is not at the next frame.
        if self.cast.picker_failed.is_some_and(|at| at.elapsed() < PICKER_RETRY) {
            return;
        }
        let Some(player) = self.airplay.player() else {
            log::warn!("airplay: no player for the route picker");
            self.cast.picker_failed = Some(Instant::now());
            return;
        };
        self.cast.picker_failed = None;
        // The retain of `player` holds through the show; the view takes
        // its own.
        if picker::show(window, player.id(), place) {
            self.cast.picker_place = Some(place);
        }
    }

    /// The panel has the AirPlay row: the system sees a receiver.
    pub fn airplay_row_shown(&self) -> bool {
        !self.airplay.routes().is_empty()
    }

    // ----- this app as a controller ---------------------------------------

    pub fn toggle_cast_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.toggle_panel(Panel::Cast, window, cx);
        if open {
            self.load_cast_sessions(cx);
            self.start_cast_search(cx);
            self.start_cast_tick(cx);
            self.watch_airplay(cx);
            // The picker needs the engine's player; it starts now so the
            // next frame has it.
            if self.airplay_row_shown() {
                self.airplay.start();
            }
        }
        cx.notify();
    }

    /// The state of the target as the panel and the chip show it.
    pub fn target_view(&self) -> TargetView {
        match &self.cast.target {
            Target::Local => TargetView::local(),
            Target::JellyfinSession(session) => target::from_session(session, self.cast.target_at),
            Target::Chromecast(device) => {
                let status = self.chromecast.session.as_ref().map(Session::status).unwrap_or_default();
                let title = self.cast.cast_item.as_ref().map(Item::display_title);
                target::from_chromecast(device, &status, title, self.cast.cast_tracks)
            }
            Target::AirPlay => target::from_airplay(&self.airplay.status()),
        }
    }

    /// Makes `chosen` the target and leaves the one before. Local playback
    /// stops: one item plays at a time, on the target.
    pub fn select_target(&mut self, chosen: Target, cx: &mut Context<Self>) {
        let before = self.cast.target.kind();
        let (target, leave) = std::mem::take(&mut self.cast.target).switch(chosen);
        // Nothing left and something was set: the same device again.
        let same = leave.is_none() && before != Kind::Local;
        self.cast.target = target;
        self.cast.target_chosen = self.next_revision();
        if let Some(leave) = leave {
            self.leave_target(leave, cx);
        }
        self.cast.target_at = Some(Instant::now());
        self.cast.error = None;
        match self.cast.target.clone() {
            Target::Local => {
                self.cast.target_at = None;
            }
            Target::JellyfinSession(session) => {
                if !same {
                    self.cast_subscribe();
                    self.start_cast_poll(cx);
                    self.toast("Play On", format!("Playback goes to {}.", session.device_name), cx);
                }
            }
            Target::Chromecast(device) => {
                let name = device.name.clone();
                if self.chromecast_open(device, cx) {
                    self.toast("Play On", format!("Playback goes to {name}."), cx);
                }
            }
            Target::AirPlay => self.watch_airplay(cx),
        }
        if self.cast.active() {
            self.start_cast_tick(cx);
        }
        if self.cast.active() && self.player_open {
            self.player.stop();
            self.close_player_view(cx);
        }
        cx.notify();
    }

    /// Undoes a target that is left: the subscription, the connection to
    /// the cast device (and the item this app put on it), the AirPlay
    /// engine.
    fn leave_target(&mut self, leave: Leave, cx: &mut Context<Self>) {
        match leave {
            Leave::Jellyfin => {
                self.cast.poll = None;
                if self.cast.subscribed {
                    self.sync.socket_send("SessionsStop", None);
                    self.cast.subscribed = false;
                }
            }
            Leave::Chromecast(device) => {
                log::info!("cast: leaving {:?}", device.name);
                if let Some(session) = &self.chromecast.session {
                    // The device keeps what another sender put on it; what
                    // this app loaded ends with the target.
                    if self.cast.cast_item.is_some() && session.status().loaded() {
                        session.stop_media();
                    }
                }
                self.chromecast_close();
            }
            Leave::AirPlay => self.airplay.disconnect(),
        }
        cx.notify();
    }

    /// Makes a session the target. It must be in the list of the panel.
    pub fn cast_to(&mut self, session_id: &str, cx: &mut Context<Self>) -> Result<String, String> {
        let Some(session) = self.cast.sessions.iter().find(|s| s.id == session_id).cloned() else {
            return Err(format!("no controllable session {session_id}"));
        };
        let name = session.device_name.clone();
        self.select_target(Target::JellyfinSession(session), cx);
        Ok(name)
    }

    /// Back to local playback, with nothing of a target left: the
    /// subscription and the poll of a Jellyfin session, the connection to
    /// a cast device (one the debug channel opened with no target as
    /// well), the AirPlay engine, and what the panel knew of the target.
    /// A Jellyfin session goes on as it is; what this app put on a cast
    /// device or an AirPlay receiver stops. A change of the account calls
    /// this, so it must leave nothing of the account before.
    pub fn cast_off(&mut self, cx: &mut Context<Self>) {
        let leave = std::mem::take(&mut self.cast.target).leave();
        self.cast.target_chosen = self.next_revision();
        self.cast.target_at = None;
        self.cast.error = None;
        if let Some(leave) = leave {
            self.leave_target(leave, cx);
        }
        self.chromecast_close();
        cx.notify();
    }

    /// "Play here instead": the item of the target goes on in this player
    /// from where the target was, when that is known.
    pub fn cast_play_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.target_view();
        let item = match &self.cast.target {
            Target::Local => None,
            Target::JellyfinSession(session) => session.now_playing_item.clone(),
            Target::Chromecast(_) => self.cast.cast_item.clone(),
            Target::AirPlay => self.playing.clone(),
        }
        .filter(|_| view.loaded);
        self.cast_off(cx);
        let Some(mut item) = item else { return };
        let resume = view.position > 0.;
        if resume {
            item.user_data.playback_position_ticks = protocol::secs_to_ticks(view.position);
        }
        self.play(&item, resume, window, cx);
    }

    // ----- Jellyfin sessions ---------------------------------------------

    /// Reads the sessions this user can control, and the state of the
    /// target with them.
    pub fn load_cast_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        // One poll on its way at a time: the next one waits for its answer.
        if self.cast.loading {
            return;
        }
        let (user_id, device_id) = (session.user_id.clone(), self.config.device_id.clone());
        self.cast.loading = true;
        let asked = PollTicket { epoch: self.session_epoch, target: self.cast.target_chosen };
        self.fetch(
            cx,
            move |client| {
                let sessions: Vec<SessionInfo> =
                    client.get("/Sessions", &[("controllableByUserId", user_id)])?;
                Ok(protocol::controllable(sessions, &device_id))
            },
            move |this, result, cx| {
                this.cast.loading = false;
                // The sessions of another user or server are not for this one.
                if this.session_epoch != asked.epoch {
                    return;
                }
                let now = PollTicket { epoch: this.session_epoch, target: this.cast.target_chosen };
                match result {
                    Ok(sessions) => {
                        if let Some(target) = this.cast.session().filter(|_| asked.sees_target(now)) {
                            match sessions.iter().find(|s| s.id == target.id) {
                                Some(fresh) => this.cast_target_update(fresh.clone()),
                                None => {
                                    let name = target.device_name.clone();
                                    this.cast_off(cx);
                                    this.toast("Play On", format!("{name} is not available any more."), cx);
                                }
                            }
                        }
                        this.cast.sessions = sessions;
                        this.cast.error = None;
                    }
                    Err(err) => {
                        log::warn!("remote control: sessions: {err:#}");
                        this.cast.error = Some(format!("{err:#}"));
                    }
                }
                cx.notify();
            },
        );
    }

    /// A push of the sessions over the socket.
    fn cast_sessions_message(&mut self, data: &Value, cx: &mut Context<Self>) {
        // Read in place: the push comes every second, with the item of
        // every session in it.
        let Ok(sessions) = <Vec<SessionInfo> as serde::Deserialize>::deserialize(data) else {
            return;
        };
        let mut changed = false;
        if let Some(target) = self.cast.session()
            && let Some(fresh) = sessions.iter().find(|s| s.id == target.id)
        {
            self.cast_target_update(fresh.clone());
            changed = true;
        }
        // The list of the panel follows for the sessions it knows.
        for known in &mut self.cast.sessions {
            if let Some(fresh) = sessions.iter().find(|s| s.id == known.id) {
                *known = fresh.clone();
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn cast_target_update(&mut self, fresh: SessionInfo) {
        if self.cast.session().is_some() {
            self.cast.target = Target::JellyfinSession(fresh);
            self.cast.target_at = Some(Instant::now());
        }
    }

    /// Asks the server to push the sessions over the socket.
    fn cast_subscribe(&mut self) {
        if self.sync.socket_send("SessionsStart", Some(Value::String(SUBSCRIPTION.into()))) {
            self.cast.subscribed = true;
        }
    }

    /// Reads the sessions on a timer while a session is the target.
    fn start_cast_poll(&mut self, cx: &mut Context<Self>) {
        self.cast.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let wait = this
                    .read_with(cx, |this, _| {
                        if this.cast.subscribed { POLL_WITH_SOCKET } else { POLL_WITHOUT_SOCKET }
                    })
                    .unwrap_or(POLL_WITHOUT_SOCKET);
                cx.background_executor().timer(wait).await;
                let going = this.update(cx, |this, cx| {
                    if this.cast.session().is_some() {
                        this.load_cast_sessions(cx);
                    }
                    this.cast.session().is_some()
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Sends items to the target instead of the player. False when there
    /// is no target: the caller plays them itself.
    pub fn cast_play(&mut self, items: &[Item], resume: bool, cx: &mut Context<Self>) -> bool {
        let Some(first) = items.first() else {
            return self.cast.active();
        };
        let start = if resume { first.resume_secs() as f64 } else { 0. };
        match self.cast.kind() {
            Kind::Local => return false,
            Kind::Jellyfin => {
                let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
                self.cast_send_play(ids, PlayCommand::PlayNow, resume.then_some(start), cx);
            }
            Kind::Chromecast => self.chromecast_load(items, start, cx),
            Kind::AirPlay => {
                self.airplay_send(first, start, cx);
            }
        }
        true
    }

    /// Puts an item in the queue of the target: after the item that plays,
    /// or at the end. Only a Jellyfin session has a queue to put it in.
    pub fn cast_enqueue(&mut self, item_id: String, next: bool, cx: &mut Context<Self>) {
        let command = if next { PlayCommand::PlayNext } else { PlayCommand::PlayLast };
        self.cast_send_play(vec![item_id], command, None, cx);
    }

    fn cast_send_play(
        &mut self,
        item_ids: Vec<String>,
        command: PlayCommand,
        start_secs: Option<f64>,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.cast.session() else { return };
        let (id, name) = (target.id.clone(), target.device_name.clone());
        let query = protocol::play_query(&item_ids, command, start_secs);
        let what = match command {
            PlayCommand::PlayNow => format!("Playing on {name}."),
            PlayCommand::PlayNext => format!("Plays next on {name}."),
            PlayCommand::PlayLast => format!("Added to the queue of {name}."),
        };
        self.cast_request(
            move |client| client.call("POST", &format!("/Sessions/{id}/Playing"), &query),
            Some(what),
            cx,
        );
    }

    /// A playstate command for a Jellyfin session: Pause, Unpause,
    /// PlayPause, Stop, NextTrack, PreviousTrack, or Seek with a position.
    pub fn cast_playstate(&mut self, command: &str, seek_secs: Option<f64>, cx: &mut Context<Self>) {
        let Some(target) = self.cast.session() else { return };
        let id = target.id.clone();
        // The panel shows the new state at once; the report of the target
        // confirms it.
        let position = self.target_view().position;
        let target = self.cast.session_mut().expect("target");
        match command {
            "Pause" => target.play_state.is_paused = true,
            "Unpause" => target.play_state.is_paused = false,
            "PlayPause" => target.play_state.is_paused = !target.play_state.is_paused,
            "Seek" => target.play_state.position_ticks = seek_secs.map(protocol::secs_to_ticks),
            "Stop" => target.now_playing_item = None,
            _ => {}
        }
        if command != "Seek" {
            target.play_state.position_ticks = Some(protocol::secs_to_ticks(position));
        }
        self.cast.target_at = Some(Instant::now());
        let mut query = Vec::new();
        if let Some(secs) = seek_secs.filter(|_| command == "Seek") {
            query.push(("seekPositionTicks", protocol::secs_to_ticks(secs).to_string()));
        }
        let path = format!("/Sessions/{id}/Playing/{command}");
        self.cast_request(move |client| client.call("POST", &path, &query), None, cx);
    }

    /// A general command for a Jellyfin session, with its arguments.
    pub fn cast_general(&mut self, name: &str, arguments: &[(&str, String)], cx: &mut Context<Self>) {
        let Some(target) = self.cast.session_mut() else { return };
        let id = target.id.clone();
        match (name, arguments.first()) {
            ("SetVolume", Some((_, value))) => {
                target.play_state.volume_level = value.parse().ok();
                target.play_state.is_muted = false;
            }
            ("ToggleMute", _) => target.play_state.is_muted = !target.play_state.is_muted,
            ("Mute", _) => target.play_state.is_muted = true,
            ("Unmute", _) => target.play_state.is_muted = false,
            ("SetAudioStreamIndex", Some((_, value))) => {
                target.play_state.audio_stream_index = value.parse().ok();
            }
            ("SetSubtitleStreamIndex", Some((_, value))) => {
                target.play_state.subtitle_stream_index = value.parse().ok();
            }
            _ => {}
        }
        let body = protocol::general_command(name, arguments);
        let path = format!("/Sessions/{id}/Command");
        self.cast_request(move |client| client.post(&path, &body).map(drop), None, cx);
    }

    pub fn cast_set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        let volume = volume.clamp(0., 100.).round();
        self.cast_general("SetVolume", &[("Volume", format!("{volume}"))], cx);
    }

    /// Sends a request to the target off the UI thread; a failure shows
    /// as a toast.
    fn cast_request<W>(&mut self, work: W, done: Option<String>, cx: &mut Context<Self>)
    where
        W: FnOnce(crate::jellyfin::Client) -> anyhow::Result<()> + Send + 'static,
    {
        self.fetch(cx, work, move |this, result, cx| {
            match result {
                Ok(()) => {
                    this.cast.error = None;
                    if let Some(text) = done {
                        this.toast("Play On", text, cx);
                    }
                }
                Err(err) => {
                    this.cast.error = Some(format!("{err:#}"));
                    this.toast("Play On", format!("The device did not take the command: {err:#}"), cx);
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    // ----- the controls of the panel, for every kind of target ------------

    pub fn target_toggle_pause(&mut self, cx: &mut Context<Self>) {
        let paused = self.target_view().paused;
        self.target_set_paused(!paused, cx);
    }

    pub fn target_set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        match self.cast.kind() {
            Kind::Local => {}
            Kind::Jellyfin => self.cast_playstate(if paused { "Pause" } else { "Unpause" }, None, cx),
            Kind::Chromecast => self.chromecast_do(|s| if paused { s.pause() } else { s.play() }),
            Kind::AirPlay => {
                if paused {
                    self.airplay.pause()
                } else {
                    self.airplay.play()
                }
            }
        }
        cx.notify();
    }

    pub fn target_seek(&mut self, secs: f64, cx: &mut Context<Self>) {
        let secs = secs.max(0.);
        match self.cast.kind() {
            Kind::Local => {}
            Kind::Jellyfin => self.cast_playstate("Seek", Some(secs), cx),
            Kind::Chromecast => self.chromecast_do(|s| s.seek(secs)),
            Kind::AirPlay => self.airplay.seek(secs),
        }
        cx.notify();
    }

    pub fn target_stop(&mut self, cx: &mut Context<Self>) {
        match self.cast.kind() {
            Kind::Local => {}
            Kind::Jellyfin => self.cast_playstate("Stop", None, cx),
            Kind::Chromecast => self.chromecast_do(|s| s.stop_media()),
            Kind::AirPlay => self.airplay.stop(),
        }
        cx.notify();
    }

    /// Next or previous in the queue of the target; only a Jellyfin
    /// session has one.
    pub fn target_skip(&mut self, next: bool, cx: &mut Context<Self>) {
        if self.cast.kind() == Kind::Jellyfin {
            self.cast_playstate(if next { "NextTrack" } else { "PreviousTrack" }, None, cx);
        }
    }

    /// 0 to 100.
    pub fn target_set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        let volume = volume.clamp(0., 100.);
        match self.cast.kind() {
            Kind::Local => {}
            Kind::Jellyfin => self.cast_set_volume(volume, cx),
            Kind::Chromecast => self.chromecast_do(|s| s.set_volume(f64::from(volume) / 100.)),
            Kind::AirPlay => self.airplay.set_volume(volume / 100.),
        }
        cx.notify();
    }

    pub fn target_toggle_mute(&mut self, cx: &mut Context<Self>) {
        let muted = self.target_view().muted;
        match self.cast.kind() {
            Kind::Local => {}
            Kind::Jellyfin => self.cast_general("ToggleMute", &[], cx),
            Kind::Chromecast => self.chromecast_do(|s| s.set_muted(!muted)),
            Kind::AirPlay => self.airplay.set_muted(!muted),
        }
        cx.notify();
    }

    /// A stream of the item by its index on the server; -1 turns subtitles
    /// off. AirPlay carries no track of ours.
    pub fn target_set_track(&mut self, kind: &str, index: i64, cx: &mut Context<Self>) {
        let audio = kind == "Audio";
        match self.cast.kind() {
            Kind::Local | Kind::AirPlay => {}
            Kind::Jellyfin => {
                let name = if audio { "SetAudioStreamIndex" } else { "SetSubtitleStreamIndex" };
                self.cast_general(name, &[("Index", index.to_string())], cx);
            }
            Kind::Chromecast => {
                if audio {
                    self.cast.cast_tracks.audio_index = Some(index);
                    self.chromecast_do(|s| s.set_audio(index));
                } else {
                    self.cast.cast_tracks.subtitle_index = Some(index);
                    self.chromecast_do(|s| s.set_subtitle((index >= 0).then_some(index)));
                }
            }
        }
        cx.notify();
    }

    // ----- cast devices -----------------------------------------------------

    fn chromecast_do(&mut self, act: impl FnOnce(&Session)) {
        match &self.chromecast.session {
            Some(session) => act(session),
            None => self.cast.error = Some("not connected to the device".into()),
        }
    }

    /// Starts the search for cast devices, unless it runs. The panel says
    /// "Searching…" for a moment after.
    pub fn start_cast_search(&mut self, cx: &mut Context<Self>) {
        if self.chromecast.discovery.is_some() {
            return;
        }
        self.chromecast.discovery = Some(mdns::discover());
        self.cast.search_started = Some(Instant::now());
        if crate::options::options().chromecast_mock && self.cast.mock.is_none() {
            let mock = crate::chromecast::mock::Mock::start();
            log::info!("cast: mock device on 127.0.0.1:{}", mock.port);
            self.cast.mock = Some(mock);
        }
        self.cast.devices = self.chromecast_devices();
        // The note goes away by itself.
        self.cast.search_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_NOTE).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        }));
    }

    /// Ends the search: its thread asks the network every few seconds, and
    /// nobody reads the answers with the panel closed. A search the debug
    /// channel asked for stays, so a test can ask it again.
    fn stop_cast_search(&mut self) {
        if self.chromecast.keep_search || self.chromecast.discovery.is_none() {
            return;
        }
        self.chromecast.discovery = None;
        self.cast.search_started = None;
        self.cast.search_timer = None;
        self.cast.devices = self.chromecast_devices();
    }

    /// Looks, while the panel is open or a target is set, at what moves
    /// with no event: the cast devices the search found, and the position
    /// of a target that plays (a cast device says nothing while its item
    /// runs on). The search ends here when nobody needs it any more.
    pub fn start_cast_tick(&mut self, cx: &mut Context<Self>) {
        if self.cast.ticking {
            return;
        }
        self.cast.ticking = true;
        self.cast.tick = Some(cx.spawn(async move |this, cx| {
            let mut looks = 0;
            loop {
                looks += 1;
                let wait = if looks <= 3 { TICK_AT_FIRST } else { TICK };
                cx.background_executor().timer(wait).await;
                let going = this.update(cx, |this, cx| {
                    let (panel, chromecast) = (this.panel_is(Panel::Cast), this.cast.kind() == Kind::Chromecast);
                    if !panel && !chromecast {
                        this.stop_cast_search();
                    }
                    let devices = this.chromecast_devices();
                    let mut moved = devices != this.cast.devices;
                    this.cast.devices = devices;
                    if this.cast.active() {
                        let view = this.target_view();
                        moved |= view.loaded && !view.paused;
                    }
                    // The picker asks the engine again at a frame.
                    moved |= this.cast.picker_failed.is_some();
                    if moved {
                        cx.notify();
                    }
                    let going = panel || this.cast.active();
                    this.cast.ticking = going;
                    going
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// The cast devices there are now: what the search found, and the fake
    /// one of a test. The panel reads `CastState::devices`, the list of
    /// the last look.
    pub fn chromecast_devices(&self) -> Vec<Device> {
        let mut devices: Vec<Device> = self.chromecast.discovery.as_ref().map(mdns::Discovery::devices).unwrap_or_default();
        if let Some(mock) = &self.cast.mock {
            devices.push(mock.device());
        }
        devices.sort_by_cached_key(|device| device.name.to_lowercase());
        devices
    }

    /// Opens the connection to the device and reads its state; nothing on
    /// the device changes until a play or a control. False when the
    /// connection to this device is open already.
    pub(crate) fn chromecast_open(&mut self, device: Device, cx: &mut Context<Self>) -> bool {
        if self.chromecast.session.as_ref().is_some_and(|s| s.device().id == device.id) {
            return false;
        }
        self.chromecast_close();
        let (session, events) = Session::connect(device);
        self.chromecast.session = Some(session);
        self.chromecast.events.clear();
        self.chromecast.task = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let fed = this.update(cx, |this, cx| this.on_cast_event(event, cx));
                if fed.is_err() {
                    break;
                }
            }
        }));
        true
    }

    /// Closes the connection to the cast device, with what the panel knew
    /// of the item on it.
    pub(crate) fn chromecast_close(&mut self) {
        self.chromecast.session = None;
        self.chromecast.task = None;
        self.cast.cast_item = None;
        self.cast.cast_load = 0;
        self.cast.cast_tracks = CastTracks::default();
    }

    /// An event of the cast connection: the status moves the panel; a
    /// report of the Jellyfin receiver app names the streams it plays.
    fn on_cast_event(&mut self, event: CastEvent, cx: &mut Context<Self>) {
        self.chromecast.note(&event);
        match &event {
            CastEvent::Error(text) => {
                log::warn!("cast: {text}");
                self.cast.error = Some(text.clone());
            }
            CastEvent::Message { namespace, payload } if namespace == cast_jellyfin::NAMESPACE => {
                if let Some(report) = cast_jellyfin::parse_report(payload) {
                    if report.audio_index.is_some() {
                        self.cast.cast_tracks.audio_index = report.audio_index;
                    }
                    if report.subtitle_index.is_some() {
                        self.cast.cast_tracks.subtitle_index = report.subtitle_index;
                    }
                }
            }
            // The item of a load is gone from the device. Only the load of
            // the item the panel shows counts: a new item makes the device
            // change its app, and the end of the app before it comes with
            // the number of the item before it.
            CastEvent::Ended { load } if *load == self.cast.cast_load => self.cast.cast_item = None,
            _ => {}
        }
        cx.notify();
    }

    /// Plays items on the cast device: the Jellyfin receiver app with the
    /// identity of this session takes them all as its queue, and the
    /// Default Media Receiver the stream URL of the first one when that
    /// app does not launch.
    pub(crate) fn chromecast_load(&mut self, items: &[Item], start_secs: f64, cx: &mut Context<Self>) {
        let (Some(session), Some(account), Some(first)) = (&self.chromecast.session, &self.session, items.first()) else {
            self.cast.error = Some("not connected to the device".into());
            cx.notify();
            return;
        };
        let request = cast_jellyfin::load_request(&account.client, &account.server_id, items, start_secs);
        let name = session.device().name.clone();
        self.cast.cast_load = session.load(request);
        self.cast.cast_item = Some(first.clone());
        self.cast.cast_tracks = CastTracks::default();
        self.toast("Play On", format!("Playing on {name}."), cx);
        cx.notify();
    }

    // ----- AirPlay -----------------------------------------------------------

    /// Drains the events of the AirPlay engine, once.
    pub fn watch_airplay(&mut self, cx: &mut Context<Self>) {
        if self.cast.airplay_task.is_some() {
            return;
        }
        let events = self.airplay.events();
        self.cast.airplay_task = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let fed = this.update(cx, |this, cx| this.on_airplay_event(event, cx));
                if fed.is_err() {
                    break;
                }
            }
        }));
    }

    /// An event of the AirPlay engine: a route the user picked, or an item
    /// sent, makes AirPlay the target; the end of it gives the target up.
    fn on_airplay_event(&mut self, event: AirPlayEvent, cx: &mut Context<Self>) {
        let status = self.airplay.status();
        let before = self.cast.target.clone();
        let after = before.clone().after_airplay(&event, &status);
        if after != before {
            if after == Target::AirPlay {
                log::info!("airplay: the target");
                self.select_target(Target::AirPlay, cx);
            } else {
                log::info!("airplay: ended; back to this device");
                self.cast.target = after;
                self.cast.target_at = None;
            }
        }
        if let AirPlayEvent::LoadFailed { error, .. } = &event {
            self.cast.error = Some(error.clone());
            self.toast("AirPlay", error.clone(), cx);
        }
        // A failure after the start has no load to answer: the user hears
        // of it here.
        if matches!(event, AirPlayEvent::State(crate::airplay::AirPlayState::Failed)) {
            let error = self.airplay.status().error.unwrap_or_else(|| "Playback failed.".to_string());
            self.cast.error = Some(error.clone());
            self.toast("AirPlay", error, cx);
        }
        cx.notify();
    }
}

/// A poll of the sessions that answers after the target was chosen
/// (review of 2026-10-05, devices finding 5). See `app::race_harness`.
#[cfg(test)]
mod race_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui_kit::TestAppContext;
    use serde_json::json;

    use crate::app::{Screen, race_harness::{MockServer, app, plain, session, tick_until}};

    /// A new item makes the device change its app: the app before goes,
    /// and its end comes with the number of the item before. That is not
    /// the end of the new item; the end with its own number is.
    #[gpui_kit::test]
    fn the_end_of_the_item_before_keeps_the_new_item(cx: &mut TestAppContext) {
        use crate::chromecast::session::{Event, Status};
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.cast.cast_item = Some(crate::app::race_harness::item("m2", "Two", "Movie"));
            this.cast.cast_load = 2;
            this.on_cast_event(Event::Ended { load: 1 }, cx);
            assert!(this.cast.cast_item.is_some(), "the end of the item before dropped the new item");
            // A status with no app (the launch takes a moment; a key on
            // the remote of the TV sends one) says nothing of the item.
            this.on_cast_event(Event::Status(Status::default()), cx);
            assert!(this.cast.cast_item.is_some(), "a status during the launch dropped the item");
            this.on_cast_event(Event::Ended { load: 2 }, cx);
            assert!(this.cast.cast_item.is_none(), "the item stays though its app is gone");
        });
    }

    /// The items of a remote `Play` come in two requests, whatever their
    /// number, and in the order of the ids, whatever order the server
    /// gives them in. An id the server does not know is left out.
    #[test]
    fn the_items_of_a_play_come_in_one_request_in_their_order() {
        let server = MockServer::start(|method, path, _| match (method, path) {
            ("GET", p) if p.starts_with("/Items/a") => (200, json!({ "Id": "a", "Name": "A", "Type": "Episode" }).to_string()),
            ("GET", p) if p.starts_with("/Items?") => {
                assert!(p.contains("ids=b%2Cgone%2Cc") || p.contains("ids=b,gone,c"), "{p}");
                let items = json!([{ "Id": "c", "Name": "C", "Type": "Episode" }, { "Id": "b", "Name": "B", "Type": "Episode" }]);
                (200, json!({ "Items": items, "TotalRecordCount": 2 }).to_string())
            }
            _ => plain(method, path),
        });
        let client = crate::jellyfin::Client::new(&server.url, "dev-1").with_session("t", "u1");
        let ids: Vec<String> = ["a", "b", "gone", "c"].map(String::from).to_vec();
        let items = super::play_items(&client, &ids).unwrap();
        assert_eq!(items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(server.count("GET", "/Items"), 2);
        assert!(super::play_items(&client, &[]).unwrap().is_empty());
        assert_eq!(server.count("GET", "/Items"), 2);
    }

    /// Back to this device leaves nothing of the target: a change of the
    /// account calls `cast_off` and counts on it.
    #[gpui_kit::test]
    fn cast_off_leaves_nothing_of_the_target(cx: &mut TestAppContext) {
        use crate::cast::target::Target;
        let mock = crate::chromecast::mock::Mock::start();
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.select_target(Target::Chromecast(mock.device()), cx);
            this.cast.cast_item = Some(crate::app::race_harness::item("m1", "One", "Movie"));
            this.cast.error = Some("old".into());
            assert!(this.chromecast.session.is_some());
            this.cast_off(cx);
            assert!(this.cast.target.is_local());
            assert!(this.chromecast.session.is_none() && this.chromecast.task.is_none());
            assert!(this.cast.cast_item.is_none() && this.cast.error.is_none() && this.cast.target_at.is_none());
            // A connection with no target (the debug channel opens one)
            // goes as well.
            this.chromecast_open(mock.device(), cx);
            this.cast_off(cx);
            assert!(this.chromecast.session.is_none());
        });
    }

    /// A server whose second list of the sessions lacks the TV.
    fn server() -> MockServer {
        let asked = AtomicUsize::new(0);
        MockServer::start(move |method, path, _| match (method, path) {
            ("GET", p) if p.starts_with("/Sessions") => {
                let tv = json!({
                    "Id": "s1", "UserId": "u1", "UserName": "U", "Client": "Web", "DeviceName": "TV",
                    "DeviceId": "dev-tv", "SupportsRemoteControl": true, "SupportsMediaControl": true
                });
                let n = asked.fetch_add(1, Ordering::Relaxed) + 1;
                (200, if n == 2 { json!([]) } else { json!([tv]) }.to_string())
            }
            _ => plain(method, path),
        })
    }

    #[gpui_kit::test]
    fn a_snapshot_from_before_the_choice_does_not_turn_the_target_off(cx: &mut TestAppContext) {
        let server = server();
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&server.url, "u1"));
            this.screen = Screen::Main;
            this.load_cast_sessions(cx);
        });
        cx.run_until_parked();
        bloom.read_with(cx, |this, _| assert_eq!(this.cast.sessions.len(), 1, "the panel lists the TV"));
        // A poll goes out and is answered (without the TV: it was away for a
        // moment); the answer waits while the user chooses the TV.
        bloom.update(cx, |this, cx| this.load_cast_sessions(cx));
        tick_until(cx, || server.count("GET", "/Sessions") == 2);
        bloom.update(cx, |this, cx| assert_eq!(this.cast_to("s1", cx), Ok("TV".to_string())));
        cx.run_until_parked();
        bloom.read_with(cx, |this, _| {
            assert_eq!(
                this.cast.session().map(|s| s.id.as_str()),
                Some("s1"),
                "the old snapshot turned the target off"
            );
        });
    }

    #[test]
    fn a_poll_sees_the_target_of_its_own_choice_only() {
        use super::PollTicket;
        use crate::app::{Revision, SessionEpoch};
        let asked = PollTicket { epoch: SessionEpoch::default(), target: Revision::default() };
        assert!(asked.sees_target(asked));
        let chosen_after = PollTicket { epoch: SessionEpoch::default(), target: Revision(1) };
        assert!(!asked.sees_target(chosen_after));
        let other_session = PollTicket { epoch: SessionEpoch(1), target: Revision::default() };
        assert!(!asked.sees_target(other_session));
    }
}
