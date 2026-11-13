// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Embedded playback with libmpv. One mpv core lives on a worker thread
//! ("mpv"), which controls it; video is rendered with OpenGL into GPU
//! surfaces that the UI composites, on a thread of its own ("render") that
//! never waits for the core: mpv's video output waits for the render thread
//! at each frame, and a render thread that waited for the core would hold
//! the core, the output and itself for mpv's 200 ms timeout (`render.h`,
//! "Threading"; measured as scheduled actions 40 to 140 ms late).
//! Progress is reported back to Jellyfin from a thread of its own, so a slow
//! server does not hold up the playback loop. The timing of the frames is
//! described in `pacing.rs`.

// On iOS mpv draws into the video layer of the window itself (gpu-next
// over MoltenVK); the render thread and the GL surfaces are the Mac's,
// and what only they used is unused there.
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use crate::stream::StreamApi as _;
use std::{
    ffi::c_void,
    path::PathBuf,
    ptr,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use libmpv2::{
    Format, Mpv,
    events::{Event, PropertyData},
    mpv_end_file_reason,
};
use libmpv2_sys as sys;
use serde::Deserialize;

use crate::{
    jellyfin::{Client, Progress, TICKS_PER_SECOND},
    pacing::{self, DisplayClock},
    video_surface::VideoFrame,
};
#[cfg(target_os = "macos")]
use crate::video_surface::{self, GlRenderer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PlayState {
    #[default]
    Idle,
    /// A file has been requested and is being opened.
    Starting,
    Playing,
    /// Playback finished; the UI should refresh and then acknowledge back to Idle.
    Ended,
}

/// The type of a track in mpv's `track-list`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    /// Subtitles; mpv's name is "sub".
    Sub,
    #[serde(other)]
    Other,
}

/// mpv's name of the type, as the debug channel prints it.
impl std::fmt::Display for TrackKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
            TrackKind::Sub => "sub",
            TrackKind::Other => "other",
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Track {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: TrackKind,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub forced: bool,
    #[serde(default)]
    pub external: bool,
}

impl Track {
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(title) = &self.title {
            parts.push(title.clone());
        }
        if let Some(lang) = &self.lang {
            parts.push(lang.to_uppercase());
        }
        if let Some(codec) = &self.codec {
            parts.push(codec.to_uppercase());
        }
        if self.forced {
            parts.push("forced".into());
        }
        if parts.is_empty() {
            format!("Track {}", self.id)
        } else {
            parts.join(" · ")
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlayerStatus {
    pub state: PlayState,
    pub title: String,
    pub position: f64,
    /// When `position` was measured; none before the first measurement.
    pub position_at: Option<Instant>,
    pub duration: f64,
    pub paused: bool,
    pub buffering: bool,
    pub tracks: Vec<Track>,
    /// Bumped whenever `tracks` changes so menus can be rebuilt lazily.
    pub tracks_version: u64,
    pub error: Option<String>,
    /// The load failed because the server no longer has the item (it
    /// answered 404): the app may look for the item's replacement.
    pub item_gone: bool,
    /// The file played to its end; a stop by the user leaves this false.
    pub reached_end: bool,
    /// What mpv opened: the URL of the stream, or the path of a local file.
    pub path: Option<String>,
    /// Size of the video mpv decodes; 0 before the first frame.
    pub video_w: u32,
    pub video_h: u32,
    /// The subtitle and audio delay in seconds, as mpv reports them.
    pub sub_delay: f64,
    pub audio_delay: f64,
    /// The id mpv gave each subtitle file added to the item, by server
    /// index. mpv numbers a file as it is added, so the menus map a file
    /// by this and not by its place in a list: a file that did not come
    /// does not shift the ids of the ones after it.
    pub external_sids: Vec<(i64, i64)>,
    /// The server indexes of the subtitle files on their way.
    pub pending_subs: Vec<i64>,
    /// Files that were to start shown and did not come: the app burns
    /// the track in instead (`Player::take_failed_subtitles`).
    pub failed_subs: Vec<i64>,
    /// The load this status is of (`Player::prepare`); 0 at rest. The
    /// worker writes what is of an item only into the status of that
    /// item's load: the end of the item before must not end a load that
    /// was prepared since.
    pub generation: u64,
}

/// A subtitle in a file of its own, added to the item once it is loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleFile {
    /// The server index of the stream.
    pub index: i64,
    pub url: String,
    pub title: String,
    pub lang: String,
    /// Starts shown.
    pub select: bool,
    /// An image track (PGS, VobSub): tens of MB, so it is fetched when it
    /// is chosen and not at the start, unless it starts shown.
    pub graphical: bool,
}

pub struct PlayRequest {
    pub client: Client,
    pub item_id: String,
    pub url: String,
    pub title: String,
    pub start_secs: f64,
    /// Load the item and stay paused at the start position.
    pub paused: bool,
    /// Comes back in the `Loaded` or `LoadFailed` event of this request.
    pub token: u64,
    /// The play session the server gave; the worker makes one otherwise.
    pub play_session_id: Option<String>,
    /// The server makes the stream: a transcode, or a remux, which the
    /// reports name "DirectStream". Such a stream is ended on the server
    /// when the item stops, and has the one audio track the server chose.
    pub transcoding: bool,
    /// "DirectPlay", "DirectStream" or "Transcode", for the reports. A
    /// transcode is ended on the server when the item stops.
    pub play_method: String,
    pub media_source_id: String,
    pub subtitles: Vec<SubtitleFile>,
}

/// What the worker tells the app, at the moment it happens. The status is
/// only a picture of the last state; these do not wait for a reader of it.
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerEvent {
    /// The item of a request is loaded and can play.
    Loaded { token: u64 },
    LoadFailed { token: u64 },
    /// An exact seek is done and the player can play from its position.
    Settled { token: u64 },
    /// The player ran out of data while it played, for some seconds.
    Stalled,
    /// The player has data again after a `Stalled`.
    Recovered,
    /// A measurement of the position; see [`Player::follow`].
    Position(Sample),
    /// The item played to its end.
    Ended,
    /// A scheduled action ran, this long after its time.
    ScheduledFired { late: Duration },
}

/// The position of the player at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Seconds into the item, by the audio that plays right now.
    pub position: f64,
    pub at: Instant,
    pub paused: bool,
    /// Length of the item in seconds; 0 when not known.
    pub duration: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScheduledAction {
    Unpause,
    /// Pause, then go to a position in seconds.
    PauseThenSeek(f64),
}

/// An action that runs on the worker at a set time, so no other thread and
/// no poll is between the clock and the player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scheduled {
    pub at: Instant,
    pub action: ScheduledAction,
}

enum Cmd {
    /// Loads an item; `generation` names the prepared load it answers, and
    /// the worker refuses it when a stop or a later load came since
    /// (`Player::stop`).
    ///
    /// `tracks` are the track choices that waited for the first worker
    /// (`Queued`). They travel in the load, not ahead of it: the worker
    /// applies them only when it accepts the load, so the choice made for
    /// a cancelled load never reaches the item after it.
    Load { generation: u64, request: Box<PlayRequest>, tracks: Tracks },
    /// Ends the worker, so mpv is closed before the process exits: at the
    /// quit of the app, and at the end of a test.
    Quit,
    TogglePause,
    SetPaused(bool),
    SeekRelative(f64),
    SeekAbsolute(f64),
    /// Seeks to the exact position, paused when asked, and reports
    /// `Settled` with the token when the player can play from there.
    SeekExact { position: f64, pause: bool, token: u64 },
    /// Sets the one scheduled action, or clears it.
    Schedule(Option<Scheduled>),
    /// A factor on top of the speed the user chose.
    SetSyncSpeed(f64),
    /// Sends position measurements while on.
    Follow(bool),
    SetAudio(Option<i64>),
    SetSubtitle(Option<i64>),
    SetVolume(f64),
    SetMuted(bool),
    SetSpeed(f64),
    /// Copies the video frame for the glass over it every tenth of a
    /// second while on (iOS; see `glass::set_video_backdrop`).
    Backdrop(bool),
    /// Sets an mpv property by name.
    SetProperty(String, String),
    /// Runs an mpv command with its arguments.
    Command(String, Vec<String>),
    /// In a SyncPlay group or not; see `pacing::video_sync`.
    SetGroup(bool),
    /// Stops the item, with the load count at the stop (see [`Player::stop`]).
    Stop(u64),
    /// A subtitle file of the load `generation` came (see `fetch_subtitles`).
    AddSubtitle { generation: u64, index: i64, path: PathBuf, select: bool, title: String, lang: String },
    /// Fetches one subtitle file of the item that plays and shows it when
    /// it comes (an image track the user chose).
    FetchSubtitle(SubtitleFile),
    /// Reads an mpv property on the worker (`dev/jctl mpv <name>`, tests).
    Probe(String, mpsc::Sender<String>),
}

/// The track choices that go with a load; `Some(None)` turns a kind off.
#[derive(Default)]
struct Tracks {
    audio: Option<Option<i64>>,
    subtitle: Option<Option<i64>>,
}

/// Chooses a track of mpv by its id; none turns the kind off.
fn set_track(mpv: &Mpv, property: &str, id: Option<i64>) {
    let value = id.map(|i| i.to_string()).unwrap_or_else(|| "no".into());
    let _ = mpv.set_property(property, value);
}

/// Settings sent before the first worker exists: the app sends them while
/// the server is still asked for the first item. The worker gets them
/// before its first load, each at its latest value. A choice of a track
/// is for one load: it goes with the load it was made for and not with a
/// later one, when that load was cancelled.
#[derive(Default)]
struct Queued {
    volume: Option<f64>,
    muted: Option<bool>,
    speed: Option<f64>,
    follow: Option<bool>,
    /// By name, in the order first set.
    properties: Vec<(String, String)>,
    /// The track choices, each with the load count when it was made.
    audio: Option<(u64, Option<i64>)>,
    subtitle: Option<(u64, Option<i64>)>,
}

impl Queued {
    /// Keeps a setting for the first worker; false for a command that is
    /// not a setting (it needs a worker and is dropped).
    fn keep(&mut self, cmd: Cmd, load_gen: u64) -> bool {
        match cmd {
            Cmd::SetVolume(v) => self.volume = Some(v),
            Cmd::SetMuted(m) => self.muted = Some(m),
            Cmd::SetSpeed(s) => self.speed = Some(s),
            Cmd::Follow(on) => self.follow = Some(on),
            Cmd::SetProperty(name, value) => match self.properties.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = value,
                None => self.properties.push((name, value)),
            },
            Cmd::SetAudio(id) => self.audio = Some((load_gen, id)),
            Cmd::SetSubtitle(id) => self.subtitle = Some((load_gen, id)),
            // `SetGroup` is read from `Shared::in_group` at each load.
            _ => return false,
        }
        true
    }

    /// The commands for a worker that starts for the load `load_gen`: the
    /// settings, which the worker gets at once, and the track choices of
    /// that load, which go inside its `Cmd::Load`.
    fn take(&mut self, load_gen: u64) -> (Vec<Cmd>, Tracks) {
        let mut cmds = Vec::new();
        if let Some(v) = self.volume.take() {
            cmds.push(Cmd::SetVolume(v));
        }
        if let Some(m) = self.muted.take() {
            cmds.push(Cmd::SetMuted(m));
        }
        if let Some(s) = self.speed.take() {
            cmds.push(Cmd::SetSpeed(s));
        }
        if let Some(on) = self.follow.take() {
            cmds.push(Cmd::Follow(on));
        }
        cmds.extend(self.properties.drain(..).map(|(n, v)| Cmd::SetProperty(n, v)));
        let of_load = |choice: Option<(u64, Option<i64>)>| {
            choice.filter(|(generation, _)| *generation == load_gen).map(|(_, id)| id)
        };
        let tracks = Tracks { audio: of_load(self.audio.take()), subtitle: of_load(self.subtitle.take()) };
        (cmds, tracks)
    }
}

