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

use std::{
    ffi::c_void,
    ptr,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
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
    pacing::{self, DisplayClock, Mode},
    video_surface::{self, GlRenderer, VideoFrame},
};

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

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Track {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
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
}

/// A subtitle in a file of its own, added to the item once it is loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleFile {
    pub url: String,
    pub title: String,
    pub lang: String,
    /// Starts shown.
    pub select: bool,
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
    Load(PlayRequest),
    /// Ends the worker, so mpv is closed before a test process exits.
    #[cfg(test)]
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
    /// Sets an mpv property by name.
    SetProperty(String, String),
    /// Runs an mpv command with its arguments.
    Command(String, Vec<String>),
    /// In a SyncPlay group or not; see `pacing::video_sync`.
    SetGroup(bool),
    Stop,
}

struct Shared {
    status: Mutex<PlayerStatus>,
    frame: Mutex<Option<VideoFrame>>,
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
    /// When mpv asked for the newest frame (see `RenderSignal`).
    frame_asked_ns: AtomicU64,
    /// When the newest frame was rendered.
    frame_done_ns: AtomicU64,
    /// The time of the tick the newest frame was made after, in `clock_ns`;
    /// the UI shows it one tick later (`pacing::due`). 0 for a frame with
    /// audio timing.
    frame_tick: AtomicU64,
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
    /// The display link, once the worker has started it (display pacing).
    clock: Mutex<Option<DisplayClock>>,
    /// mpv's own view of the display sync, for `dev/jctl pacing clock`.
    sync_info: Mutex<String>,
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
struct RenderThread {
    handle: Option<thread::JoinHandle<()>>,
    signal: &'static RenderSignal,
}

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

/// `BLOOM_OLD_PACING=1` gives the timing of two changes ago (frames wait
/// for the timers of the worker and of the UI), to compare with
/// `dev/jctl pacing`. See `pacing::mode` for the other switches.
pub fn old_pacing() -> bool {
    pacing::mode() == Mode::Old
}

impl Shared {
    fn wake(&self) {
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_one();
    }

    /// Events nobody reads are dropped when the channel is full.
    fn emit(&self, event: PlayerEvent) {
        let _ = self.events.0.try_send(event);
    }
}

/// Shared handle owned by the UI. Cheap to clone.
#[derive(Clone)]
pub struct Player {
    shared: Arc<Shared>,
    commands: Arc<Mutex<Option<mpsc::Sender<Cmd>>>>,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                status: Mutex::new(PlayerStatus::default()),
                frame: Mutex::new(None),
                frame_seq: AtomicU64::new(0),
                target_w: AtomicU32::new(1280),
                target_h: AtomicU32::new(720),
                load_gen: AtomicU64::new(0),
                wake: (Mutex::new(false), Condvar::new()),
                events: async_channel::bounded(256),
                frames: async_channel::bounded(1),
                frame_asked_ns: AtomicU64::new(0),
                frame_done_ns: AtomicU64::new(0),
                frame_tick: AtomicU64::new(0),
                dwidth: AtomicU32::new(0),
                dheight: AtomicU32::new(0),
                ui_seen_ns: AtomicU64::new(0),
                display: AtomicU32::new(0),
                in_group: AtomicBool::new(false),
                clock: Mutex::new(None),
                sync_info: Mutex::new(String::new()),
            }),
            commands: Arc::new(Mutex::new(None)),
        }
    }
}

impl Player {
    pub fn status(&self) -> PlayerStatus {
        self.shared.status.lock().unwrap().clone()
    }

    /// Latest rendered frame and its sequence number.
    pub fn frame(&self) -> (u64, Option<VideoFrame>) {
        (
            self.shared.frame_seq.load(Ordering::Acquire),
            self.shared.frame.lock().unwrap().clone(),
        )
    }

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
        *self.shared.frame.lock().unwrap() = None;
        self.shared.frame_seq.fetch_add(1, Ordering::Release);
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

    /// The time of the tick the newest frame was made after (0 for a frame
    /// with audio timing), the time now, and the period of the display, all
    /// in nanoseconds of `clock_ns`. For `pacing::due`.
    pub fn frame_tick_time(&self) -> (u64, u64, u64) {
        let tick = self.shared.frame_tick.load(Ordering::Acquire);
        let now = clock_ns();
        let (last_tick, period) = self
            .shared
            .clock
            .lock()
            .unwrap()
            .as_ref()
            .map_or((0, 0), |clock| {
                let state = clock.state();
                (state.tick_ns.load(Ordering::Acquire), state.period_ns.load(Ordering::Relaxed))
            });
        // Where this draw lands after our tick, for `pacing clock`.
        if last_tick != 0 {
            pacing::note_phase(now.saturating_sub(last_tick));
        }
        (tick, now, if period == 0 { 16_666_667 } else { period })
    }

    /// The display drives the frames (see `pacing.rs`), and the clock runs.
    pub fn display_pacing(&self) -> bool {
        pacing::mode() == Mode::Display && self.shared.clock.lock().unwrap().is_some()
    }

    /// The UI draws the frames right now: call it from each draw. The
    /// worker renders no frame for a window that stopped drawing.
    pub fn ui_seen(&self) {
        self.shared.ui_seen_ns.store(clock_ns(), Ordering::Release);
    }

    /// The display the window is on; the display link follows it.
    pub fn set_display(&self, display: u32) {
        self.shared.display.store(display, Ordering::Relaxed);
    }

    /// In a SyncPlay group or not: chooses the video-sync mode of mpv.
    pub fn set_group(&self, in_group: bool) {
        self.shared.in_group.store(in_group, Ordering::Relaxed);
        self.send(Cmd::SetGroup(in_group));
    }

