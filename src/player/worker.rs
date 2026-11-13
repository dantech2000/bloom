//! The worker thread of the player: it runs mpv's commands, reads its
//! events, and sends the reports of playback to the server.

use super::*;

// ----- worker thread ---------------------------------------------------------

pub(super) struct Session {
    pub(super) client: Client,
    pub(super) item_id: String,
    pub(super) play_session_id: String,
    pub(super) play_method: String,
    /// The server makes the stream (`PlayRequest::transcoding`).
    pub(super) transcoding: bool,
    pub(super) media_source_id: String,
    pub(super) last_report: Instant,
    /// Last position of this item; the shared status can already show the
    /// item that replaces it.
    pub(super) position: f64,
}

/// A playback report for the server. The AirPlay sender sends its own
/// through a reporter of its own.
pub(crate) struct Report {
    pub(crate) client: Client,
    pub(crate) kind: ReportKind,
    pub(crate) progress: Progress,
    /// The server makes the stream of the play session (a transcode, or a
    /// remux, which the reports name "DirectStream"): the stop ends it.
    pub(crate) transcoding: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportKind {
    Start,
    Progress,
    /// The stop. It ends the stream of a report with `transcoding` too.
    Stopped,
}

/// Sends the playback reports in the order they come. The requests block,
/// so they have a thread of their own; on the worker they would stop the
/// video and every deadline for as long as the server takes.
pub(crate) fn spawn_reporter() -> mpsc::Sender<Report> {
    reporter().0
}

/// The reporter with its thread, which ends once every sender is gone and
/// the reports are sent: the worker waits for it at the quit of the app.
pub(super) fn reporter() -> (mpsc::Sender<Report>, Option<thread::JoinHandle<()>>) {
    let (tx, rx) = mpsc::channel::<Report>();
    let handle = thread::Builder::new().name("playback-reports".into()).spawn(move || {
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
                // A local play keeps its position on this Mac as well, under
                // the server and user of the report; an unsent one goes at
                // the next start of that profile with a connection.
                if matches!(report.kind, ReportKind::Progress | ReportKind::Stopped)
                    && let Some(who) = crate::downloads::offline::Identity::of(&report.client)
                    && crate::downloads::local_path(report.client.server_id.as_deref(), &report.progress.item_id).is_some()
                {
                    crate::downloads::offline::note_report(&who, &report.progress, result.is_err());
                }
                // A stream the server makes runs on until the server is
                // told to end it.
                if report.kind == ReportKind::Stopped && report.transcoding {
                    let id = &report.progress.play_session_id;
                    match report.client.stop_encoding(id) {
                        Ok(()) => log::info!("ended the transcode of session {}", &id[..id.len().min(8)]),
                        Err(err) => log::warn!("stop encoding failed: {err:#}"),
                    }
                }
            }
        }
    });
    (tx, handle.ok())
}


/// How long the player may be out of data before it counts as a stall.
pub(super) const STALL_AFTER: Duration = Duration::from_secs(3);
/// Time between two position measurements while following.
pub(super) const SAMPLE_EVERY: Duration = Duration::from_millis(100);
/// Data the player must have ahead before it counts as able to play.
pub(super) const READY_CACHE_SECS: f64 = 2.;
/// It counts as able to play after this long without that much data.
pub(super) const READY_WAIT: Duration = Duration::from_secs(5);
/// An exact seek lands within a frame of its target; this is the check.
pub(super) const SETTLED_WITHIN_SECS: f64 = 0.3;

/// The player gets able to play: after a load or after a seek.
pub(super) struct Pending {
    pub(super) token: u64,
    /// Target of a seek; none for a load.
    pub(super) target: Option<f64>,
    /// When mpv said that playback can go on.
    pub(super) restarted: Option<Instant>,
    /// The load an exact seek took the place of, still to be answered.
    pub(super) load: Option<u64>,
}

impl Pending {
    /// The token of the load that has no answer yet.
    pub(super) fn load_token(&self) -> Option<u64> {
        if self.target.is_none() { Some(self.token) } else { self.load }
    }
}

/// While the player gets able to play after mpv restarted playback, the
/// worker looks at it this often.
pub(super) const PENDING_POLL: Duration = Duration::from_millis(10);
/// The longest sleep of the worker with an item (see the end of `run`).
pub(super) const ACTIVE_WAIT: Duration = Duration::from_millis(100);
/// Time between two progress reports.
pub(super) const REPORT_EVERY: Duration = Duration::from_secs(10);