/// The newest frame as the render thread published it: the picture, its
/// number and the tick it was made after, as one. The UI reads the three
/// as one too (`Bloom::sync_frame`), so the tick of one frame is never
/// paired with the picture of the next.
#[derive(Clone, Default)]
pub struct Published {
    /// Counts up with each frame, and once more at `acknowledge_end`.
    pub seq: u64,
    /// The time of the tick the frame was made after, in `clock_ns`; the
    /// UI shows it one tick later (`pacing::due`). 0 for a frame with
    /// audio timing.
    pub tick_ns: u64,
    /// The refresh that tick was for (its `outputTime`), in `clock_ns`;
    /// 0 with audio timing. The phase loop measures each frame against it.
    pub vsync_ns: u64,
    pub frame: Option<VideoFrame>,
}

struct Shared {
    status: Mutex<PlayerStatus>,
    /// `Player::set_backdrop` sends a change alone.
    backdrop_wanted: AtomicBool,
    frame: Mutex<Published>,
    /// `Published::seq`, for a check without the lock. Written under the
    /// `frame` lock only, so it never goes backwards.
    frame_seq: AtomicU64,
    target_w: AtomicU32,
    target_h: AtomicU32,
    /// Counts the loads, so a load that waited on the server does not
    /// replace a later one (see [`Player::prepare`]).
    load_gen: AtomicU64,
    /// Wakes the worker: mpv has an event or a frame, or a command came.
    wake: (Mutex<bool>, Condvar),
    events: (
        async_channel::Sender<PlayerEvent>,
        async_channel::Receiver<PlayerEvent>,
    ),
    /// One signal for each new frame (several frames while nobody reads
    /// become one). The UI draws at once on it, not at its next poll: a
    /// wait of some milliseconds here made the times between frames uneven.
    frames: (async_channel::Sender<()>, async_channel::Receiver<()>),
    /// When the newest frame was rendered.
    frame_done_ns: AtomicU64,
    /// The size of the picture mpv shows (`dwidth`, `dheight`), from the
    /// worker to the render thread; 0 before the first frame.
    dwidth: AtomicU32,
    dheight: AtomicU32,
    /// `clock_ns` of the last draw of the UI; 0 before the first one.
    ui_seen_ns: AtomicU64,
    /// The display the window is on, as the UI last saw it; 0 for none.
    display: AtomicU32,
    /// In a SyncPlay group: read at each load, as the group can be joined
    /// before the worker runs.
    in_group: AtomicBool,
    /// Something reads the colours of the picture (the events for other
    /// programs): the render thread then looks at a frame some times a
    /// second. `colours` has the count of the looks and the newest bands,
    /// from the top of the picture to its bottom.
    colours_on: AtomicBool,
    colours_ns: AtomicU64,
    colours: Mutex<(u64, Vec<[u8; 3]>)>,
    /// The display link, once the worker has started it (display pacing).
    clock: Mutex<Option<DisplayClock>>,
    /// What that link writes at each tick, for a read without the `clock`
    /// lock (each draw and each frame reads it); null while no link is
    /// there. The state is never freed (`DisplayClock::start`).
    clock_state: AtomicPtr<pacing::ClockState>,
    /// The subtitle files on their way (see `fetch_subtitles`).
    fetching: Mutex<Vec<Fetch>>,
    /// The factor of the group on the speed, as last asked for (the bits
    /// of the `f64`): for `Player::sync_state`.
    sync_speed: AtomicU64,
}