    /// The display link and mpv's display sync, for `dev/jctl pacing clock`.
    pub fn pacing_state(&self) -> String {
        let clock = self
            .shared
            .clock
            .lock()
            .unwrap()
            .as_ref()
            .map_or_else(|| "no display link".to_string(), DisplayClock::describe);
        format!(
            "{clock} | {} | mpv: {}",
            pacing::report(),
            self.shared.sync_info.lock().unwrap()
        )
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
    /// A factor on the speed the user chose, for small corrections of the
    /// position. It does not show as the speed of the user.
    pub fn set_sync_speed(&self, factor: f64) {
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
    pub fn set_property(&self, name: &str, value: &str) {
        self.send(Cmd::SetProperty(name.to_string(), value.to_string()));
    }
    pub fn command(&self, name: &str, args: &[&str]) {
        self.send(Cmd::Command(
            name.to_string(),
            args.iter().map(|a| a.to_string()).collect(),
        ));
    }
    pub fn stop(&self) {
        self.send(Cmd::Stop);
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
        *status = PlayerStatus {
            state: PlayState::Starting,
            title: title.to_string(),
            position: start_secs,
            paused,
            sub_delay,
            audio_delay,
            ..Default::default()
        };
        self.shared.load_gen.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Loads the item of a prepared load. False, and nothing happens,
    /// when a later load was prepared since.
    pub fn play_prepared(&self, generation: u64, request: PlayRequest) -> bool {
        if self.shared.load_gen.load(Ordering::Acquire) != generation {
            return false;
        }
        self.ensure_thread();
        self.send(Cmd::Load(request));
        true
    }

    /// A prepared load that got no address: the item ends with the error,
    /// unless a later load was prepared since.
    pub fn fail_load(&self, generation: u64, token: u64, message: &str) {
        if self.shared.load_gen.load(Ordering::Acquire) != generation {
            return;
        }
        let mut status = self.shared.status.lock().unwrap();
        status.error = Some(format!("Could not start playback: {message}"));
        status.state = PlayState::Ended;
        self.shared.emit(PlayerEvent::LoadFailed { token });
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = self.commands.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
            // The worker sleeps up to a few milliseconds between rounds.
            self.shared.wake();
        }
    }

    /// Ends the worker and waits for it. A process that exits while mpv
    /// still runs can crash, so a test with the real player ends with this.
    #[cfg(test)]
    pub(crate) fn shut_down(&self) {
        self.send(Cmd::Quit);
        let end = Instant::now() + Duration::from_secs(10);
        while self.commands.lock().unwrap().is_some() && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn ensure_thread(&self) {
        let mut slot = self.commands.lock().unwrap();
        if slot.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        *slot = Some(tx);
        let shared = self.shared.clone();
        let commands = self.commands.clone();
        thread::Builder::new()
            .name("mpv".into())
            .spawn(move || {
                if let Err(err) = run(shared.clone(), rx) {
                    log::error!("mpv worker failed: {err:#}");
                    let mut status = shared.status.lock().unwrap();
                    status.error = Some(format!("{err:#}"));
                    status.state = PlayState::Ended;
                }
                *commands.lock().unwrap() = None;
            })
            .expect("spawn mpv thread");
    }
}

// ----- worker thread ---------------------------------------------------------

struct Session {
    client: Client,
    item_id: String,
    play_session_id: String,
    play_method: String,
    media_source_id: String,
    last_report: Instant,
    /// Last position of this item; the shared status can already show the
    /// item that replaces it.
    position: f64,
}

/// A playback report for the server. The AirPlay sender sends its own
/// through a reporter of its own.
pub(crate) struct Report {
    pub(crate) client: Client,
    pub(crate) kind: ReportKind,
    pub(crate) progress: Progress,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportKind {
    Start,
    Progress,
    Stopped,
}

/// Sends the playback reports in the order they come. The requests block,
/// so they have a thread of their own; on the worker they would stop the
/// video and every deadline for as long as the server takes.
pub(crate) fn spawn_reporter() -> mpsc::Sender<Report> {
    let (tx, rx) = mpsc::channel::<Report>();
    let _ = thread::Builder::new().name("playback-reports".into()).spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut batch = vec![first];
            while let Ok(more) = rx.try_recv() {
                batch.push(more);
            }
            for (n, report) in batch.iter().enumerate() {
                // A progress report with a later report of its playback
                // behind it is out of date.
                let later = batch[n + 1..]
                    .iter()
                    .any(|r| r.progress.play_session_id == report.progress.play_session_id);
                if report.kind == ReportKind::Progress && later {
                    continue;
                }
                let (name, result) = match report.kind {
                    ReportKind::Start => ("start", report.client.report_start(&report.progress)),
                    ReportKind::Progress => {
                        ("progress", report.client.report_progress(&report.progress))
                    }
                    ReportKind::Stopped => {
                        ("stopped", report.client.report_stopped(&report.progress))
                    }
                };
                if let Err(err) = &result {
                    log::warn!("report {name} failed: {err:#}");
                }
                // A local play keeps its position on this Mac as well; an
                // unsent one goes at the next start with a connection.
                if report.kind != ReportKind::Start
                    && crate::downloads::local_path(&report.progress.item_id).is_some()
                {
                    crate::downloads::offline::note_report(&report.progress, result.is_err());
                }
                // A transcode runs on until the server is told to end it.
                if report.kind == ReportKind::Stopped && report.progress.play_method == "Transcode" {
                    let id = &report.progress.play_session_id;
                    match report.client.stop_encoding(id) {
                        Ok(()) => log::info!("ended the transcode of session {}", &id[..id.len().min(8)]),
                        Err(err) => log::warn!("stop encoding failed: {err:#}"),
                    }
                }
            }
        }
    });
    tx
}

/// How long the player may be out of data before it counts as a stall.
const STALL_AFTER: Duration = Duration::from_secs(3);
/// Time between two position measurements while following.
const SAMPLE_EVERY: Duration = Duration::from_millis(100);
/// Data the player must have ahead before it counts as able to play.
const READY_CACHE_SECS: f64 = 2.;
/// It counts as able to play after this long without that much data.
const READY_WAIT: Duration = Duration::from_secs(5);
/// An exact seek lands within a frame of its target; this is the check.
const SETTLED_WITHIN_SECS: f64 = 0.3;

/// The player gets able to play: after a load or after a seek.
struct Pending {
    token: u64,
    /// Target of a seek; none for a load.
    target: Option<f64>,
    /// When mpv said that playback can go on.
    restarted: Option<Instant>,
}

/// State of the worker that is not in the shared status.
struct Worker {
    reports: mpsc::Sender<Report>,
    /// A load was sent and mpv has not started the file: the events that
    /// come are of the file before.
    awaiting_start: bool,
    /// Subtitle files to add once the file of the load is loaded.
    subtitles: Vec<SubtitleFile>,
    /// The file is loaded: time to add them.
    add_subtitles: bool,
    pending: Option<Pending>,
    scheduled: Option<Scheduled>,
    follow: bool,
    last_sample: Instant,
    user_speed: f64,
    sync_speed: f64,
    /// Out of data since then.
    stall_since: Option<Instant>,
    stalled: bool,
    /// The volume and mute the app set, for the reports.
    volume: f64,
    muted: bool,
    /// A change the server must hear of soon (pause, seek, volume): the
    /// next progress report goes at this time, not at the usual interval.
    report_at: Option<Instant>,
    /// The item that plays or played last. A delay is for one item: it
    /// stays for a load of the same item and resets for another one.
    item_id: Option<String>,
    /// The delays of the item that loads again: mpv takes them back to 0
    /// with the new file, so they are set again once it is loaded.
    keep_delays: Option<(f64, f64)>,
    /// The size of the picture changed (see `RenderSignal::redraw`).
    size_changed: bool,
}

impl Worker {
    fn apply_speed(&self, mpv: &Mpv) {
        let speed = (self.user_speed * self.sync_speed).clamp(0.25, 4.);
        let _ = mpv.set_property("speed", speed);
    }

    /// Asks for a progress report in a moment: a device that controls this
    /// one sees the change through the server. The wait lets the position
    /// of a seek settle first.
    fn report_soon(&mut self) {
        if self.report_at.is_none() {
            self.report_at = Some(Instant::now() + Duration::from_millis(300));
        }
    }

    fn seek_exact(&mut self, mpv: &Mpv, position: f64, token: u64) {
        let _ = mpv.command("seek", &[&format!("{position:.3}"), "absolute+exact"]);
        self.pending = Some(Pending { token, target: Some(position), restarted: None });
    }
}

struct Renderer {
    ctx: *mut sys::mpv_render_context,
    gl: GlRenderer,
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // mpv frees its GL objects here, so the context in `gl` must outlive it.
        unsafe { sys::mpv_render_context_free(self.ctx) };
    }
}

unsafe extern "C" fn on_render_update(ctx: *mut c_void) {
    let signal = unsafe { &*(ctx as *const RenderSignal) };
    if !signal.needed.swap(true, Ordering::AcqRel) {
        signal.asked_ns.store(clock_ns(), Ordering::Release);
    }
    if !old_pacing() {
        signal.wake();
    }
}

