// SPDX-License-Identifier: AGPL-3.0-or-later
//! "Play on": where playback goes, and remote control both ways.
//!
//! As a controller: the user picks a target in the panel: another session
//! of the server, a cast device, or an AirPlay receiver (`target.rs` has
//! the one enum of them; `remote.rs` has the target that is set, behind
//! one trait). While one is set, every play of the app goes to
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
pub mod remote;
pub mod target;
pub mod ui;

use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use gpui_kit::{Bounds, Context, Pixels, Task, Window};
use serde_json::Value;

use self::{
    protocol::{Action, Local, PlayCommand, RemoteCommand, SessionInfo},
    remote::{AirPlayRemote, CastLoad, ChromecastRemote, Effect, JellyfinRemote, Play, Remote, RemoteTarget, Track},
    target::{Kind, Target, TargetView},
};
use crate::{
    airplay::{AirPlayEvent, picker},
    app::{Bloom, Panel},
    chromecast::{
        discovery::{self, Device},
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
    /// Where playback goes: the target that is set, with what its kind
    /// keeps (the last state of a Jellyfin session, the connection of a
    /// cast device, the AirPlay engine). None is this device.
    pub remote: Option<Remote>,
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
    /// Where the seek bar and the volume bar of the panel were painted.
    pub seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub volume_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// What a remote command asks of the window; the next frame does it.
    pending: Vec<Pending>,
    /// The cast devices the panel lists, as of the last look (`TICK`).
    pub devices: Vec<Device>,
    /// Looks at what moves with no event while the panel is open or a
    /// target is set (`start_cast_tick`); `ticking` until it ends.
    tick: Option<Task<()>>,
    ticking: bool,
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
    #[cfg(feature = "cast-mock")]
    mock: Option<crate::chromecast::mock::Mock>,
}

enum Pending {
    Volume(f32),
    Fullscreen,
}

impl CastState {
    /// A target is set: plays go to it.
    pub fn active(&self) -> bool {
        self.remote.is_some()
    }

    pub fn kind(&self) -> Kind {
        self.remote.as_ref().map_or(Kind::Local, Remote::kind)
    }

    /// The choice the target is; this device with none.
    pub fn target(&self) -> Target {
        self.remote.as_ref().map_or(Target::Local, |remote| remote.target().choice())
    }

    pub fn device_name(&self) -> String {
        self.target().name()
    }

    /// The Jellyfin session that is the target, when one is.
    pub fn session(&self) -> Option<&SessionInfo> {
        self.jellyfin().map(|remote| &remote.session)
    }

    fn jellyfin(&self) -> Option<&JellyfinRemote> {
        match &self.remote {
            Some(Remote::Jellyfin(remote)) => Some(remote),
            _ => None,
        }
    }

    fn jellyfin_mut(&mut self) -> Option<&mut JellyfinRemote> {
        match &mut self.remote {
            Some(Remote::Jellyfin(remote)) => Some(remote),
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
            move |client| client.post_capabilities(&body),
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
            player_open: self.playback.open,
            paused: self.playback.status.paused,
            volume: self.playback.volume,
            muted: self.playback.muted,
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
                if self.playback.muted != muted {
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
        let playing = self.playback.open && self.playback.playing.is_some();
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
                    .playback.volume_slider
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
        let wanted = self.panel_is(Panel::Cast) && !self.cast.active() && self.airplay_row_shown();
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
        self.cast.remote.as_ref().map_or_else(TargetView::local, |remote| remote.target().view())
    }

    /// Makes `chosen` the target and leaves the one before. Local playback
    /// stops: one item plays at a time, on the target.
    pub fn select_target(&mut self, chosen: Target, cx: &mut Context<Self>) {
        let same = self.cast.target().same(&chosen);
        self.cast.target_chosen = self.next_revision();
        self.cast.error = None;
        match chosen {
            // The same device again: a Jellyfin session comes with its
            // state of now.
            Target::JellyfinSession(session) if same => {
                if let Some(remote) = self.cast.jellyfin_mut() {
                    remote.update(session);
                }
            }
            _ if same => {}
            Target::Local => self.leave_remote(cx),
            Target::JellyfinSession(session) => {
                self.leave_remote(cx);
                let name = session.device_name.clone();
                let mut remote = JellyfinRemote::new(session);
                remote.poll = Some(self.cast_poll(cx));
                self.cast.remote = Some(Remote::Jellyfin(remote));
                self.cast_subscribe();
                self.toast("Play On", format!("Playback goes to {name}."), cx);
            }
            Target::Chromecast(device) => {
                self.leave_remote(cx);
                // A connection the debug channel opened to this device
                // becomes the target as it is.
                let remote = match self.chromecast.probe.take() {
                    Some(probe) if probe.device.id == device.id => probe,
                    _ => {
                        self.toast("Play On", format!("Playback goes to {}.", device.name), cx);
                        self.chromecast_connect(device, cx)
                    }
                };
                self.cast.remote = Some(Remote::Chromecast(remote));
            }
            Target::AirPlay => {
                self.leave_remote(cx);
                self.watch_airplay(cx);
                // The engine may hold an item already (the debug channel
                // sends one): it is the one the app plays.
                let remote = AirPlayRemote::new(self.airplay.clone(), self.playback.playing.clone());
                self.cast.remote = Some(Remote::AirPlay(remote));
            }
        }
        if self.cast.active() {
            self.start_cast_tick(cx);
        }
        if self.cast.active() && self.playback.open {
            self.player.stop();
            self.close_player_view(cx);
        }
        cx.notify();
    }

    /// Leaves the target that is set: what its kind undoes (the item this
    /// app put on a cast device, the AirPlay engine), its connection and
    /// its poll, which go with it, and the push of the sessions.
    fn leave_remote(&mut self, cx: &mut Context<Self>) {
        if let Some(mut remote) = self.cast.remote.take() {
            let effect = remote.target_mut().leave();
            self.cast_effect(effect, cx);
        }
        if self.cast.subscribed {
            self.socket.send("SessionsStop", None);
            self.cast.subscribed = false;
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
        self.leave_remote(cx);
        self.cast.target_chosen = self.next_revision();
        self.cast.error = None;
        self.chromecast.probe = None;
        cx.notify();
    }

    /// "Play here instead": the item of the target goes on in this player
    /// from where the target was, when that is known.
    pub fn cast_play_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.target_view();
        let item = self.cast.remote.as_ref().and_then(|remote| remote.target().item().cloned()).filter(|_| view.loaded);
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
                let sessions: Vec<SessionInfo> = client.sessions(&[("controllableByUserId", user_id)])?;
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
        if let Some(remote) = self.cast.jellyfin_mut() {
            remote.update(fresh);
        }
    }

    /// Asks the server to push the sessions over the socket.
    fn cast_subscribe(&mut self) {
        if self.socket.send("SessionsStart", Some(Value::String(SUBSCRIPTION.into()))) {
            self.cast.subscribed = true;
        }
    }

    /// Reads the sessions on a timer while a session is the target; the
    /// target keeps the task and ends it when it goes.
    fn cast_poll(&mut self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
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
        })
    }

    /// Sends items to the target instead of the player. False when there
    /// is no target: the caller plays them itself.
    pub fn cast_play(&mut self, items: &[Item], resume: bool, cx: &mut Context<Self>) -> bool {
        let (Some(remote), Some(account), Some(first)) = (&mut self.cast.remote, &self.session, items.first()) else {
            return self.cast.active();
        };
        let effect = remote.target_mut().play(Play {
            items,
            start_secs: if resume { first.resume_secs() as f64 } else { 0. },
            resume,
            client: &account.client,
            server_id: &account.server_id,
            max_bitrate: self.config.max_bitrate,
        });
        self.cast_effect(effect, cx);
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
        let Some(remote) = self.cast.jellyfin() else { return };
        let effect = remote.send_play(item_ids, command, start_secs);
        self.cast_effect(effect, cx);
    }

    /// A general command for a Jellyfin session, with its arguments.
    pub fn cast_general(&mut self, name: &str, arguments: &[(&str, String)], cx: &mut Context<Self>) {
        let Some(remote) = self.cast.jellyfin_mut() else { return };
        let effect = remote.general(name, arguments);
        self.cast_effect(effect, cx);
    }

    /// Does what a control of a target left for the app: says its toast,
    /// and sends its request to the server off the UI thread; a failure
    /// of that shows as a toast.
    fn cast_effect(&mut self, effect: Effect, cx: &mut Context<Self>) {
        if let Some(item) = effect.playing {
            self.playback.playing = Some(item);
        }
        if let Some(text) = effect.toast {
            self.toast("Play On", text, cx);
        }
        if let Some(request) = effect.request {
            let done = request.done;
            self.fetch(cx, request.work, move |this, result, cx| {
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
        }
        cx.notify();
    }

    // ----- the controls of the panel, for every kind of target ------------

    /// Runs a control on the target that is set; nothing with none.
    fn target_do(&mut self, control: impl FnOnce(&mut dyn RemoteTarget) -> Effect, cx: &mut Context<Self>) {
        let Some(remote) = &mut self.cast.remote else { return };
        let effect = control(remote.target_mut());
        self.cast_effect(effect, cx);
    }

    pub fn target_toggle_pause(&mut self, cx: &mut Context<Self>) {
        let paused = self.target_view().paused;
        self.target_set_paused(!paused, cx);
    }

    pub fn target_set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        self.target_do(|target| target.set_paused(paused), cx);
    }

    pub fn target_seek(&mut self, secs: f64, cx: &mut Context<Self>) {
        self.target_do(|target| target.seek(secs.max(0.)), cx);
    }

    pub fn target_stop(&mut self, cx: &mut Context<Self>) {
        self.target_do(|target| target.stop(), cx);
    }

    /// Next or previous in the queue of the target; only a Jellyfin
    /// session has one.
    pub fn target_skip(&mut self, next: bool, cx: &mut Context<Self>) {
        self.target_do(|target| target.skip(next), cx);
    }

    /// 0 to 100.
    pub fn target_set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.target_do(|target| target.set_volume(volume.clamp(0., 100.)), cx);
    }

    pub fn target_toggle_mute(&mut self, cx: &mut Context<Self>) {
        let muted = self.target_view().muted;
        self.target_do(|target| target.set_muted(!muted), cx);
    }

    /// A stream of the item by its index on the server; -1 turns subtitles
    /// off.
    pub fn target_set_track(&mut self, track: Track, index: i64, cx: &mut Context<Self>) {
        self.target_do(|target| target.set_track(track, index), cx);
    }

    // ----- cast devices -----------------------------------------------------

    /// Starts the search for cast devices, unless it runs. The panel says
    /// "Searching…" for a moment after.
    pub fn start_cast_search(&mut self, cx: &mut Context<Self>) {
        if self.chromecast.discovery.is_some() {
            return;
        }
        self.chromecast.discovery = Some(discovery::discover());
        self.cast.search_started = Some(Instant::now());
        #[cfg(feature = "cast-mock")]
        {
            if crate::options::options().chromecast_mock && self.cast.mock.is_none() {
                let mock = crate::chromecast::mock::Mock::start();
                log::info!("cast: mock device on 127.0.0.1:{}", mock.port);
                self.cast.mock = Some(mock);
            }
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
                    let (panel, kind) = (this.panel_is(Panel::Cast), this.cast.kind());
                    if !panel && kind != Kind::Chromecast {
                        this.stop_cast_search();
                    }
                    // The browse for AirPlay receivers and the route
                    // detection of the engine listen on the network too;
                    // the panel starts them again when it opens.
                    if !panel && kind != Kind::AirPlay && this.airplay.browsing() {
                        this.airplay.stop_routes();
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
        let mut devices: Vec<Device> = self.chromecast.discovery.as_ref().map(discovery::Discovery::devices).unwrap_or_default();
        #[cfg(feature = "cast-mock")]
        {
            if let Some(mock) = &self.cast.mock {
                devices.push(mock.device());
            }
        }
        devices.sort_by_cached_key(|device| device.name.to_lowercase());
        devices
    }

    /// Opens a connection to the device and reads its state; nothing on
    /// the device changes until a play or a control. The certificate the
    /// device showed before, when one is kept, is the one it must show.
    fn chromecast_connect(&mut self, device: Device, cx: &mut Context<Self>) -> ChromecastRemote {
        let pin = self.config.cast_pins.get(&device.id).and_then(|kept| BASE64.decode(kept).ok());
        let (session, events) = Session::connect(device.clone(), pin);
        self.chromecast.events.clear();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let fed = this.update(cx, |this, cx| this.on_cast_event(event, cx));
                if fed.is_err() {
                    break;
                }
            }
        });
        ChromecastRemote { device, session, loaded: CastLoad::default(), _task: task }
    }

    /// The connection to a cast device that is open: the one of the
    /// target, or the one the debug channel opened with no target.
    pub(crate) fn chromecast_link(&self) -> Option<&ChromecastRemote> {
        match &self.cast.remote {
            Some(Remote::Chromecast(remote)) => Some(remote),
            _ => self.chromecast.probe.as_ref(),
        }
    }

    fn chromecast_link_mut(&mut self) -> Option<&mut ChromecastRemote> {
        match &mut self.cast.remote {
            Some(Remote::Chromecast(remote)) => Some(remote),
            _ => self.chromecast.probe.as_mut(),
        }
    }

    /// For the debug channel: a connection to the device with no target,
    /// to look at it. False when a connection to this device is open
    /// already (the target's as well).
    pub(crate) fn chromecast_open(&mut self, device: Device, cx: &mut Context<Self>) -> bool {
        if self.chromecast_link().is_some_and(|open| open.device.id == device.id) {
            return false;
        }
        self.chromecast.probe = Some(self.chromecast_connect(device, cx));
        true
    }

    /// An event of the cast connection: the status moves the panel, and
    /// the connection that is open keeps what the event says of its item.
    fn on_cast_event(&mut self, event: CastEvent, cx: &mut Context<Self>) {
        self.chromecast.note(&event);
        match &event {
            CastEvent::Error(text) => {
                log::warn!("cast: {text}");
                self.cast.error = Some(text.clone());
            }
            CastEvent::Pinned(certificate) => self.keep_cast_pin(certificate, cx),
            _ => {}
        }
        if let Some(open) = self.chromecast_link_mut() {
            open.loaded.note(&event);
        }
        cx.notify();
    }

    /// Keeps the certificate the cast device showed with the config, by
    /// the id of the device: the next connection takes that one only. The
    /// fake device of a test (on this Mac) is not kept.
    fn keep_cast_pin(&mut self, certificate: &[u8], cx: &mut Context<Self>) {
        let Some(device) = self.chromecast_link().map(|open| &open.device) else { return };
        if device.address.is_loopback() {
            return;
        }
        let (id, kept) = (device.id.clone(), BASE64.encode(certificate));
        if self.config.cast_pins.get(&id) != Some(&kept) {
            log::info!("cast: the certificate of {:?} is kept", device.name);
            self.config.cast_pins.insert(id, kept);
            self.save_config(cx);
        }
    }

    /// For the debug channel: plays items on the cast device that is
    /// open, the target or not.
    pub(crate) fn chromecast_load(&mut self, items: &[Item], start_secs: f64, cx: &mut Context<Self>) {
        let max_bitrate = self.config.max_bitrate;
        let effect = match (&self.session, &mut self.cast.remote, &mut self.chromecast.probe) {
            (Some(account), Some(Remote::Chromecast(open)), _) | (Some(account), _, Some(open)) => open.play(Play {
                items,
                start_secs,
                resume: start_secs > 0.,
                client: &account.client,
                server_id: &account.server_id,
                max_bitrate,
            }),
            _ => {
                self.cast.error = Some("not connected to the device".into());
                Effect::default()
            }
        };
        self.cast_effect(effect, cx);
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
        let before = self.cast.kind();
        let after = self.cast.target().after_airplay(&event, &status).kind();
        if after == Kind::AirPlay && before != Kind::AirPlay {
            log::info!("airplay: the target");
            self.select_target(Target::AirPlay, cx);
        } else if after != before {
            // The engine ended it itself: nothing is left to undo.
            log::info!("airplay: ended; back to this device");
            self.cast.remote = None;
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
    #[cfg(feature = "cast-mock")]
    #[gpui_kit::test]
    fn cast_off_leaves_nothing_of_the_target(cx: &mut TestAppContext) {
        use crate::cast::target::{Kind, Target};
        let mock = crate::chromecast::mock::Mock::start();
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.select_target(Target::Chromecast(mock.device()), cx);
            this.cast.error = Some("old".into());
            assert_eq!(this.cast.kind(), Kind::Chromecast);
            assert!(this.chromecast_link().is_some());
            this.cast_off(cx);
            assert!(!this.cast.active() && this.cast.target() == Target::Local);
            assert!(this.chromecast_link().is_none());
            assert!(this.cast.error.is_none());
            // A connection with no target (the debug channel opens one)
            // goes as well.
            assert!(this.chromecast_open(mock.device(), cx));
            assert!(this.chromecast_link().is_some() && !this.cast.active());
            this.cast_off(cx);
            assert!(this.chromecast_link().is_none());
        });
    }

    /// A connection the debug channel opened to a device becomes the
    /// target when the device is chosen: one connection, not two.
    #[cfg(feature = "cast-mock")]
    #[gpui_kit::test]
    fn a_connection_with_no_target_becomes_the_target(cx: &mut TestAppContext) {
        use crate::cast::target::{Kind, Target};
        let mock = crate::chromecast::mock::Mock::start();
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            assert!(this.chromecast_open(mock.device(), cx));
            assert!(!this.chromecast_open(mock.device(), cx), "a second connection to the same device");
            this.select_target(Target::Chromecast(mock.device()), cx);
            assert_eq!(this.cast.kind(), Kind::Chromecast);
            assert!(this.chromecast.probe.is_none());
            assert!(!this.chromecast_open(mock.device(), cx), "the target has the connection");
            this.cast_off(cx);
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