/// A subtitle file on its way: the load it is of, its server index, and
/// whether it is shown when it comes. A choice of the file while it is on
/// its way sets `select`; it is not fetched a second time.
struct Fetch {
    generation: u64,
    index: i64,
    select: bool,
}

/// What mpv's update callback gets: the flag for the worker, and the
/// worker's wake-up, so a frame is rendered when mpv asks, not up to 4 ms
/// later.
struct RenderSignal {
    needed: AtomicBool,
    shared: Arc<Shared>,
    /// When mpv asked for the frame that waits, in nanoseconds of `clock_ns`.
    asked_ns: AtomicU64,
    /// The display link ticked: the render thread reports the swap to mpv.
    swap_due: AtomicBool,
    /// The size of the picture came after a frame was skipped for want of
    /// it: the render thread draws the frame mpv has once more.
    redraw: AtomicBool,
    /// The worker ends: the render thread frees mpv's render context.
    quit: AtomicBool,
    /// Wakes the render thread.
    wake: (Mutex<bool>, Condvar),
}

impl RenderSignal {
    fn wake(&self) {
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_one();
    }
}

/// The render thread, ended and joined when the worker ends, before the
/// core goes (see `render_loop`).
#[cfg(target_os = "macos")]
struct RenderThread {
    handle: Option<thread::JoinHandle<()>>,
    signal: &'static RenderSignal,
}