fn run(shared: Arc<Shared>, rx: mpsc::Receiver<Cmd>) -> Result<()> {
    let mut mpv = Mpv::with_initializer(|init| {
        init.set_option("vo", "libmpv")?;
        init.set_option("hwdec", "auto-safe")?;
        init.set_option("keep-open", "no")?;
        init.set_option("idle", "yes")?;
        init.set_option("terminal", "no")?;
        init.set_option("ytdl", "no")?;
        init.set_option("config", "no")?;
        init.set_option("sub-auto", "no")?;
        init.set_option("audio-display", "no")?;
        init.set_option(
            "user-agent",
            format!("{}/{}", crate::config::APP_NAME, crate::config::APP_VERSION),
        )?;
        Ok(())
    })
    .map_err(|e| anyhow!("could not create libmpv core: {e}"))?;

    // Wake the loop from mpv's threads on events.
    {
        let shared = shared.clone();
        mpv.set_wakeup_callback(move || shared.wake());
    }
    // The frames are rendered on a thread of their own (see `render_loop`).
    // It ends before the core does: it is declared after `mpv`.
    let signal: &'static RenderSignal = Box::leak(Box::new(RenderSignal {
        needed: AtomicBool::new(false),
        shared: shared.clone(),
        asked_ns: AtomicU64::new(0),
        swap_due: AtomicBool::new(false),
        redraw: AtomicBool::new(false),
        quit: AtomicBool::new(false),
        wake: (Mutex::new(false), Condvar::new()),
    }));
    let _render = RenderThread::start(mpv.ctx.as_ptr() as usize, signal)?;
    let display_pacing = shared.clock.lock().unwrap().is_some();
    let _ = mpv.set_property("video-sync", pacing::video_sync(pacing::mode(), false));
    let mut in_group = false;
    let mut display_fps = 0.;
    let mut sync_checked = Instant::now();

    mpv.observe_property("time-pos", Format::Double, 1)?;
    mpv.observe_property("duration", Format::Double, 2)?;
    mpv.observe_property("pause", Format::Flag, 3)?;
    mpv.observe_property("track-list", Format::String, 4)?;
    mpv.observe_property("paused-for-cache", Format::Flag, 5)?;
    mpv.observe_property("path", Format::String, 6)?;
    mpv.observe_property("video-params/w", Format::Int64, 7)?;
    mpv.observe_property("video-params/h", Format::Int64, 8)?;
    mpv.observe_property("sub-delay", Format::Double, 9)?;
    mpv.observe_property("audio-delay", Format::Double, 10)?;
    // The size of the picture, for the render thread, which must not ask.
    mpv.observe_property("dwidth", Format::Int64, 11)?;
    mpv.observe_property("dheight", Format::Int64, 12)?;

    let mut session: Option<Session> = None;
    // End-of-file events still to come for files that a new load replaced.
    let mut stale_ends = 0u32;
    let mut worker = Worker {
        reports: spawn_reporter(),
        awaiting_start: false,
        subtitles: Vec::new(),
        add_subtitles: false,
        pending: None,
        scheduled: None,
        follow: false,
        last_sample: Instant::now(),
        user_speed: 1.,
        sync_speed: 1.,
        stall_since: None,
        stalled: false,
        volume: 100.,
        muted: false,
        report_at: None,
        item_id: None,
        keep_delays: None,
        size_changed: false,
    };

    loop {
        // Once a second: the display the window is on, its refresh rate for
        // mpv, and mpv's view of the sync for `dev/jctl pacing clock`.
        if display_pacing && sync_checked.elapsed() >= Duration::from_secs(1) {
            sync_checked = Instant::now();
            let fps = {
                let mut clock = shared.clock.lock().unwrap();
                let clock = clock.as_mut().expect("the clock runs");
                clock.set_display(shared.display.load(Ordering::Relaxed));
                clock.fps()
            };
            if let Some(fps) = fps
                && (fps - display_fps).abs() > 1e-3
            {
                display_fps = fps;
                log::info!("display refresh rate {fps:.3} Hz for mpv's display sync");
                let _ = mpv.set_property("display-fps-override", fps);
            }
            // Property reads wait for the core: none while a load, a seek
            // or a scheduled action is on its way, so they stay on time.
            if session.is_some() && worker.pending.is_none() && worker.scheduled.is_none() {
                let started = Instant::now();
                let read = |name: &str| {
                    mpv.get_property::<String>(name).unwrap_or_else(|_| "-".into())
                };
                *shared.sync_info.lock().unwrap() = format!(
                    "video-sync={} active={} display-fps={} estimated={} vsync-ratio={} jitter={} mistimed={} delayed={} dropped={}",
                    read("video-sync"),
                    read("display-sync-active"),
                    read("display-fps"),
                    read("estimated-display-fps"),
                    read("vsync-ratio"),
                    read("vsync-jitter"),
                    read("mistimed-frame-count"),
                    read("vo-delayed-frame-count"),
                    read("frame-drop-count"),
                );
                if started.elapsed() > Duration::from_millis(5) {
                    log::debug!("sync info took {:?}", started.elapsed());
                }
            }
        }

        // Events from mpv.
        while let Some(event) = mpv.wait_event(0.0) {
            match event {
                // The end of a file that a new load replaced.
                Ok(Event::EndFile(_)) if stale_ends > 0 => stale_ends -= 1,
                Ok(event) => {
                    if handle_event(&event, &shared, &mut session, &mut worker) {
                        finish(&shared, &mut session, &mut worker);
                    }
                }
                // The wrapper reports events that carry an mpv error (such as a
                // failed end-file) as `Err`; treat those as a failed load.
                Err(err) => {
                    log::warn!("mpv event error: {err}");
                    if session.is_some() {
                        let message = match err {
                            libmpv2::Error::Raw(code) => mpv_error_text(code),
                            other => other.to_string(),
                        };
                        shared.status.lock().unwrap().error =
                            Some(format!("Playback failed: {message}"));
                        finish(&shared, &mut session, &mut worker);
                    }
                }
            }
        }

        // The size of the picture is known: a frame skipped for want of it
        // is drawn once more (the event can come after mpv's first frame).
        if std::mem::take(&mut worker.size_changed)
            && shared.dwidth.load(Ordering::Relaxed) > 0
            && shared.dheight.load(Ordering::Relaxed) > 0
        {
            signal.redraw.store(true, Ordering::Release);
            signal.wake();
        }

        // Subtitles in files of their own, once the item is loaded. The
        // one that starts shown is added first, so its id in mpv is the
        // one the app expects.
        if std::mem::take(&mut worker.add_subtitles) {
            if let Some((sub, audio)) = worker.keep_delays.take() {
                let _ = mpv.set_property("sub-delay", sub);
                let _ = mpv.set_property("audio-delay", audio);
            }
            for sub in std::mem::take(&mut worker.subtitles) {
                let flag = if sub.select { "select" } else { "auto" };
                if let Err(err) =
                    mpv.command("sub-add", &[sub.url.as_str(), flag, &sub.title, &sub.lang])
                {
                    log::warn!("sub-add failed: {err}");
                }
            }
        }

        // Commands from the UI.
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Load(req) => {
                    let delays = {
                        let status = shared.status.lock().unwrap();
                        worker
                            .keep_delays
                            .unwrap_or((status.sub_delay, status.audio_delay))
                    };
                    // The item before goes away without the `Ended` state:
                    // the UI must not close the player between two items.
                    if let Some(active) = session.take() {
                        let _ = mpv.command("stop", &[]);
                        stale_ends += 1;
                        let _ = worker.reports.send(Report {
                            client: active.client.clone(),
                            kind: ReportKind::Stopped,
                            progress: Progress {
                                item_id: active.item_id.clone(),
                                play_session_id: active.play_session_id.clone(),
                                position_ticks: (active.position * TICKS_PER_SECOND as f64) as i64,
                                paused: false,
                                volume: worker.volume as i64,
                                muted: worker.muted,
                                play_method: active.play_method.clone(),
                                media_source_id: active.media_source_id.clone(),
                            },
                        });
                    }
                    let header = req.client.mpv_auth_header();
                    mpv.set_property("http-header-fields", header)?;
                    mpv.set_property("force-media-title", req.title.clone())?;
                    // The pause of the item before must not carry over, and a
                    // request can ask for a paused start.
                    mpv.set_property("pause", req.paused)?;
                    // A transcode has the one audio track the server chose,
                    // and no subtitle in it: a track choice made for the
                    // file itself must not carry over.
                    if req.play_method == "Transcode" {
                        let _ = mpv.set_property("aid", "auto");
                        let _ = mpv.set_property("sid", "auto");
                    }
                    worker.keep_delays = (worker.item_id.as_deref() == Some(req.item_id.as_str())
                        && delays != (0., 0.))
                    .then_some(delays);
                    if worker.keep_delays.is_none() {
                        let _ = mpv.set_property("sub-delay", 0.0);
                        let _ = mpv.set_property("audio-delay", 0.0);
                    }
                    worker.item_id = Some(req.item_id.clone());
                    let group = shared.in_group.load(Ordering::Relaxed);
                    if in_group != group {
                        in_group = group;
                        let _ = mpv.set_property("video-sync", pacing::video_sync(pacing::mode(), group));
                    }
                    worker.subtitles = req.subtitles.clone();
                    worker.scheduled = None;
                    worker.sync_speed = 1.;
                    worker.apply_speed(&mpv);
                    worker.stall_since = None;
                    worker.stalled = false;
                    worker.awaiting_start = true;
                    worker.pending =
                        Some(Pending { token: req.token, target: None, restarted: None });
                    let start = if req.start_secs > 0. {
                        format!("start=+{:.3}", req.start_secs)
                    } else {
                        String::new()
                    };
                    let mut args: Vec<&str> = vec![req.url.as_str(), "replace", "-1"];
                    if !start.is_empty() {
                        args.push(&start);
                    }
                    if let Err(err) = mpv.command("loadfile", &args) {
                        let mut status = shared.status.lock().unwrap();
                        status.error = Some(format!("loadfile failed: {err}"));
                        status.state = PlayState::Ended;
                        worker.awaiting_start = false;
                        worker.pending = None;
                        shared.emit(PlayerEvent::LoadFailed { token: req.token });
                        continue;
                    }
                    let progress = Progress {
                        item_id: req.item_id.clone(),
                        play_session_id: req
                            .play_session_id
                            .clone()
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        position_ticks: (req.start_secs * TICKS_PER_SECOND as f64) as i64,
                        paused: req.paused,
                        volume: worker.volume as i64,
                        muted: worker.muted,
                        play_method: req.play_method.clone(),
                        media_source_id: req.media_source_id.clone(),
                    };
                    let play_session_id = progress.play_session_id.clone();
                    let _ = worker.reports.send(Report {
                        client: req.client.clone(),
                        kind: ReportKind::Start,
                        progress,
                    });
                    session = Some(Session {
                        client: req.client,
                        item_id: req.item_id,
                        play_session_id,
                        play_method: req.play_method,
                        media_source_id: req.media_source_id,
                        last_report: Instant::now(),
                        position: req.start_secs,
                    });
                }
                Cmd::TogglePause => {
                    let _ = mpv.command("cycle", &["pause"]);
                }
                Cmd::SetPaused(paused) => {
                    let _ = mpv.set_property("pause", paused);
                }
                Cmd::SeekExact { position, pause, token } => {
                    worker.report_soon();
                    if pause {
                        let _ = mpv.set_property("pause", true);
                    }
                    worker.seek_exact(&mpv, position, token);
                }
                Cmd::Schedule(scheduled) => worker.scheduled = scheduled,
                Cmd::SetSyncSpeed(factor) => {
                    worker.sync_speed = factor;
                    worker.apply_speed(&mpv);
                }
                Cmd::Follow(on) => worker.follow = on,
                Cmd::SeekRelative(secs) => {
                    let _ = mpv.command("seek", &[&secs.to_string(), "relative"]);
                    worker.report_soon();
                }
                Cmd::SeekAbsolute(secs) => {
                    let _ = mpv.command("seek", &[&secs.to_string(), "absolute"]);
                    worker.report_soon();
                }
                Cmd::SetAudio(id) => {
                    let value = id.map(|i| i.to_string()).unwrap_or_else(|| "no".into());
                    let _ = mpv.set_property("aid", value);
                }
                Cmd::SetSubtitle(id) => {
                    let value = id.map(|i| i.to_string()).unwrap_or_else(|| "no".into());
                    let _ = mpv.set_property("sid", value);
                }
                Cmd::SetVolume(volume) => {
                    let _ = mpv.set_property("volume", volume.clamp(0., 100.));
                    worker.volume = volume.clamp(0., 100.);
                    worker.report_soon();
                }
                Cmd::SetMuted(muted) => {
                    let _ = mpv.set_property("mute", muted);
                    worker.muted = muted;
                    worker.report_soon();
                }
                Cmd::SetSpeed(speed) => {
                    worker.user_speed = speed;
                    worker.apply_speed(&mpv);
                }
                Cmd::SetProperty(name, value) => {
                    if let Err(err) = mpv.set_property(name.as_str(), value) {
                        log::debug!("mpv property {name}: {err}");
                    }
                }
                Cmd::Command(name, args) => {
                    let args: Vec<&str> = args.iter().map(String::as_str).collect();
                    if let Err(err) = mpv.command(name.as_str(), &args) {
                        log::debug!("mpv command {name}: {err}");
                    }
                }
                Cmd::SetGroup(group) => {
                    if in_group != group {
                        in_group = group;
                        let _ = mpv.set_property("video-sync", pacing::video_sync(pacing::mode(), group));
                    }
                }
                Cmd::Stop => {
                    worker.item_id = None;
                    let _ = mpv.command("stop", &[]);
                }
                #[cfg(test)]
                Cmd::Quit => return Ok(()),
            }
        }

        // The scheduled action, at its time.
        if let Some(scheduled) = worker.scheduled
            && Instant::now() >= scheduled.at
        {
            worker.scheduled = None;
            // Measured before the action: the action itself takes time (the
            // audio device starts), and that is not lateness of this loop.
            let late = scheduled.at.elapsed();
            match scheduled.action {
                ScheduledAction::Unpause => {
                    let _ = mpv.set_property("pause", false);
                }
                ScheduledAction::PauseThenSeek(position) => {
                    let _ = mpv.set_property("pause", true);
                    let _ = mpv.command("seek", &[&format!("{position:.3}"), "absolute+exact"]);
                }
            }
            shared.emit(PlayerEvent::ScheduledFired { late });
        }

        // The player is able to play after a load or a seek: mpv restarted
        // playback, the position is the one asked for, and there is data
        // ahead, so a start does not stall at once.
        if let Some(pending) = &worker.pending
            && let Some(restarted) = pending.restarted
            && !mpv.get_property::<bool>("seeking").unwrap_or(false)
        {
            let position = mpv.get_property::<f64>("time-pos").unwrap_or(f64::MAX);
            let there = pending
                .target
                .is_none_or(|target| (position - target).abs() <= SETTLED_WITHIN_SECS);
            let has_data = mpv
                .get_property::<f64>("demuxer-cache-duration")
                .is_ok_and(|secs| secs >= READY_CACHE_SECS)
                || mpv.get_property::<bool>("demuxer-cache-idle").unwrap_or(false)
                || restarted.elapsed() >= READY_WAIT;
            if there && has_data {
                let token = pending.token;
                let event = match pending.target {
                    Some(_) => PlayerEvent::Settled { token },
                    None => PlayerEvent::Loaded { token },
                };
                worker.pending = None;
                {
                    let mut status = shared.status.lock().unwrap();
                    status.position = position;
                    status.position_at = Some(Instant::now());
                }
                shared.emit(event);
            }
        }

        // A stall that lasts.
        if !worker.stalled && worker.stall_since.is_some_and(|at| at.elapsed() >= STALL_AFTER) {
            worker.stalled = true;
            shared.emit(PlayerEvent::Stalled);
        }

        // The position by the audio that plays now. `time-pos` moves once a
        // video frame; the audio position moves all the time and takes the
        // delay of the audio output into account.
        if worker.follow && session.is_some() && worker.last_sample.elapsed() >= SAMPLE_EVERY {
            worker.last_sample = Instant::now();
            // `audio-pts` is the position of the sound, which `audio-delay`
            // moves (measured: +500 ms of delay gives 500 ms less). The
            // group needs the position of the picture, so the delay is added.
            let position = mpv
                .get_property::<f64>("audio-pts")
                .map(|pts| pts + mpv.get_property::<f64>("audio-delay").unwrap_or(0.))
                .or_else(|_| mpv.get_property::<f64>("time-pos"));
            let at = Instant::now();
            if let Ok(position) = position {
                let status = shared.status.lock().unwrap();
                let (paused, duration) = (status.paused, status.duration);
                drop(status);
                shared.emit(PlayerEvent::Position(Sample { position, at, paused, duration }));
            }
        }

        // Periodic progress reporting, and a report soon after a change
        // another device may watch (see `Worker::report_soon`).
        let soon = worker.report_at.is_some_and(|at| Instant::now() >= at);
        if let Some(active) = session.as_mut()
            && (soon || active.last_report.elapsed() >= Duration::from_secs(10))
        {
            active.last_report = Instant::now();
            worker.report_at = None;
            let snapshot = shared.status.lock().unwrap().clone();
            if snapshot.state == PlayState::Playing {
                let _ = worker.reports.send(Report {
                    client: active.client.clone(),
                    kind: ReportKind::Progress,
                    progress: progress_of(active, &snapshot, &worker),
                });
            }
        } else if session.is_none() {
            worker.report_at = None;
        }

        // Sleep until something happens (bounded so timers keep running),
        // and not past the scheduled action.
        let mut timeout = Duration::from_millis(4);
        if let Some(scheduled) = worker.scheduled {
            timeout = timeout.min(scheduled.at.saturating_duration_since(Instant::now()));
        }
        let (lock, cvar) = &shared.wake;
        let mut flag = lock.lock().unwrap();
        if !*flag {
            let (guard, _) = cvar.wait_timeout(flag, timeout).unwrap();
            flag = guard;
        }
        *flag = false;
    }
}