/// State of the worker that is not in the shared status.
pub(super) struct Worker {
    pub(super) reports: mpsc::Sender<Report>,
    /// A load was sent and mpv has not started the file: the events that
    /// come are of the file before.
    pub(super) awaiting_start: bool,
    /// Subtitle files to add once the file of the load is loaded.
    pub(super) subtitles: Vec<SubtitleFile>,
    /// The file is loaded: time to add them.
    pub(super) add_subtitles: bool,
    pub(super) pending: Option<Pending>,
    pub(super) scheduled: Option<Scheduled>,
    pub(super) follow: bool,
    pub(super) last_sample: Instant,
    pub(super) user_speed: f64,
    pub(super) sync_speed: f64,
    /// Out of data since then.
    pub(super) stall_since: Option<Instant>,
    pub(super) stalled: bool,
    /// The volume and mute the app set, for the reports.
    pub(super) volume: f64,
    pub(super) muted: bool,
    /// A change the server must hear of soon (pause, seek, volume): the
    /// next progress report goes at this time, not at the usual interval.
    pub(super) report_at: Option<Instant>,
    /// The item that plays or played last. A delay is for one item: it
    /// stays for a load of the same item and resets for another one.
    pub(super) item_id: Option<String>,
    /// The delays of the item that loads again: mpv takes them back to 0
    /// with the new file, so they are set again once it is loaded.
    pub(super) keep_delays: Option<(f64, f64)>,
    /// The size of the picture changed (see `RenderSignal::redraw`).
    pub(super) size_changed: bool,
    /// The way to this worker, for the thread that fetches the subtitles.
    pub(super) tx: mpsc::Sender<Cmd>,
    /// The load count of the item that plays (`Cmd::Load`).
    pub(super) generation: u64,
    /// The subtitle files added to the item that plays.
    pub(super) sub_files: Vec<PathBuf>,
    /// Copies of the frame for the glass are wanted; the time of the last.
    pub(super) backdrop: Option<Instant>,
}

impl Worker {
    pub(super) fn new(reports: mpsc::Sender<Report>, tx: mpsc::Sender<Cmd>) -> Self {
        Self {
            reports,
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
            tx,
            generation: 0,
            sub_files: Vec::new(),
            backdrop: None,
        }
    }

    /// The status is of the item of this worker, not of a load that was
    /// prepared since or of a player at rest (`PlayerStatus::generation`).
    fn owns(&self, status: &PlayerStatus) -> bool {
        status.generation == self.generation
    }

    /// Removes the subtitle files of the item: with a new item or at the
    /// end they have no use.
    fn forget_subtitles(&mut self) {
        for path in self.sub_files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

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
        self.await_seek(position, token);
    }

    /// An exact seek is on its way. A load that still waits for its answer
    /// keeps it: both are answered once the player can play at the target.
    pub(super) fn await_seek(&mut self, position: f64, token: u64) {
        let load = self.pending.take().and_then(|pending| pending.load_token());
        self.pending = Some(Pending { token, target: Some(position), restarted: None, load });
    }
}


/// The properties the worker hears the changes of. `dwidth` and `dheight`
/// are the size of the picture, for the render thread, which must not ask.
pub(super) const OBSERVED: [(&str, Format); 12] = [
    ("time-pos", Format::Double),
    ("duration", Format::Double),
    ("pause", Format::Flag),
    ("track-list", Format::String),
    ("paused-for-cache", Format::Flag),
    ("path", Format::String),
    ("video-params/w", Format::Int64),
    ("video-params/h", Format::Int64),
    ("sub-delay", Format::Double),
    ("audio-delay", Format::Double),
    ("dwidth", Format::Int64),
    ("dheight", Format::Int64),
];

/// What an error in place of an event of mpv says, when it ends the item:
/// the wrapper gives the error of a file that ended with one as `Raw`.
/// Any other error is of the event itself (a log line or the text of a
/// property that is not UTF-8) and says nothing about the playback.
pub(super) fn end_error(err: &libmpv2::Error) -> Option<String> {
    match err {
        libmpv2::Error::Raw(code) => Some(mpv_error_text(*code)),
        _ => None,
    }
}

/// Makes the next worker fail before it has a core, for a test.
#[cfg(test)]
pub(crate) static FAIL_INIT: AtomicBool = AtomicBool::new(false);