#[cfg(target_os = "macos")]
impl RenderThread {
    /// Starts the thread and waits until mpv's render context exists, so
    /// a load that follows has a video output.
    fn start(mpv_handle: usize, signal: &'static RenderSignal) -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("render".into())
            .spawn(move || render_loop(mpv_handle, signal, ready_tx))
            .expect("spawn render thread");
        ready_rx
            .recv()
            .map_err(|_| anyhow!("the render thread ended before it was ready"))??;
        Ok(Self { handle: Some(handle), signal })
    }
}

#[cfg(target_os = "macos")]
impl Drop for RenderThread {
    fn drop(&mut self) {
        self.signal.quit.store(true, Ordering::Release);
        self.signal.wake();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Nanoseconds since the first call, for times that cross threads.
pub(crate) fn clock_ns() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

impl Shared {
    fn wake(&self) {
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_one();
    }

    /// Publishes a frame (or none, at the end of playback) with its tick
    /// as the next one; returns its number. The number goes to the atomic
    /// under the same lock, so a reader of the atomic and a reader of the
    /// snapshot agree on the order of the frames.
    fn publish(&self, frame: Option<VideoFrame>, tick_ns: u64, vsync_ns: u64) -> u64 {
        let mut published = self.frame.lock().unwrap();
        published.seq += 1;
        published.tick_ns = tick_ns;
        published.vsync_ns = vsync_ns;
        published.frame = frame;
        self.frame_seq.store(published.seq, Ordering::Release);
        published.seq
    }

    /// What the display link wrote last; none while no link is there.
    fn clock_state(&self) -> Option<&'static pacing::ClockState> {
        // SAFETY: null, or the state a `DisplayClock` leaked at its start.
        unsafe { self.clock_state.load(Ordering::Acquire).as_ref() }
    }

    /// Events nobody reads are dropped when the channel is full.
    fn emit(&self, event: PlayerEvent) {
        let _ = self.events.0.try_send(event);
    }
}

/// A display link that ticked within this time runs (`Player::ui_seen`).
const LINK_TICKS_WITHIN_NS: u64 = 100_000_000;

/// The properties of mpv's display sync that `dev/jctl pacing clock` shows,
/// each with its name in the line.
const SYNC_INFO: [(&str, &str); 9] = [
    ("video-sync", "video-sync"),
    ("active", "display-sync-active"),
    ("display-fps", "display-fps"),
    ("estimated", "estimated-display-fps"),
    ("vsync-ratio", "vsync-ratio"),
    ("jitter", "vsync-jitter"),
    ("mistimed", "mistimed-frame-count"),
    ("delayed", "vo-delayed-frame-count"),
    ("dropped", "frame-drop-count"),
];

/// Shared handle owned by the UI. Cheap to clone.
#[derive(Clone)]
pub struct Player {
    shared: Arc<Shared>,
    commands: Arc<Mutex<Option<mpsc::Sender<Cmd>>>>,
    /// Settings sent before the first worker exists (see [`Queued`]).
    queued: Arc<Mutex<Queued>>,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                status: Mutex::new(PlayerStatus::default()),
                backdrop_wanted: AtomicBool::new(false),
                frame: Mutex::new(Published::default()),
                frame_seq: AtomicU64::new(0),
                target_w: AtomicU32::new(1280),
                target_h: AtomicU32::new(720),
                load_gen: AtomicU64::new(0),
                wake: (Mutex::new(false), Condvar::new()),
                events: async_channel::bounded(256),
                frames: async_channel::bounded(1),
                frame_done_ns: AtomicU64::new(0),
                dwidth: AtomicU32::new(0),
                dheight: AtomicU32::new(0),
                ui_seen_ns: AtomicU64::new(0),
                display: AtomicU32::new(0),
                in_group: AtomicBool::new(false),
                colours_on: AtomicBool::new(false),
                colours_ns: AtomicU64::new(0),
                colours: Mutex::new((0, Vec::new())),
                clock: Mutex::new(None),
                clock_state: AtomicPtr::new(ptr::null_mut()),
                fetching: Mutex::new(Vec::new()),
                sync_speed: AtomicU64::new(1f64.to_bits()),
            }),
            commands: Arc::new(Mutex::new(None)),
            queued: Arc::new(Mutex::new(Queued::default())),
        }
    }
}

impl Player {
    pub fn status(&self) -> PlayerStatus {
        self.shared.status.lock().unwrap().clone()
    }

    /// The newest frame with its number and its tick, read as one.
    pub fn frame(&self) -> Published {
        self.shared.frame.lock().unwrap().clone()
    }