// ----- render thread ---------------------------------------------------------

/// Renders the frames mpv asks for, and reports each refresh of the display
/// to mpv (see `pacing.rs`). It calls only `mpv_render_*` functions, never
/// the core: the sizes it needs come through `Shared`. The display link
/// and mpv's render context live and die here.
fn render_loop(mpv_handle: usize, signal: &'static RenderSignal, ready: mpsc::Sender<Result<()>>) {
    let shared = &signal.shared;
    let mut renderer = match create_renderer(mpv_handle as *mut sys::mpv_handle, signal) {
        Ok(renderer) => renderer,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    // The clock of the display: at each tick the swap is reported to mpv,
    // which then hands over the frame for the next refresh.
    if pacing::mode() == Mode::Display {
        let on_tick = Box::new(move || {
            signal.swap_due.store(true, Ordering::Release);
            signal.wake();
        });
        match DisplayClock::start(on_tick) {
            Ok(clock) => *shared.clock.lock().unwrap() = Some(clock),
            // Without the clock the frames keep the audio timing.
            Err(err) => log::warn!("no display link, audio timing for the frames: {err:#}"),
        }
    }
    let display_pacing = shared.clock.lock().unwrap().is_some();
    let _ = ready.send(Ok(()));

    while !signal.quit.load(Ordering::Acquire) {
        // The display refreshed: mpv hears of it first, so it can hand
        // over the frame for the next refresh at once.
        if signal.swap_due.swap(false, Ordering::AcqRel) {
            unsafe { sys::mpv_render_context_report_swap(renderer.ctx) };
        }

        // Video frame. A frame in display sync is rendered at once and the
        // call does not wait; one with audio timing waits in the render
        // call for its time.
        let redraw = signal.redraw.swap(false, Ordering::AcqRel);
        if signal.needed.swap(false, Ordering::AcqRel) || redraw {
            shared.frame_asked_ns.store(signal.asked_ns.load(Ordering::Acquire), Ordering::Release);
            let flags = unsafe { sys::mpv_render_context_update(renderer.ctx) };
            if flags & (sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64) == 0 {
                // No new frame: with the size just known, the frame mpv has
                // is drawn once more.
                if redraw && let Err(err) = render_frame(&mut renderer, shared, false, false) {
                    log::debug!("redraw skipped: {err}");
                }
            } else {
                let info = next_frame_info(&renderer);
                let synced =
                    info.flags & sys::mpv_render_frame_info_flag_MPV_RENDER_FRAME_INFO_BLOCK_VSYNC as u64 != 0;
                let repeat = info.flags & sys::mpv_render_frame_info_flag_MPV_RENDER_FRAME_INFO_REPEAT as u64 != 0
                    && info.flags & sys::mpv_render_frame_info_flag_MPV_RENDER_FRAME_INFO_REDRAW as u64 == 0;
                // The same picture again, or nobody draws: mpv counts the
                // frame as rendered and keeps its timing, the surface stays.
                let watching = pacing::ui_watching(shared.ui_seen_ns.load(Ordering::Acquire), clock_ns());
                let skip = display_pacing && (repeat || !watching);
                if skip {
                    if repeat {
                        pacing::count_repeat();
                    } else {
                        pacing::count_unseen();
                    }
                }
                if let Err(err) = render_frame(&mut renderer, shared, skip, synced) {
                    log::debug!("render skipped: {err}");
                }
            }
        }

        // Sleep until mpv asks or the display ticks (bounded: with the old
        // timing nothing wakes this thread).
        let (lock, cvar) = &signal.wake;
        let mut flag = lock.lock().unwrap();
        if !*flag && !signal.needed.load(Ordering::Acquire) {
            let (guard, _) = cvar.wait_timeout(flag, Duration::from_millis(4)).unwrap();
            flag = guard;
        }
        *flag = false;
    }
    // The clock stops first; mpv's render context goes before the core.
    *shared.clock.lock().unwrap() = None;
    drop(renderer);
}

fn create_renderer(mpv_handle: *mut sys::mpv_handle, signal: &'static RenderSignal) -> Result<Renderer> {
    // The GL context becomes current on this thread; mpv renders with it.
    let gl = GlRenderer::new()?;
    let mut init = sys::mpv_opengl_init_params {
        get_proc_address: Some(video_surface::get_proc_address),
        get_proc_address_ctx: ptr::null_mut(),
    };
    let mut params = [
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
            data: sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
        },
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
            data: &mut init as *mut sys::mpv_opengl_init_params as *mut c_void,
        },
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    let mut ctx: *mut sys::mpv_render_context = ptr::null_mut();
    let code = unsafe { sys::mpv_render_context_create(&mut ctx, mpv_handle, params.as_mut_ptr()) };
    if code < 0 || ctx.is_null() {
        return Err(anyhow!("mpv_render_context_create failed ({code})"));
    }
    unsafe {
        sys::mpv_render_context_set_update_callback(
            ctx,
            Some(on_render_update),
            signal as *const RenderSignal as *mut c_void,
        );
    }
    Ok(Renderer { ctx, gl })
}