pub(super) fn run(shared: Arc<Shared>, rx: &mpsc::Receiver<Cmd>, tx: mpsc::Sender<Cmd>, reports: mpsc::Sender<Report>) -> Result<()> {
    #[cfg(test)]
    if FAIL_INIT.load(Ordering::Relaxed) {
        return Err(anyhow!("test: the worker fails to start"));
    }
    let mut mpv = Mpv::with_initializer(|init| {
        // The Mac renders through the render API into surfaces the UI
        // composites (`render_loop`). iOS: mpv's own output, Metal through
        // MoltenVK, into the `CAMetalLayer` the window keeps under GPUI's
        // (`gpui_ios::video_layer`); the page leaves a hole for it.
        #[cfg(target_os = "macos")]
        init.set_option("vo", "libmpv")?;
        #[cfg(target_os = "ios")]
        {
            let layer = gpui_ios::video_layer();
            if layer.is_null() {
                return Err(libmpv2::Error::Raw(-1));
            }
            init.set_option("vo", "gpu-next")?;
            init.set_option("gpu-api", "vulkan")?;
            init.set_option("gpu-context", "moltenvk")?;
            init.set_option("wid", layer as i64)?;
            // The backdrop copy (`backdrop_copy`) takes the decoded frame
            // on the CPU. The GPU path (`pl_tex_download` through MoltenVK)
            // aborted the simulator's Metal driver on 2026-10-22, and it
            // would block the vo thread on a real GPU as well.
            init.set_option("screenshot-sw", "yes")?;
        }
        init.set_option("hwdec", "auto-safe")?;
        // A file with interlaced frames (a DVD rip, a TV recording) shows
        // comb lines unless its fields are put together; `auto` does that
        // for the frames a file marks as interlaced and leaves every other
        // file alone.
        init.set_option("deinterlace", "auto")?;
        init.set_option("keep-open", "no")?;
        init.set_option("idle", "yes")?;
        init.set_option("terminal", "no")?;
        // An mpv without Lua (MPVKit's build for iOS) has no `ytdl` option.
        let _ = init.set_option("ytdl", "no");
        init.set_option("config", "no")?;
        init.set_option("sub-auto", "no")?;
        init.set_option("audio-display", "no")?;
        // The chroma planes are upscaled with bilinear filtering, not with
        // the lanczos of `scale`. Chroma is a quarter of the picture and
        // the difference is not seen; the two lanczos passes over a full
        // 4K frame were. Measured 2026-10-20 on Dune (3840x2160 HEVC Dolby
        // Vision, direct play): mpv's render took 15 ms of GPU a frame and
        // 130 of 962 frames were shown a refresh late; with bilinear, 9 ms
        // and none late.
        init.set_option("cscale", "bilinear")?;
        // In a test the sound goes to mpv's null output, which keeps the
        // time of a real one. The CoreAudio output listens for changes of
        // the audio devices of the Mac, and that listener of libmpv
        // crashed the test process when a device came or went while a
        // core of a test closed (two runs on 2026-10-07, in
        // `HALSystem::ObjectsPublishedAndDied`). That was mpv-player/mpv#18274
        // (a listener left behind by an output whose setup failed), and the
        // upstream fix is in `vendor/mpv/patches` since 2026-10-20; it hit
        // Daniel's own app on 2026-10-20 as well. The tests are muted in any
        // case. `BLOOM_TEST_REAL_AUDIO=1` gives a test the real output.
        #[cfg(test)]
        if std::env::var_os("BLOOM_TEST_REAL_AUDIO").is_none() {
            init.set_option("ao", "null")?;
        }
        init.set_option(
            "user-agent",
            format!("{}/{}", crate::config::APP_NAME, crate::config::APP_VERSION),
        )?;
        // libmpv checks no server certificate by itself (see `tls_roots_file`).
        init.set_option("tls-verify", "yes")?;
        init.set_option("tls-ca-file", tls_roots_file())?;
        // Built-in scripts the player has no use for, each a Lua thread.
        // `stats` stays: the playback info shows it (`toggle_playback_info`).
        // An older mpv may lack one.
        for script in [
            "osc",
            "load-osd-console",
            "load-auto-profiles",
            "load-select",
            "load-positioning",
            "load-commands",
            "load-context-menu",
        ] {
            let _ = init.set_option(script, "no");
        }
        // Options of a test (`BLOOM_MPV_OPTS`), after the app's own.
        for (name, value) in &crate::options::options().mpv_opts {
            if let Err(err) = init.set_option(name.as_str(), value.as_str()) {
                log::warn!("BLOOM_MPV_OPTS {name}={value}: {err}");
            }
        }
        Ok(())
    })
    .map_err(|e| anyhow!("could not create libmpv core: {e}"))?;

    // Wake the loop from mpv's threads on events.
    {
        let shared = shared.clone();
        mpv.set_wakeup_callback(move || shared.wake());
    }
    // mpv's own warnings and errors come as events and go to the app's log
    // (`handle_event`): an audio output that fails to open, a stream that
    // ends in an error. Without them the app saw none of it (on macOS 27
    // the CoreAudio output failed for every file and mpv fell back to
    // AVFoundation in silence; found on 2026-10-20).
    let code = unsafe { sys::mpv_request_log_messages(mpv.ctx.as_ptr(), c"warn".as_ptr()) };
    if code < 0 {
        log::debug!("mpv_request_log_messages failed ({code})");
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
    #[cfg(target_os = "macos")]
    let _render = RenderThread::start(mpv.ctx.as_ptr() as usize, signal)?;
    let display_pacing = shared.clock.lock().unwrap().is_some();
    let _ = mpv.set_property("video-sync", pacing::video_sync(false));
    let mut in_group = false;
    let mut display_fps = 0.;
    let mut sync_checked = Instant::now();

    for (id, (name, format)) in OBSERVED.iter().enumerate() {
        mpv.observe_property(name, *format, id as u64 + 1)?;
    }

    let mut session: Option<Session> = None;
    // End-of-file events still to come for files that a new load replaced.
    let mut stale_ends = 0u32;
    let mut worker = Worker::new(reports, tx);

    loop {
        // Once a second: the display the window is on, and its refresh rate
        // for mpv.
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
        }

        // Events from mpv. The wrapper gives `None` for an empty queue, and
        // also for a property that is not there any more, with events
        // behind it. Each observed property has one change in the queue at
        // most, so the queue is empty after more of them in a row than
        // there are properties. Nothing may stay in it: the loop sleeps
        // until mpv has a new event.
        let mut empty = 0;
        while empty <= OBSERVED.len() {
            let Some(event) = mpv.wait_event(0.0) else {
                empty += 1;
                continue;
            };
            empty = 0;
            match event {
                Ok(Event::LogMessage { prefix, level, text, .. }) => {
                    let text = text.trim_end();
                    if level == "error" || level == "fatal" {
                        log::warn!("mpv [{prefix}] {text}");
                    } else {
                        log::info!("mpv [{prefix}] {text}");
                    }
                }
                // The end of a file that a new load replaced.
                Ok(Event::EndFile(_)) if stale_ends > 0 => stale_ends -= 1,
                Ok(event) => {
                    if handle_event(&event, &shared, &mut session, &mut worker) {
                        finish(&shared, &mut session, &mut worker);
                    }
                }
                // The end of a file that a new load replaced, when that
                // file ended with an error: it comes as `Err` (below), and
                // it is not the end of the item that loads now.
                Err(libmpv2::Error::Raw(code)) if stale_ends > 0 => {
                    stale_ends -= 1;
                    log::debug!("mpv: the replaced file ended with an error ({code})");
                }
                // The wrapper reports events that carry an mpv error (such as a
                // failed end-file) as `Err`; treat those as a failed load.
                Err(err) => {
                    log::warn!("mpv event error: {err}");
                    if session.is_some()
                        && let Some(message) = end_error(&err)
                    {
                        let mut status = shared.status.lock().unwrap();
                        if worker.owns(&status) {
                            status.error = Some(format!("Playback failed: {message}"));
                        }
                        drop(status);
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

        // Subtitles in files of their own, once the item is loaded. They
        // are fetched on a thread of their own and come back as
        // `Cmd::AddSubtitle`, in their order, so the one that starts shown
        // is added first and its id in mpv is the one the app expects.
        if std::mem::take(&mut worker.add_subtitles) {
            if let Some((sub, audio)) = worker.keep_delays.take() {
                let _ = mpv.set_property("sub-delay", sub);
                let _ = mpv.set_property("audio-delay", audio);
            }
            let subtitles = std::mem::take(&mut worker.subtitles);
            if !subtitles.is_empty()
                && let Some(active) = session.as_ref()
            {
                fetch_subtitles(
                    shared.clone(),
                    worker.tx.clone(),
                    active.client.clone(),
                    worker.generation,
                    subtitles,
                );
            }
        }

        // A copy of the frame for the glass over the video, while a popup
        // is open over the player (iOS: the video is under GPUI's layer).
        if let Some(last) = worker.backdrop
            && last.elapsed() >= BACKDROP_INTERVAL
            && session.is_some()
        {
            worker.backdrop = Some(Instant::now());
            if let Some(image) = backdrop_copy(&mpv) {
                crate::ui::glass::set_video_backdrop(Some(Arc::new(image)));
            }
        }

        // Commands from the UI.
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Load { generation, request: req, tracks } => {
                    // A stop or a later load came while this one was on its
                    // way: it must not start playback behind the stop. The
                    // status is the newer load's, or the stop set it.
                    if shared.load_gen.load(Ordering::Acquire) != generation {
                        shared.emit(PlayerEvent::LoadFailed { token: req.token });
                        Leftover::of(&req).end();
                        continue;
                    }
                    // The display drives the frames again (held while idle).
                    if let Some(clock) = shared.clock.lock().unwrap().as_mut() {
                        clock.set_running(true);
                    }
                    // The draw phase starts from the model again for a new
                    // item: a phase that a slow spell of the compositor
                    // pushed to its floor must not stay for the session.
                    pacing::phase::reset();
                    // The track choices made for this load before a worker
                    // was there; mpv keeps them over the `loadfile` below.
                    if let Some(id) = tracks.audio {
                        set_track(&mpv, "aid", id);
                    }
                    if let Some(id) = tracks.subtitle {
                        set_track(&mpv, "sid", id);
                    }
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
                        report_stop(&active, false, &worker);
                    }
                    // A failure here ends the worker; the load is answered
                    // first, as a group waits for that answer.
                    let token = req.token;
                    let answered = |err| {
                        shared.emit(PlayerEvent::LoadFailed { token });
                        err
                    };
                    let header = req.client.mpv_auth_header();
                    mpv.set_property("http-header-fields", header).map_err(answered)?;
                    mpv.set_property("force-media-title", req.title.clone()).map_err(answered)?;
                    // The pause of the item before must not carry over, and a
                    // request can ask for a paused start.
                    mpv.set_property("pause", req.paused).map_err(answered)?;
                    // A transcode has the one audio track the server chose,
                    // and no subtitle in it: a track choice made for the
                    // file itself must not carry over.
                    if req.transcoding {
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
                        let _ = mpv.set_property("video-sync", pacing::video_sync(group));
                    }
                    worker.subtitles = req.subtitles.clone();
                    worker.generation = generation;
                    worker.forget_subtitles();
                    worker.scheduled = None;
                    worker.sync_speed = 1.;
                    shared.sync_speed.store(1f64.to_bits(), Ordering::Relaxed);
                    worker.apply_speed(&mpv);
                    worker.stall_since = None;
                    worker.stalled = false;
                    worker.awaiting_start = true;
                    worker.pending =
                        Some(Pending { token: req.token, target: None, restarted: None, load: None });
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
                        transcoding: req.transcoding,
                    });
                    session = Some(Session {
                        client: req.client,
                        item_id: req.item_id,
                        play_session_id,
                        play_method: req.play_method,
                        transcoding: req.transcoding,
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
                Cmd::SetAudio(id) => set_track(&mpv, "aid", id),
                Cmd::SetSubtitle(id) => set_track(&mpv, "sid", id),
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
                Cmd::Backdrop(on) => {
                    worker.backdrop = on.then(|| Instant::now() - BACKDROP_INTERVAL);
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
                        let _ = mpv.set_property("video-sync", pacing::video_sync(group));
                    }
                }
                Cmd::Stop(generation) => {
                    worker.item_id = None;
                    let _ = mpv.command("stop", &[]);
                    // A load that was only prepared ends here, unless a
                    // later one was prepared since the stop: no end of a
                    // file comes for it. (The end of an item that plays
                    // is for the status of that item alone, see `finish`.)
                    let mut status = shared.status.lock().unwrap();
                    if status.state == PlayState::Starting
                        && shared.load_gen.load(Ordering::Acquire) == generation
                    {
                        status.state = PlayState::Ended;
                    }
                    drop(status);
                    if session.is_none() {
                        rest(&shared);
                    }
                }
                Cmd::AddSubtitle { generation, index, path, select, title, lang } => {
                    set_pending(&shared, generation, index, false);
                    // Of an item that is over, or of a load a later one replaced.
                    if generation != worker.generation || session.is_none() {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    let flag = if select { "select" } else { "auto" };
                    let file = path.to_string_lossy().into_owned();
                    match mpv.command("sub-add", &[file.as_str(), flag, &title, &lang]) {
                        Err(err) => log::warn!("sub-add failed: {err}"),
                        Ok(()) => {
                            // mpv appends the track, so its id is the highest
                            // subtitle id now; the menus find the file by it.
                            let sid = mpv
                                .get_property::<String>("track-list")
                                .ok()
                                .and_then(|json| serde_json::from_str::<Vec<Track>>(&json).ok())
                                .and_then(|tracks| tracks.iter().filter(|t| t.kind == TrackKind::Sub).map(|t| t.id).max());
                            let mut status = shared.status.lock().unwrap();
                            if let Some(sid) = sid
                                && worker.owns(&status)
                            {
                                status.external_sids.retain(|(i, _)| *i != index);
                                status.external_sids.push((index, sid));
                                status.tracks_version += 1;
                            }
                        }
                    }
                    worker.sub_files.push(path);
                }
                Cmd::FetchSubtitle(file) => {
                    if let Some(active) = session.as_ref() {
                        fetch_subtitles(
                            shared.clone(),
                            worker.tx.clone(),
                            active.client.clone(),
                            worker.generation,
                            vec![SubtitleFile { select: true, ..file }],
                        );
                    }
                }
                Cmd::Quit => {
                    // The item that plays ends with the app: the server
                    // hears of the stop (the thread of this worker waits
                    // for the report, see `ensure_thread`).
                    finish(&shared, &mut session, &mut worker);
                    worker.forget_subtitles();
                    return Ok(());
                }
                Cmd::Probe(name, reply) => {
                    let _ = reply.send(mpv.get_property::<String>(&name).unwrap_or_default());
                }
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
            // No position is no place to write down, and not the target.
            let position = mpv.get_property::<f64>("time-pos").ok();
            let there = pending.target.is_none_or(|target| {
                position.is_some_and(|position| (position - target).abs() <= SETTLED_WITHIN_SECS)
            });
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
                let load = pending.load;
                worker.pending = None;
                {
                    let mut status = shared.status.lock().unwrap();
                    if let Some(position) = position
                        && worker.owns(&status)
                    {
                        status.position = position;
                        status.position_at = Some(Instant::now());
                    }
                }
                // The load a seek took the place of is answered first.
                if let Some(token) = load {
                    shared.emit(PlayerEvent::Loaded { token });
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
            // While paused the sound does not run, and after a seek
            // `audio-pts` still says where the sound stopped: `time-pos`
            // is the place then (measured: the `Ready` after a seek while
            // paused carried the position of the pause before it).
            let status = shared.status.lock().unwrap();
            let (paused, duration) = (status.paused, status.duration);
            drop(status);
            let position = if paused {
                mpv.get_property::<f64>("time-pos")
            } else {
                mpv.get_property::<f64>("audio-pts")
                    .map(|pts| pts + mpv.get_property::<f64>("audio-delay").unwrap_or(0.))
                    .or_else(|_| mpv.get_property::<f64>("time-pos"))
            };
            let at = Instant::now();
            if let Ok(position) = position {
                shared.emit(PlayerEvent::Position(Sample { position, at, paused, duration }));
            }
        }

        // Periodic progress reporting, and a report soon after a change
        // another device may watch (see `Worker::report_soon`).
        let soon = worker.report_at.is_some_and(|at| Instant::now() >= at);
        if let Some(active) = session.as_mut()
            && (soon || active.last_report.elapsed() >= REPORT_EVERY)
        {
            active.last_report = Instant::now();
            worker.report_at = None;
            let (state, paused) = {
                let status = shared.status.lock().unwrap();
                (status.state, status.paused)
            };
            if state == PlayState::Playing {
                let _ = worker.reports.send(Report {
                    client: active.client.clone(),
                    kind: ReportKind::Progress,
                    progress: progress_of(active, paused, &worker),
                    transcoding: active.transcoding,
                });
            }
        } else if session.is_none() {
            worker.report_at = None;
        }

        // Sleep until something happens: a command or an event of mpv wakes
        // the loop. The wait ends at the next thing the loop does by the
        // clock: the scheduled action, a position measurement, a look at a
        // player that gets able to play, a copy of the frame. The timers
        // that count in seconds (the reports, the stall) are looked at
        // every `ACTIVE_WAIT` with an item; an idle core has none and
        // sleeps. (Before 2026-11-10 the loop ran every 4 ms with an item,
        // 250 rounds a second for timers known ahead.)
        let idle = session.is_none() && worker.pending.is_none() && worker.scheduled.is_none();
        let now = Instant::now();
        let mut timeout = if idle { IDLE_WAIT } else { ACTIVE_WAIT };
        let mut wake_at = |at: Instant| timeout = timeout.min(at.saturating_duration_since(now));
        if let Some(scheduled) = worker.scheduled {
            wake_at(scheduled.at);
        }
        if session.is_some() {
            if worker.follow {
                wake_at(worker.last_sample + SAMPLE_EVERY);
            }
            if let Some(last) = worker.backdrop {
                wake_at(last + BACKDROP_INTERVAL);
            }
        }
        if worker.pending.as_ref().is_some_and(|pending| pending.restarted.is_some()) {
            wake_at(now + PENDING_POLL);
        }
        let (lock, cvar) = &shared.wake;
        let mut flag = lock.lock().unwrap();
        if !*flag {
            let (guard, _) = cvar.wait_timeout(flag, timeout).unwrap();
            flag = guard;
        }
        *flag = false;
        crate::perf::count_loop(crate::perf::Loop::Worker);
    }
}

/// How long an idle thread of the player sleeps between its rounds; a
/// command, an event or a frame wakes it before.
pub(super) const IDLE_WAIT: Duration = Duration::from_secs(1);

/// How often the frame is copied for the glass over the video.
pub(super) const BACKDROP_INTERVAL: Duration = Duration::from_millis(100);

/// A copy of the video frame at a quarter of its size, as the image GPUI
/// paints (BGRA): `screenshot-raw video` with `screenshot-sw`, which
/// converts the decoded frame on the CPU (about 10 ms for 1080p; the GPU
/// path crashed the simulator), then a 4x4 box downscale.
/// The glass blurs it, so a quarter is plenty (the Mac samples its
/// quarter-size copy of the frame too).
pub(super) fn backdrop_copy(mpv: &Mpv) -> Option<gpui_kit::RenderImage> {
    let (w, h, stride, data) = screenshot_raw(mpv)?;
    let (ow, oh) = ((w / 4).max(1), (h / 4).max(1));
    let mut out = Vec::with_capacity((ow * oh * 4) as usize);
    for y in 0..oh {
        for x in 0..ow {
            let (mut b, mut g, mut r) = (0u32, 0u32, 0u32);
            for dy in 0..4 {
                let row = ((y * 4 + dy).min(h - 1) as usize) * stride;
                for dx in 0..4 {
                    let i = row + ((x * 4 + dx).min(w - 1) as usize) * 4;
                    b += u32::from(data[i]);
                    g += u32::from(data[i + 1]);
                    r += u32::from(data[i + 2]);
                }
            }
            out.extend_from_slice(&[(b / 16) as u8, (g / 16) as u8, (r / 16) as u8, 255]);
        }
    }
    let pixels = image::RgbaImage::from_raw(ow, oh, out)?;
    Some(gpui_kit::RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(pixels), 1)))
}

/// `screenshot-raw video` through the node API: the frame as `bgr0`
/// (width, height, stride in bytes, the bytes). None when mpv has no frame.
pub(super) fn screenshot_raw(mpv: &Mpv) -> Option<(u32, u32, usize, Vec<u8>)> {
    use std::ffi::{CStr, CString};
    let (cmd, flag) = (CString::new("screenshot-raw").ok()?, CString::new("video").ok()?);
    let mut args = [
        sys::mpv_node { u: sys::mpv_node__bindgen_ty_1 { string: cmd.as_ptr() as *mut _ }, format: sys::mpv_format_MPV_FORMAT_STRING },
        sys::mpv_node { u: sys::mpv_node__bindgen_ty_1 { string: flag.as_ptr() as *mut _ }, format: sys::mpv_format_MPV_FORMAT_STRING },
    ];
    let mut list = sys::mpv_node_list { num: 2, values: args.as_mut_ptr(), keys: std::ptr::null_mut() };
    let mut node = sys::mpv_node { u: sys::mpv_node__bindgen_ty_1 { list: &mut list }, format: sys::mpv_format_MPV_FORMAT_NODE_ARRAY };
    let mut result = sys::mpv_node { u: sys::mpv_node__bindgen_ty_1 { int64: 0 }, format: sys::mpv_format_MPV_FORMAT_NONE };
    let code = unsafe { sys::mpv_command_node(mpv.ctx.as_ptr(), &mut node, &mut result) };
    if code < 0 {
        return None;
    }
    let mut out = None;
    unsafe {
        if result.format == sys::mpv_format_MPV_FORMAT_NODE_MAP {
            let map = &*result.u.list;
            let (mut w, mut h, mut stride, mut data) = (0i64, 0i64, 0i64, None::<&[u8]>);
            let mut format_ok = false;
            for i in 0..map.num as isize {
                let key = CStr::from_ptr(*map.keys.offset(i)).to_string_lossy();
                let value = &*map.values.offset(i);
                match (key.as_ref(), value.format) {
                    ("w", sys::mpv_format_MPV_FORMAT_INT64) => w = value.u.int64,
                    ("h", sys::mpv_format_MPV_FORMAT_INT64) => h = value.u.int64,
                    ("stride", sys::mpv_format_MPV_FORMAT_INT64) => stride = value.u.int64,
                    ("format", sys::mpv_format_MPV_FORMAT_STRING) => {
                        format_ok = CStr::from_ptr(value.u.string).to_bytes() == b"bgr0";
                    }
                    ("data", sys::mpv_format_MPV_FORMAT_BYTE_ARRAY) => {
                        let bytes = &*value.u.ba;
                        data = Some(std::slice::from_raw_parts(bytes.data as *const u8, bytes.size));
                    }
                    _ => {}
                }
            }
            if let Some(data) = data
                && format_ok
                && w > 0
                && h > 0
                && stride >= w * 4
                && data.len() >= (stride * (h - 1) + w * 4) as usize
            {
                out = Some((w as u32, h as u32, stride as usize, data.to_vec()));
            }
        }
        sys::mpv_free_node_contents(&mut result);
    }
    out
}

/// Nothing plays: the display link holds, so it does not wake the render
/// thread sixty times a second for no frame.
pub(super) fn rest(shared: &Shared) {
    if let Some(clock) = shared.clock.lock().unwrap().as_mut() {
        clock.set_running(false);
    }
}


/// Applies one mpv event to the shared status. Returns true when playback ended.
pub(super) fn handle_event(
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
            // What is of the item goes into the status of the item alone:
            // while the next load waits for the server, the status is of
            // that load already, and the item before still plays.
            let mine = worker.owns(&status);
            match (*name, change) {
                ("time-pos", PropertyData::Double(v)) => {
                    if mine {
                        status.position = *v;
                        status.position_at = Some(Instant::now());
                    }
                    if let Some(active) = session.as_mut() {
                        active.position = *v;
                    }
                }
                ("duration", PropertyData::Double(v)) if mine => status.duration = *v,
                ("video-params/w", PropertyData::Int64(v)) if mine => status.video_w = (*v).max(0) as u32,
                ("video-params/h", PropertyData::Int64(v)) if mine => status.video_h = (*v).max(0) as u32,
                ("dwidth", PropertyData::Int64(v)) => {
                    shared.dwidth.store((*v).max(0) as u32, Ordering::Relaxed);
                    worker.size_changed = true;
                }
                ("dheight", PropertyData::Int64(v)) => {
                    shared.dheight.store((*v).max(0) as u32, Ordering::Relaxed);
                    worker.size_changed = true;
                }
                ("sub-delay", PropertyData::Double(v)) if mine => status.sub_delay = *v,
                ("audio-delay", PropertyData::Double(v)) if mine => status.audio_delay = *v,
                ("pause", PropertyData::Flag(v)) => {
                    if status.paused != *v {
                        worker.report_soon();
                    }
                    if mine {
                        status.paused = *v;
                    }
                }
                ("paused-for-cache", PropertyData::Flag(v)) => {
                    if mine {
                        status.buffering = *v;
                    }
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
                ("path", PropertyData::Str(path)) if mine => status.path = Some(path.to_string()),
                ("track-list", PropertyData::Str(json)) if mine => {
                    let tracks: Vec<Track> = serde_json::from_str(json).unwrap_or_default();
                    let tracks: Vec<Track> =
                        tracks.into_iter().filter(|t| t.kind != TrackKind::Video).collect();
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
            if session.is_some() && worker.owns(&status) {
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
            if worker.owns(&status) {
                if *reason == mpv_end_file_reason::Error {
                    status.error = Some("mpv could not play this stream".to_string());
                }
                status.reached_end = *reason == mpv_end_file_reason::Eof;
            }
            // Only the active session's end is meaningful (stop+load emits one too).
            session.is_some()
        }
        Event::Shutdown => true,
        _ => false,
    }
}

pub(super) fn mpv_error_text(code: i32) -> String {
    unsafe {
        let ptr = sys::mpv_error_string(code);
        if ptr.is_null() {
            format!("mpv error {code}")
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

pub(super) fn progress_of(session: &Session, paused: bool, worker: &Worker) -> Progress {
    Progress {
        item_id: session.item_id.clone(),
        play_session_id: session.play_session_id.clone(),
        // The position of this item: the status can be of the load that
        // replaces it, or cleared by the app that closed the player.
        position_ticks: (session.position * TICKS_PER_SECOND as f64) as i64,
        paused,
        volume: worker.volume as i64,
        muted: worker.muted,
        play_method: session.play_method.clone(),
        media_source_id: session.media_source_id.clone(),
    }
}

/// Reports the stop of an item. The reporter ends the stream the server
/// made for it with the report, by the bool of the load: a remux has the
/// method "DirectStream" in its reports, so the name does not tell.
pub(super) fn report_stop(session: &Session, paused: bool, worker: &Worker) {
    let _ = worker.reports.send(Report {
        client: session.client.clone(),
        kind: ReportKind::Stopped,
        progress: progress_of(session, paused, worker),
        transcoding: session.transcoding,
    });
}

/// Reports the stop to Jellyfin and marks the session ended. The status
/// is marked when it is of this item alone: while the next load waits for
/// the server the status is of that load (`Player::prepare`), and the end
/// of the item before, by itself or by a stop, must not end it. On
/// 2026-11-10 the review found that it did: the app then took the load
/// for an item that ended, and went on in the queue or closed the player.
pub(super) fn finish(shared: &Shared, session: &mut Option<Session>, worker: &mut Worker) {
    let Some(active) = session.take() else { return };
    let (mine, paused, reached_end) = {
        let status = shared.status.lock().unwrap();
        let mine = worker.owns(&status);
        (mine, status.paused, mine && status.reached_end && status.error.is_none())
    };
    report_stop(&active, paused, worker);
    worker.scheduled = None;
    worker.item_id = None;
    worker.stall_since = None;
    worker.stalled = false;
    worker.forget_subtitles();
    // A load that ended before the item could play has failed.
    if let Some(token) = worker.pending.take().and_then(|pending| pending.load_token()) {
        shared.emit(PlayerEvent::LoadFailed { token });
    } else if reached_end {
        shared.emit(PlayerEvent::Ended);
    }
    let mut status = shared.status.lock().unwrap();
    // The app closed the player and took the end already
    // (`acknowledge_end`): the status stays at rest.
    if mine && worker.owns(&status) && status.state != PlayState::Idle {
        status.state = PlayState::Ended;
    }
    drop(status);
    rest(shared);
}