    /// The number and the tick of the newest frame, without the picture.
    pub fn frame_timing(&self) -> (u64, u64) {
        let published = self.shared.frame.lock().unwrap();
        (published.seq, published.tick_ns)
    }

    /// The number of the newest frame; a cheap check for a new one.
    pub fn frame_seq(&self) -> u64 {
        self.shared.frame_seq.load(Ordering::Acquire)
    }

    /// Tells the renderer how large the video area is, in device pixels.
    pub fn set_target_size(&self, width: u32, height: u32) {
        self.shared.target_w.store(width.max(16), Ordering::Relaxed);
        self.shared
            .target_h
            .store(height.max(16), Ordering::Relaxed);
    }

    pub fn acknowledge_end(&self) {
        let mut status = self.shared.status.lock().unwrap();
        if status.state == PlayState::Ended {
            *status = PlayerStatus::default();
        }
        self.shared.publish(None, 0, 0);
    }

    /// The events of the worker. Every receiver takes from one queue, so
    /// one reader must own it.
    pub fn events(&self) -> async_channel::Receiver<PlayerEvent> {
        self.shared.events.1.clone()
    }

    /// How long ago the newest frame was rendered, in milliseconds.
    pub fn frame_wait_ms(&self) -> f64 {
        let done = self.shared.frame_done_ns.load(Ordering::Acquire);
        clock_ns().saturating_sub(done) as f64 / 1e6
    }

    /// A signal for each new video frame.
    pub fn frames(&self) -> async_channel::Receiver<()> {
        self.shared.frames.1.clone()
    }

    /// The time now and the period of the display (a 60 Hz one before the
    /// first tick), in nanoseconds of `clock_ns`. For `pacing::due` and
    /// `pacing::frame_phase` with the tick of a [`Published`] frame.
    pub fn clock_time(&self) -> (u64, u64) {
        let period = self.shared.clock_state().map_or(0, |state| state.period_ns.load(Ordering::Relaxed));
        (clock_ns(), if period == 0 { 16_666_667 } else { period })
    }

    /// The display drives the frames (see `pacing.rs`), and the clock runs.
    pub fn display_pacing(&self) -> bool {
        self.shared.clock_state().is_some()
    }

    /// The UI draws the frames right now: call it from each draw. The
    /// worker renders no frame for a window that stopped drawing.
    pub fn ui_seen(&self) {
        let now = clock_ns();
        self.shared.ui_seen_ns.store(now, Ordering::Release);
        // Where this draw lands after our tick, for `pacing clock`. A link
        // that is held (nothing plays) has no tick to measure from.
        if let Some(state) = self.shared.clock_state() {
            let since_tick = now.saturating_sub(state.tick_ns.load(Ordering::Acquire));
            if since_tick < LINK_TICKS_WITHIN_NS {
                pacing::note_phase(since_tick);
            }
        }
    }

    /// The display the window is on; the display link follows it.
    pub fn set_display(&self, display: u32) {
        self.shared.display.store(display, Ordering::Relaxed);
    }