/// What mpv says about the frame it asks to render.
fn next_frame_info(renderer: &Renderer) -> sys::mpv_render_frame_info {
    let mut info = sys::mpv_render_frame_info { flags: 0, target_time: 0 };
    let param = sys::mpv_render_param {
        type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_NEXT_FRAME_INFO,
        data: &mut info as *mut sys::mpv_render_frame_info as *mut c_void,
    };
    unsafe { sys::mpv_render_context_get_info(renderer.ctx, param) };
    info
}

/// Renders the current frame at the largest size that fits the UI's video area
/// without exceeding the source resolution, then publishes its GPU surface.
/// With `skip` mpv takes the frame as rendered and nothing is drawn.
/// `synced` says the frame is in display sync: made for the next refresh.
fn render_frame(renderer: &mut Renderer, shared: &Shared, skip: bool, synced: bool) -> Result<()> {
    // The time of the tick a frame in display sync is made after. A frame
    // with audio timing waits in the render call for its time instead and
    // gets no tick.
    let tick = if synced {
        shared
            .clock
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |clock| clock.state().tick_ns.load(Ordering::Acquire))
    } else {
        0
    };
    if skip {
        let mut one: i32 = 1;
        let mut params = [
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_SKIP_RENDERING,
                data: &mut one as *mut i32 as *mut c_void,
            },
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let code = unsafe { sys::mpv_render_context_render(renderer.ctx, params.as_mut_ptr()) };
        if code < 0 {
            return Err(anyhow!("mpv_render_context_render (skip) failed ({code})"));
        }
        return Ok(());
    }
    let src_w = shared.dwidth.load(Ordering::Relaxed);
    let src_h = shared.dheight.load(Ordering::Relaxed);
    if src_w == 0 || src_h == 0 {
        // The size is on its way from the worker (its event can come after
        // mpv's first frame): this frame is not drawn, and the worker asks
        // for it again when the size is there (`RenderSignal::redraw`).
        pacing::count_no_size();
        return render_frame(renderer, shared, true, synced);
    }
    let target_w = shared.target_w.load(Ordering::Relaxed).max(16);
    let target_h = shared.target_h.load(Ordering::Relaxed).max(16);
    let scale = (target_w as f64 / src_w as f64)
        .min(target_h as f64 / src_h as f64)
        .min(1.0);
    let w = (((src_w as f64 * scale) as u32).max(2) / 2) * 2;
    let h = (((src_h as f64 * scale) as u32).max(2) / 2) * 2;

    let mut fbo = sys::mpv_opengl_fbo {
        fbo: renderer.gl.begin(w, h)?,
        w: w as i32,
        h: h as i32,
        internal_format: 0,
    };
    let mut params = [
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
            data: &mut fbo as *mut sys::mpv_opengl_fbo as *mut c_void,
        },
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    let code = unsafe { sys::mpv_render_context_render(renderer.ctx, params.as_mut_ptr()) };
    if code < 0 {
        return Err(anyhow!("mpv_render_context_render failed ({code})"));
    }
    let frame = renderer.gl.finish()?;
    *shared.frame.lock().unwrap() = Some(frame);
    shared.frame_tick.store(tick, Ordering::Release);
    pacing::count_rendered(synced);
    shared.frame_seq.fetch_add(1, Ordering::Release);
    // From mpv's request to the finished frame. mpv asks early and the
    // render call waits for the time of the frame, so this is about the
    // time of one frame. The UI measures its own wait from here.
    let asked = shared.frame_asked_ns.load(Ordering::Acquire);
    let now = clock_ns();
    crate::perf::video_frame_rendered(now.saturating_sub(asked) as f64 / 1e6);
    shared.frame_done_ns.store(now, Ordering::Release);
    if !old_pacing() {
        let _ = shared.frames.0.try_send(());
    }
    Ok(())
}

/// Applies one mpv event to the shared status. Returns true when playback ended.
fn handle_event(
    event: &Event,
    shared: &Shared,
    session: &mut Option<Session>,
    worker: &mut Worker,
) -> bool {
    let mut status = shared.status.lock().unwrap();
    match event {
        Event::StartFile => {
            worker.awaiting_start = false;
            false
        }
        // Of the file before the one that loads.
        Event::FileLoaded | Event::PlaybackRestart if worker.awaiting_start => false,
        Event::PropertyChange { name: "time-pos" | "duration" | "paused-for-cache", .. }
            if worker.awaiting_start =>
        {
            false
        }
        Event::PropertyChange { name, change, .. } => {
            match (*name, change) {
                ("time-pos", PropertyData::Double(v)) => {
                    status.position = *v;
                    status.position_at = Some(Instant::now());
                    if let Some(active) = session.as_mut() {
                        active.position = *v;
                    }
                }
                ("duration", PropertyData::Double(v)) => status.duration = *v,
                ("video-params/w", PropertyData::Int64(v)) => status.video_w = (*v).max(0) as u32,
                ("video-params/h", PropertyData::Int64(v)) => status.video_h = (*v).max(0) as u32,
                ("dwidth", PropertyData::Int64(v)) => {
                    shared.dwidth.store((*v).max(0) as u32, Ordering::Relaxed);
                    worker.size_changed = true;
                }
                ("dheight", PropertyData::Int64(v)) => {
                    shared.dheight.store((*v).max(0) as u32, Ordering::Relaxed);
                    worker.size_changed = true;
                }
                ("sub-delay", PropertyData::Double(v)) => status.sub_delay = *v,
                ("audio-delay", PropertyData::Double(v)) => status.audio_delay = *v,
                ("pause", PropertyData::Flag(v)) => {
                    if status.paused != *v {
                        worker.report_soon();
                    }
                    status.paused = *v;
                }
                ("paused-for-cache", PropertyData::Flag(v)) => {
                    status.buffering = *v;
                    if *v {
                        // Out of data during a load or a seek is no stall.
                        if worker.pending.is_none() && worker.stall_since.is_none() {
                            worker.stall_since = Some(Instant::now());
                        }
                    } else {
                        worker.stall_since = None;
                        if std::mem::take(&mut worker.stalled) {
                            shared.emit(PlayerEvent::Recovered);
                        }
                    }
                }
                ("path", PropertyData::Str(path)) => status.path = Some(path.to_string()),
                ("track-list", PropertyData::Str(json)) => {
                    let tracks: Vec<Track> = serde_json::from_str(json).unwrap_or_default();
                    let tracks: Vec<Track> =
                        tracks.into_iter().filter(|t| t.kind != "video").collect();
                    if tracks != status.tracks {
                        status.tracks = tracks;
                        status.tracks_version += 1;
                    }
                }
                _ => {}
            }
            false
        }
        Event::FileLoaded | Event::PlaybackRestart => {
            if session.is_some() {
                status.state = PlayState::Playing;
            }
            if matches!(event, Event::FileLoaded) {
                worker.add_subtitles = true;
            }
            if matches!(event, Event::PlaybackRestart)
                && let Some(pending) = &mut worker.pending
            {
                pending.restarted = Some(Instant::now());
            }
            false
        }
        Event::EndFile(reason) => {
            if *reason == mpv_end_file_reason::Error {
                status.error = Some("mpv could not play this stream".to_string());
            }
            status.reached_end = *reason == mpv_end_file_reason::Eof;
            // Only the active session's end is meaningful (stop+load emits one too).
            session.is_some()
        }
        Event::Shutdown => true,
        _ => false,
    }
}