    /// Looks at the colours of the picture some times a second, or not.
    pub fn watch_colours(&self, on: bool) {
        // On again after a time off: the colours of the last look are of
        // another moment, and must not be told as those of this one.
        if on && !self.shared.colours_on.swap(on, Ordering::Relaxed) {
            self.shared.colours.lock().unwrap().1.clear();
        } else {
            self.shared.colours_on.store(on, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub fn watches_colours(&self) -> bool {
        self.shared.colours_on.load(Ordering::Relaxed)
    }

    /// Bands as the render thread would leave them, for a test.
    #[cfg(test)]
    pub fn put_colours(&self, bands: Vec<[u8; 3]>) {
        let mut colours = self.shared.colours.lock().unwrap();
        *colours = (colours.0 + 1, bands);
    }

    /// The count of the looks at the picture, and the colours of the
    /// newest one: bands from the top of the picture to its bottom.
    pub fn colours(&self) -> (u64, Vec<[u8; 3]>) {
        self.shared.colours.lock().unwrap().clone()
    }

    /// In a SyncPlay group or not: chooses the video-sync mode of mpv.
    pub fn set_group(&self, in_group: bool) {
        self.shared.in_group.store(in_group, Ordering::Relaxed);
        self.send(Cmd::SetGroup(in_group));
    }

    /// The display link and mpv's display sync, for `dev/jctl pacing clock`.
    /// mpv's part is read through the worker now, when the command asks:
    /// a read waits for the core, so the worker does none by itself. The
    /// caller waits for the answers (`probe`); with no worker, or one that
    /// does not answer the first, the values are `-`.
    pub fn pacing_state(&self) -> String {
        let mut answers = true;
        let sync_info: Vec<String> = SYNC_INFO
            .iter()
            .map(|(label, name)| {
                let value = if answers { self.probe(name) } else { None };
                answers = value.is_some();
                format!("{label}={}", value.filter(|v| !v.is_empty()).unwrap_or_else(|| "-".into()))
            })
            .collect();
        let clock = self
            .shared
            .clock
            .lock()
            .unwrap()
            .as_ref()
            .map_or_else(|| "no display link".to_string(), DisplayClock::describe);
        format!("{clock} | {} | mpv: {}", pacing::report(), sync_info.join(" "))
    }

    pub fn toggle_pause(&self) {
        self.send(Cmd::TogglePause);
    }
    pub fn set_paused(&self, paused: bool) {
        self.send(Cmd::SetPaused(paused));
    }
    pub fn seek_exact(&self, position: f64, pause: bool, token: u64) {
        self.send(Cmd::SeekExact { position, pause, token });
    }
    /// Sets the action that runs at a set time; a new one takes the place
    /// of the one that waits, and `None` clears it.
    pub fn schedule(&self, scheduled: Option<Scheduled>) {
        self.send(Cmd::Schedule(scheduled));
    }
    /// What a group left on the player, for a test: in a group or not,
    /// and the factor on the speed. A new load takes the factor back to 1.
    #[cfg(test)]
    pub fn sync_state(&self) -> (bool, f64) {
        (
            self.shared.in_group.load(Ordering::Relaxed),
            f64::from_bits(self.shared.sync_speed.load(Ordering::Relaxed)),
        )
    }
    /// A factor on the speed the user chose, for small corrections of the
    /// position. It does not show as the speed of the user.
    pub fn set_sync_speed(&self, factor: f64) {
        self.shared.sync_speed.store(factor.to_bits(), Ordering::Relaxed);
        self.send(Cmd::SetSyncSpeed(factor));
    }
    /// Sends a `Position` event about ten times a second while on.
    pub fn follow(&self, on: bool) {
        self.send(Cmd::Follow(on));
    }
    pub fn seek_relative(&self, secs: f64) {
        self.send(Cmd::SeekRelative(secs));
    }
    pub fn seek_absolute(&self, secs: f64) {
        self.send(Cmd::SeekAbsolute(secs));
    }
    pub fn set_audio(&self, id: Option<i64>) {
        self.send(Cmd::SetAudio(id));
    }
    pub fn set_subtitle(&self, id: Option<i64>) {
        self.send(Cmd::SetSubtitle(id));
    }
    /// Fetches a subtitle file of the item that plays, on the side, and
    /// shows it when it comes: an image track the user chose, which was
    /// not fetched at the start.
    pub fn fetch_subtitle_file(&self, file: SubtitleFile) {
        self.send(Cmd::FetchSubtitle(file));
    }
    /// The server indexes of the chosen subtitle files that did not come
    /// since the last call.
    pub fn take_failed_subtitles(&self) -> Vec<i64> {
        std::mem::take(&mut self.shared.status.lock().unwrap().failed_subs)
    }
    /// Volume from 0 to 100.
    pub fn set_volume(&self, volume: f64) {
        self.send(Cmd::SetVolume(volume));
    }
    pub fn set_muted(&self, muted: bool) {
        self.send(Cmd::SetMuted(muted));
    }
    pub fn set_speed(&self, speed: f64) {
        self.send(Cmd::SetSpeed(speed));
    }
    /// Copies of the video frame for the glass over it, while on. A
    /// change alone reaches the worker.
    pub fn set_backdrop(&self, on: bool) {
        if self.shared.backdrop_wanted.swap(on, Ordering::AcqRel) != on {
            self.send(Cmd::Backdrop(on));
            if !on {
                crate::ui::glass::set_video_backdrop(None);
            }
        }
    }
    pub fn set_property(&self, name: &str, value: &str) {
        self.send(Cmd::SetProperty(name.to_string(), value.to_string()));
    }
    pub fn command(&self, name: &str, args: &[&str]) {
        self.send(Cmd::Command(
            name.to_string(),
            args.iter().map(|a| a.to_string()).collect(),
        ));
    }
    /// Stops the item. A load that still waits for the server is cancelled
    /// too: its answer must not start playback behind the stop. The load
    /// count moves on, so `play_prepared` refuses the answer, and so does
    /// the worker for a load that was already on its way to it.
    pub fn stop(&self) {
        let generation = self.shared.load_gen.fetch_add(1, Ordering::AcqRel) + 1;
        if !self.send(Cmd::Stop(generation)) {
            // No worker, so no end of file will come: a load that was only
            // prepared ends here.
            let mut status = self.shared.status.lock().unwrap();
            if status.state == PlayState::Starting {
                status.state = PlayState::Ended;
            }
        }
    }

    /// Starts playback, replacing any running item. The app prepares a
    /// load and plays it in two steps (see `stream`); this is the one step.
    #[allow(dead_code)]
    pub fn play(&self, request: PlayRequest) {
        let generation = self.prepare(&request.title, request.start_secs, request.paused);
        self.play_prepared(generation, request);
    }

    /// Shows an item as opening before its address is known: the status
    /// is `Starting` at the position. The number that comes back names
    /// this load for [`Player::play_prepared`].
    pub fn prepare(&self, title: &str, start_secs: f64, paused: bool) -> u64 {
        let mut status = self.shared.status.lock().unwrap();
        // The delays are of the item; the worker decides on the load.
        let (sub_delay, audio_delay) = (status.sub_delay, status.audio_delay);
        // The count moves under the lock of the status, so the status is
        // always of the newest load.
        let generation = self.shared.load_gen.fetch_add(1, Ordering::AcqRel) + 1;
        *status = PlayerStatus {
            state: PlayState::Starting,
            title: title.to_string(),
            position: start_secs,
            paused,
            sub_delay,
            audio_delay,
            generation,
            ..Default::default()
        };
        generation
    }

    /// Loads the item of a prepared load. False, and nothing happens,
    /// when a later load was prepared since.
    pub fn play_prepared(&self, generation: u64, request: PlayRequest) -> bool {
        if self.shared.load_gen.load(Ordering::Acquire) != generation {
            return false;
        }
        #[cfg(test)]
        tests::hook(tests::At::Checked);
        let token = request.token;
        let tracks = self.ensure_thread(generation);
        #[cfg(test)]
        tests::hook(tests::At::Started);
        // A stop between the check and here moves the count on: the worker
        // refuses the load when it takes it. A worker that failed to start
        // before the load reached it answers here.
        let leftover = Leftover::of(&request);
        if !self.send(Cmd::Load { generation, request: Box::new(request), tracks }) {
            self.shared.emit(PlayerEvent::LoadFailed { token });
            leftover.end();
        }
        true
    }

    /// A prepared load that got no address: the item ends with the error,
    /// unless a later load was prepared since.
    pub fn fail_load(&self, generation: u64, token: u64, message: &str, gone: bool) {
        if self.shared.load_gen.load(Ordering::Acquire) != generation {
            return;
        }
        let mut status = self.shared.status.lock().unwrap();
        status.error = Some(format!("Could not start playback: {message}"));
        status.item_gone = gone;
        status.state = PlayState::Ended;
        self.shared.emit(PlayerEvent::LoadFailed { token });
    }

    /// Gives the command to the worker; false when there is none. A setting
    /// sent with no worker waits for the first one ([`Queued`]).
    fn send(&self, cmd: Cmd) -> bool {
        let slot = self.commands.lock().unwrap();
        match slot.as_ref() {
            Some(tx) => {
                let _ = tx.send(cmd);
                // The worker sleeps between rounds.
                self.shared.wake();
                true
            }
            None => {
                let load_gen = self.shared.load_gen.load(Ordering::Acquire);
                self.queued.lock().unwrap().keep(cmd, load_gen);
                false
            }
        }
    }

    /// The value of an mpv property as the worker sees it; none without a
    /// worker or after two seconds.
    pub fn probe(&self, name: &str) -> Option<String> {
        let (tx, rx) = mpsc::channel();
        if !self.send(Cmd::Probe(name.to_string(), tx)) {
            return None;
        }
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }

    /// Ends the worker and waits for it, up to `wait`. A process that
    /// exits while mpv still runs can crash, so a test with the real
    /// player ends with this; the app quits through [`shut_down_all`].
    #[cfg(test)]
    pub(crate) fn shut_down(&self, wait: Duration) {
        self.send(Cmd::Quit);
        let end = Instant::now() + wait;
        while self.commands.lock().unwrap().is_some() && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Starts the worker when there is none, for the load `load_gen`: the
    /// settings that waited go to it first. Gives back the track choices
    /// that waited for this load, for its `Cmd::Load`.
    fn ensure_thread(&self, load_gen: u64) -> Tracks {
        let mut slot = self.commands.lock().unwrap();
        if slot.is_some() {
            return Tracks::default();
        }
        let (tx, rx) = mpsc::channel();
        let (settings, tracks) = self.queued.lock().unwrap().take(load_gen);
        for cmd in settings {
            let _ = tx.send(cmd);
        }
        let worker_tx = tx.clone();
        *slot = Some(tx);
        {
            let mut players = PLAYERS.lock().unwrap();
            players.retain(|p| !Arc::ptr_eq(&p.commands, &self.commands));
            players.push(self.clone());
        }
        let shared = self.shared.clone();
        let commands = self.commands.clone();
        thread::Builder::new()
            .name("mpv".into())
            .spawn(move || {
                let (reports, reporter) = reporter();
                match run(shared.clone(), &rx, worker_tx, reports) {
                    // The quit: the core is closed by now, and the stop
                    // report of the item that played goes out before the
                    // slot closes, which the quit of the app waits for
                    // (`shut_down_all`, up to `QUIT_WAIT`).
                    Ok(()) => {
                        if let Some(reporter) = reporter {
                            let _ = reporter.join();
                        }
                    }
                    Err(err) => {
                        log::error!("mpv worker failed: {err:#}");
                        let mut status = shared.status.lock().unwrap();
                        status.error = Some(format!("{err:#}"));
                        status.state = PlayState::Ended;
                    }
                }
                // The slot closes under its lock, and the loads still in
                // the channel fail under the same lock: a load that comes
                // after finds no worker and fails in `play_prepared`, one
                // that came before fails here. Each is answered once; a
                // group waits for that answer.
                let mut slot = commands.lock().unwrap();
                *slot = None;
                while let Ok(cmd) = rx.try_recv() {
                    if let Cmd::Load { request, .. } = cmd {
                        shared.emit(PlayerEvent::LoadFailed { token: request.token });
                        Leftover::of(&request).end();
                    }
                }
                drop(slot);
            })
            .expect("spawn mpv thread");
        tracks
    }
}

/// What a load that was refused after the server prepared it leaves on the
/// server: a transcode nobody will play. No session of the worker knows it,
/// so no stop report ends it; it is ended here, off the caller's thread.
struct Leftover {
    client: Client,
    play_session_id: Option<String>,
    transcode: bool,
}

impl Leftover {
    fn of(request: &PlayRequest) -> Self {
        Self {
            client: request.client.clone(),
            play_session_id: request.play_session_id.clone(),
            transcode: request.transcoding,
        }
    }

    fn end(self) {
        let (true, Some(id)) = (self.transcode, self.play_session_id) else { return };
        let client = self.client;
        let _ = thread::Builder::new().name("mpv-refused-load".into()).spawn(move || {
            if let Err(err) = client.stop_encoding(&id) {
                log::debug!("transcode of a refused load not ended: {err:#}");
            }
        });
    }
}

/// The players that started a worker, for [`shut_down_all`].
static PLAYERS: Mutex<Vec<Player>> = Mutex::new(Vec::new());

/// How long the quit of the app waits for the cores to close.
const QUIT_WAIT: Duration = Duration::from_secs(2);

/// Closes every mpv core, the players and the trailer, and waits for them
/// up to `QUIT_WAIT`: for the quit of the app, as a process that exits
/// while a core runs can crash. Each worker frees its render context and
/// then the core (`run`, `trailer::run`).
pub fn shut_down_all() {
    let started = Instant::now();
    let players: Vec<Player> = PLAYERS.lock().unwrap().clone();
    for player in &players {
        player.send(Cmd::Quit);
    }
    crate::trailer::ask_to_quit();
    let running = || {
        players.iter().any(|p| p.commands.lock().unwrap().is_some()) || crate::trailer::running()
    };
    while running() && started.elapsed() < QUIT_WAIT {
        thread::sleep(Duration::from_millis(5));
    }
    if running() {
        log::warn!("an mpv core did not close within {QUIT_WAIT:?}");
    } else {
        log::info!("mpv closed in {} ms", started.elapsed().as_millis());
    }
}

/// The root certificates mpv trusts: Mozilla's bundle, the same roots the
/// REST client has compiled in (`webpki-roots`), so a server the app signs
/// in to also plays, and no other one does. libmpv checks no certificate
/// by itself, and its GnuTLS looks for `/etc/ssl/certs/ca-certificates.crt`,
/// which a Mac does not have. `/etc/ssl/cert.pem` of macOS is a copy of
/// 2021 and refuses, for one, the ISRG Root X2 chain of Let's Encrypt.
const TLS_ROOTS: &[u8] = include_bytes!("../../assets/cacert.pem");

/// The bundle as a file, as GnuTLS reads one: in the cache folder, written
/// at the first core of a start when it changed. The file of the system
/// when the folder cannot be written.
pub(crate) fn tls_roots_file() -> &'static str {
    static FILE: OnceLock<String> = OnceLock::new();
    FILE.get_or_init(|| {
        let write = || -> std::io::Result<String> {
            let dir = dirs::cache_dir()
                .ok_or_else(|| std::io::Error::other("no cache folder"))?
                .join(crate::brand::FOLDER);
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("tls-roots.pem");
            if std::fs::read(&path).ok().as_deref() != Some(TLS_ROOTS) {
                std::fs::write(&path, TLS_ROOTS)?;
            }
            Ok(path.to_string_lossy().into_owned())
        };
        write().unwrap_or_else(|err| {
            log::warn!("could not write the TLS roots for mpv ({err}); using /etc/ssl/cert.pem");
            "/etc/ssl/cert.pem".into()
        })
    })
}

mod render;
mod subtitles;
mod worker;

use self::{render::*, subtitles::*, worker::*};
pub(crate) use self::render::COLOUR_BANDS;
pub(crate) use self::worker::{Report, ReportKind, spawn_reporter};
#[cfg(test)]
pub(crate) use self::worker::FAIL_INIT;

/// One test with the real player at a time: two of them at once decode two
/// videos, and the times they measure in milliseconds go wrong.
#[cfg(test)]
pub(crate) static REAL_PLAYER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The turn of a test with the real player (hold the guard to the end of the
/// test). With `BLOOM_CI_NO_PLAYER` set, as on a hosted CI runner that has no
/// display or audio, the test does not run: it prints one line that starts
/// with `SKIPPED in CI` and returns `None`, and the workflow counts the lines
/// (`dev/ci-test`), so a skipped test is never taken for a passed one.
/// `BLOOM_CI_SKIP_PLAYER_TESTS` skips the same way only the tests it names
/// (last part of the name, separated by commas): those whose timing or
/// picture a virtual machine cannot hold, so the others can be a gate.
#[cfg(test)]
pub(crate) fn real_player_turn() -> Option<std::sync::MutexGuard<'static, ()>> {
    let test = std::thread::current().name().unwrap_or("?").to_owned();
    if std::env::var_os("BLOOM_CI_NO_PLAYER").is_some_and(|v| !v.is_empty()) {
        // The newline first: the test runner prints "test x ..." without one.
        eprintln!("\nSKIPPED in CI (BLOOM_CI_NO_PLAYER is set): {test}");
        return None;
    }
    let short = test.rsplit("::").next().unwrap_or(&test);
    if std::env::var("BLOOM_CI_SKIP_PLAYER_TESTS").is_ok_and(|names| names.split(',').any(|name| name.trim() == short)) {
        eprintln!("\nSKIPPED in CI (BLOOM_CI_SKIP_PLAYER_TESTS names it): {test}");
        return None;
    }
    Some(REAL_PLAYER.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Closes the player of a test when the test ends, also when it fails.
#[cfg(test)]
pub(crate) struct Closer(pub Player);

#[cfg(test)]
impl Drop for Closer {
    fn drop(&mut self) {
        self.0.shut_down(Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests;