fn mpv_error_text(code: i32) -> String {
    unsafe {
        let ptr = sys::mpv_error_string(code);
        if ptr.is_null() {
            format!("mpv error {code}")
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

fn progress_of(session: &Session, status: &PlayerStatus, worker: &Worker) -> Progress {
    Progress {
        item_id: session.item_id.clone(),
        play_session_id: session.play_session_id.clone(),
        position_ticks: (status.position * TICKS_PER_SECOND as f64) as i64,
        paused: status.paused,
        volume: worker.volume as i64,
        muted: worker.muted,
        play_method: session.play_method.clone(),
        media_source_id: session.media_source_id.clone(),
    }
}

/// Reports the stop to Jellyfin and marks the session ended.
fn finish(shared: &Shared, session: &mut Option<Session>, worker: &mut Worker) {
    let Some(active) = session.take() else { return };
    let snapshot = shared.status.lock().unwrap().clone();
    let _ = worker.reports.send(Report {
        client: active.client.clone(),
        kind: ReportKind::Stopped,
        progress: progress_of(&active, &snapshot, worker),
    });
    worker.scheduled = None;
    worker.item_id = None;
    worker.stall_since = None;
    worker.stalled = false;
    // A load that ended before the item could play has failed.
    if let Some(pending) = worker.pending.take()
        && pending.target.is_none()
    {
        shared.emit(PlayerEvent::LoadFailed { token: pending.token });
    } else if snapshot.reached_end && snapshot.error.is_none() {
        shared.emit(PlayerEvent::Ended);
    }
    let mut status = shared.status.lock().unwrap();
    status.state = PlayState::Ended;
}

/// One test with the real player at a time: two of them at once decode two
/// videos, and the times they measure in milliseconds go wrong.
#[cfg(test)]
pub(crate) static REAL_PLAYER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Closes the player of a test when the test ends, also when it fails.
#[cfg(test)]
pub(crate) struct Closer(pub Player);

#[cfg(test)]
impl Drop for Closer {
    fn drop(&mut self) {
        self.0.shut_down();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_track_list_json() {
        let json = r#"[{"id":1,"type":"video","selected":true},
            {"id":1,"type":"audio","lang":"eng","codec":"aac","selected":true,"default":true},
            {"id":2,"type":"sub","title":"English SDH","lang":"eng","codec":"subrip","selected":false}]"#;
        let tracks: Vec<Track> = serde_json::from_str(json).unwrap();
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[1].label(), "ENG · AAC");
        assert_eq!(tracks[2].label(), "English SDH · ENG · SUBRIP");
    }

    /// Plays a synthetic clip through the embedded core, checks frames arrive,
    /// pauses, seeks, and stops. Skipped when libmpv or ffmpeg are unavailable.
    #[test]
    fn embedded_playback_roundtrip() {
        let _one = REAL_PLAYER.lock().unwrap_or_else(|e| e.into_inner());
        let Some(media) = test_clip("bloom-embedded-test.mp4") else {
            return;
        };
        let client = Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user");
        let player = Player::default();
        let _closer = Closer(player.clone());
        player.set_target_size(640, 480);
        player.play(PlayRequest {
            client,
            item_id: "test".into(),
            url: media.display().to_string(),
            title: "Embedded test".into(),
            start_secs: 0.,
            paused: false,
            token: 0,
            play_session_id: None,
            play_method: String::new(),
            media_source_id: String::new(),
            subtitles: Vec::new(),
        });
        roundtrip(&player);
    }

    /// A 30 s clip with a tone; `None` when ffmpeg is not there.
    fn test_clip(name: &str) -> Option<std::path::PathBuf> {
        let media = std::env::temp_dir().join(name);
        let encoded = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                // Red top half, blue bottom half: checks orientation and chroma.
                "color=c=red:s=320x240:r=10:d=30,drawbox=x=0:y=120:w=320:h=120:color=blue:t=fill",
            ])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=30"])
            .args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&media)
            .status();
        if !encoded.map(|s| s.success()).unwrap_or(false) {
            eprintln!("ffmpeg not available; skipping");
            return None;
        }
        Some(media)
    }

    fn roundtrip(player: &Player) {
        let wait = |what: &str, mut ok: Box<dyn FnMut() -> bool>| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !ok() {
                let status = player.status();
                assert!(status.error.is_none(), "{what}: {status:?}");
                assert!(Instant::now() < deadline, "{what} timed out: {status:?}");
                thread::sleep(Duration::from_millis(50));
            }
        };
        let p = player.clone();
        wait(
            "playing",
            Box::new(move || p.status().state == PlayState::Playing),
        );
        let p = player.clone();
        wait("first frame", Box::new(move || p.frame().1.is_some()));
        let (_, frame) = player.frame();
        let frame = frame.unwrap();
        assert_eq!(frame.width(), 320);
        assert_eq!(frame.height(), 240);
        // Let the GPU finish, then read pixels (bytes are B, G, R, A).
        thread::sleep(Duration::from_millis(300));
        let buffer = player.frame().1.unwrap().buffer();
        buffer.lock_base_address(core_video::pixel_buffer::kCVPixelBufferLock_ReadOnly);
        let stride = buffer.get_bytes_per_row();
        let pixel = |x: usize, y: usize| unsafe {
            let base = buffer.get_base_address() as *const u8;
            let at = base.add(y * stride + x * 4);
            (*at.add(2), *at.add(1), *at)
        };
        let (top, bottom) = (pixel(160, 40), pixel(160, 200));
        buffer.unlock_base_address(core_video::pixel_buffer::kCVPixelBufferLock_ReadOnly);
        assert!(top.0 > 200 && top.2 < 60, "top must be red, got RGB {top:?}");
        assert!(bottom.2 > 200 && bottom.0 < 60, "bottom must be blue, got RGB {bottom:?}");
        let p = player.clone();
        wait(
            "tracks",
            Box::new(move || p.status().tracks.iter().any(|t| t.kind == "audio")),
        );
        player.toggle_pause();
        let p = player.clone();
        wait("paused", Box::new(move || p.status().paused));
        player.seek_absolute(20.0);
        let p = player.clone();
        wait("seeked", Box::new(move || p.status().position >= 19.0));
        player.stop();
        let p = player.clone();
        wait(
            "ended",
            Box::new(move || p.status().state == PlayState::Ended),
        );
        player.acknowledge_end();
        assert_eq!(player.status().state, PlayState::Idle);
    }

    /// The next event that fits, within ten seconds.
    fn expect(player: &Player, what: &str, mut fits: impl FnMut(&PlayerEvent) -> bool) -> PlayerEvent {
        let events = player.events();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match events.try_recv() {
                Ok(event) if fits(&event) => return event,
                Ok(_) => {}
                Err(_) => {
                    let status = player.status();
                    assert!(status.error.is_none(), "{what}: {status:?}");
                    assert!(Instant::now() < deadline, "{what} timed out: {status:?}");
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }

    /// Position measurements over a time, and the speed they show.
    fn measured_speed(player: &Player, over: Duration) -> (usize, f64) {
        let events = player.events();
        while events.try_recv().is_ok() {}
        thread::sleep(over);
        let samples: Vec<Sample> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                PlayerEvent::Position(sample) => Some(sample),
                _ => None,
            })
            .collect();
        let (first, last) = (samples.first().unwrap(), samples.last().unwrap());
        let speed = (last.position - first.position) / (last.at - first.at).as_secs_f64();
        (samples.len(), speed)
    }

    /// How far the position is from where a steady clock would put it, after
    /// each change of the speed. Prints the numbers; run with --nocapture.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn speed_change_position_error() {
        let Some(media) = test_clip("bloom-embedded-speed-test.mp4") else {
            return;
        };
        let client = Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user");
        let player = Player::default();
        let _closer = Closer(player.clone());
        player.set_target_size(320, 240);
        player.play(PlayRequest {
            client,
            item_id: "test".into(),
            url: media.display().to_string(),
            title: "Speed test".into(),
            start_secs: 2.,
            paused: false,
            token: 1,
            play_session_id: None,
            play_method: String::new(),
            media_source_id: String::new(),
            subtitles: Vec::new(),
        });
        player.set_muted(true);
        expect(&player, "loaded", |e| *e == PlayerEvent::Loaded { token: 1 });
        player.follow(true);
        thread::sleep(Duration::from_millis(1500));
        let events = player.events();
        let latest = |events: &async_channel::Receiver<PlayerEvent>| {
            let mut last = None;
            while let Ok(event) = events.try_recv() {
                if let PlayerEvent::Position(sample) = event {
                    last = Some(sample);
                }
            }
            last
        };
        thread::sleep(Duration::from_millis(300));
        let start = latest(&events).unwrap();
        // Position a steady clock would give, from the speeds set so far.
        let mut expected = start.position;
        let mut since = start.at;
        let mut speed = 1.0;
        for (factor, hold_ms) in [(0.97, 2000), (1.0, 1500), (1.03, 2000), (1.0, 1500), (0.95, 1500), (1.0, 1500)] {
            let now = Instant::now();
            expected += (now - since).as_secs_f64() * speed;
            since = now;
            speed = factor;
            player.set_sync_speed(factor);
            thread::sleep(Duration::from_millis(hold_ms));
            let sample = latest(&events).unwrap();
            let at_sample = expected + (sample.at - since).as_secs_f64() * speed;
            let status = player.status();
            let video = status.position
                - (expected + (status.position_at.unwrap() - since).as_secs_f64() * speed);
            eprintln!(
                "speed {factor:.2} for {hold_ms} ms: audio position off by {:+.1} ms, video by {:+.1} ms",
                (sample.position - at_sample) * 1000.,
                video * 1000.
            );
        }
        player.stop();
    }

    /// What group playback needs of the player: a paused load at a position,
    /// a start at a set time, exact seeks that report when they are done,
    /// position measurements, a speed correction, and the end of the item.
    #[test]
    fn precise_control_for_group_playback() {
        let _one = REAL_PLAYER.lock().unwrap_or_else(|e| e.into_inner());
        let Some(media) = test_clip("bloom-embedded-sync-test.mp4") else {
            return;
        };
        let client = Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user");
        let player = Player::default();
        let _closer = Closer(player.clone());
        player.set_target_size(320, 240);
        let request = |start_secs: f64, paused: bool, token: u64| PlayRequest {
            client: client.clone(),
            item_id: "test".into(),
            url: media.display().to_string(),
            title: "Sync test".into(),
            start_secs,
            paused,
            token,
            play_session_id: None,
            play_method: String::new(),
            media_source_id: String::new(),
            subtitles: Vec::new(),
        };

        // A paused load at a position with a fraction.
        player.play(request(5.5, true, 7));
        player.set_muted(true);
        expect(&player, "loaded", |e| *e == PlayerEvent::Loaded { token: 7 });
        let status = player.status();
        assert!(status.paused, "{status:?}");
        assert!((status.position - 5.5).abs() < 0.1, "{status:?}");
        thread::sleep(Duration::from_millis(300));
        assert!((player.status().position - 5.5).abs() < 0.1, "moved while paused");

        // A start at a set time.
        player.schedule(Some(Scheduled {
            at: Instant::now() + Duration::from_millis(400),
            action: ScheduledAction::Unpause,
        }));
        thread::sleep(Duration::from_millis(250));
        assert!(player.status().paused, "started before its time");
        let fired = expect(&player, "scheduled start", |e| {
            matches!(e, PlayerEvent::ScheduledFired { .. })
        });
        let PlayerEvent::ScheduledFired { late } = fired else { unreachable!() };
        eprintln!("scheduled start ran {late:?} after its time");
        assert!(late < Duration::from_millis(20), "start was {late:?} late");
        let p = player.clone();
        let deadline = Instant::now() + Duration::from_secs(5);
        while p.status().paused || p.status().position < 5.6 {
            assert!(Instant::now() < deadline, "did not start: {:?}", p.status());
            thread::sleep(Duration::from_millis(10));
        }

        // Position measurements, and a correction of the speed.
        player.follow(true);
        thread::sleep(Duration::from_millis(500));
        let (count, speed) = measured_speed(&player, Duration::from_millis(1500));
        assert!(count >= 10, "{count} measurements");
        eprintln!("{count} measurements in 1.5 s, speed {speed:.4}");
        assert!((speed - 1.0).abs() < 0.03, "speed {speed}");
        player.set_sync_speed(1.08);
        thread::sleep(Duration::from_millis(500));
        let (_, speed) = measured_speed(&player, Duration::from_millis(1500));
        eprintln!("speed with a factor of 1.08: {speed:.4}");
        assert!((speed - 1.08).abs() < 0.03, "corrected speed {speed}");
        player.set_sync_speed(1.0);
        player.follow(false);

        // An exact seek that parks the player and says when it is done.
        player.seek_exact(20.25, true, 9);
        expect(&player, "settled", |e| *e == PlayerEvent::Settled { token: 9 });
        let status = player.status();
        assert!(status.paused, "{status:?}");
        assert!((status.position - 20.25).abs() < 0.1, "{status:?}");

        // A pause at a set time that goes to a position.
        player.set_paused(false);
        player.set_paused(false);
        thread::sleep(Duration::from_millis(300));
        assert!(!player.status().paused);
        player.schedule(Some(Scheduled {
            at: Instant::now() + Duration::from_millis(200),
            action: ScheduledAction::PauseThenSeek(12.0),
        }));
        expect(&player, "scheduled pause", |e| matches!(e, PlayerEvent::ScheduledFired { .. }));
        let p = player.clone();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(p.status().paused && (p.status().position - 12.0).abs() < 0.1) {
            assert!(Instant::now() < deadline, "did not park: {:?}", p.status());
            thread::sleep(Duration::from_millis(10));
        }

        // A new item starts to play though the one before was paused.
        player.play(request(0., false, 11));
        player.set_muted(true);
        expect(&player, "second load", |e| *e == PlayerEvent::Loaded { token: 11 });
        thread::sleep(Duration::from_millis(400));
        let status = player.status();
        assert!(!status.paused && status.position > 0.1, "{status:?}");

        // The end of the item.
        player.seek_exact(29.0, false, 12);
        expect(&player, "ended", |e| *e == PlayerEvent::Ended);
        player.acknowledge_end();
    }
}
