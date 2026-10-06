// SPDX-License-Identifier: AGPL-3.0-or-later
//! Trailer playback for the home hero. A second mpv core, separate from the
//! main player, renders into GPU surfaces. It reports nothing to Jellyfin and
//! resolves YouTube links through mpv's ytdl hook, which needs yt-dlp.

use std::{
    ffi::c_void,
    path::PathBuf,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Result, anyhow};
use libmpv2::{
    Format, Mpv,
    events::{Event, PropertyData},
    mpv_end_file_reason,
};
use libmpv2_sys as sys;

use crate::video_surface::{self, GlRenderer, VideoFrame};

/// Volume of a trailer once the user turns the sound on.
const VOLUME: i64 = 40;

/// State of the trailer of one load. `load` tells the UI which request the
/// rest belongs to, so a late event of an old trailer is not mistaken.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrailerStatus {
    pub load: u64,
    pub position: f64,
    pub duration: f64,
    pub ended: bool,
    pub failed: bool,
}

enum Cmd {
    Load { url: String, load: u64 },
    Stop,
    Muted(bool),
    Paused(bool),
    /// Ends the worker, so mpv is closed before the process exits (see
    /// `player::shut_down_all`).
    Quit,
}

/// The trailer players that started a worker, for the quit of the app.
static TRAILERS: Mutex<Vec<TrailerPlayer>> = Mutex::new(Vec::new());

/// Asks every trailer worker to end; [`running`] says when they are gone.
pub(crate) fn ask_to_quit() {
    let trailers: Vec<TrailerPlayer> = TRAILERS.lock().unwrap().clone();
    for trailer in &trailers {
        trailer.send(Cmd::Quit);
    }
}

pub(crate) fn running() -> bool {
    let trailers: Vec<TrailerPlayer> = TRAILERS.lock().unwrap().clone();
    trailers
        .iter()
        .any(|trailer| trailer.commands.lock().unwrap().is_some())
}

struct Shared {
    status: Mutex<TrailerStatus>,
    /// Newest frame, with the load it belongs to and its number in that load.
    frame: Mutex<Option<(u64, u64, VideoFrame)>>,
    target_w: AtomicU32,
    target_h: AtomicU32,
    /// Wakes the worker: mpv has an event or a frame, or a command came.
    wake: (Mutex<bool>, std::sync::Condvar),
}

impl Shared {
    fn wake(&self) {
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_one();
    }
}

/// What mpv's update callback gets: the flag for the worker and its wake-up.
struct RenderAsk {
    needed: AtomicBool,
    shared: Arc<Shared>,
}

/// Handle owned by the UI. Cheap to clone.
#[derive(Clone)]
pub struct TrailerPlayer {
    shared: Arc<Shared>,
    commands: Arc<Mutex<Option<mpsc::Sender<Cmd>>>>,
    ytdl: Option<PathBuf>,
}

impl Default for TrailerPlayer {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                status: Mutex::new(TrailerStatus::default()),
                frame: Mutex::new(None),
                target_w: AtomicU32::new(1280),
                target_h: AtomicU32::new(720),
                wake: (Mutex::new(false), std::sync::Condvar::new()),
            }),
            commands: Arc::new(Mutex::new(None)),
            ytdl: find_ytdl(),
        }
    }
}

impl TrailerPlayer {
    /// False when yt-dlp is missing; trailers cannot play then.
    pub fn available(&self) -> bool {
        self.ytdl.is_some()
    }

    pub fn status(&self) -> TrailerStatus {
        self.shared.status.lock().unwrap().clone()
    }

    /// Newest frame of the given load and its number, if one has been rendered.
    pub fn frame(&self, load: u64) -> Option<(u64, VideoFrame)> {
        match &*self.shared.frame.lock().unwrap() {
            Some((id, seq, frame)) if *id == load => Some((*seq, frame.clone())),
            _ => None,
        }
    }

    /// Size of the area the video covers, in device pixels.
    pub fn set_target_size(&self, width: u32, height: u32) {
        self.shared.target_w.store(width.max(16), Ordering::Relaxed);
        self.shared
            .target_h
            .store(height.max(16), Ordering::Relaxed);
    }

    /// Starts a trailer, replacing the running one.
    pub fn play(&self, url: String, load: u64, muted: bool) {
        let Some(ytdl) = self.ytdl.clone() else {
            return;
        };
        *self.shared.status.lock().unwrap() = TrailerStatus {
            load,
            ..Default::default()
        };
        *self.shared.frame.lock().unwrap() = None;
        self.ensure_thread(ytdl);
        self.send(Cmd::Muted(muted));
        self.send(Cmd::Paused(false));
        self.send(Cmd::Load { url, load });
    }

    pub fn stop(&self) {
        self.send(Cmd::Stop);
        *self.shared.frame.lock().unwrap() = None;
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(Cmd::Muted(muted));
    }

    pub fn set_paused(&self, paused: bool) {
        self.send(Cmd::Paused(paused));
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = self.commands.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
            // The worker sleeps between rounds.
            self.shared.wake();
        }
    }

    fn ensure_thread(&self, ytdl: PathBuf) {
        let mut slot = self.commands.lock().unwrap();
        if slot.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        *slot = Some(tx);
        {
            let mut trailers = TRAILERS.lock().unwrap();
            trailers.retain(|t| !Arc::ptr_eq(&t.commands, &self.commands));
            trailers.push(self.clone());
        }
        let shared = self.shared.clone();
        let commands = self.commands.clone();
        thread::Builder::new()
            .name("mpv-trailer".into())
            .spawn(move || {
                if let Err(err) = run(shared.clone(), rx, ytdl) {
                    log::warn!("trailer worker failed: {err:#}");
                    shared.status.lock().unwrap().failed = true;
                }
                *commands.lock().unwrap() = None;
            })
            .expect("spawn trailer thread");
    }
}

/// yt-dlp on `PATH`, which mpv's ytdl hook runs to resolve a YouTube link.
fn find_ytdl() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    // An app started from Finder gets a short PATH, so the usual install
    // places of Homebrew and Nix are searched too.
    let user = std::env::var("USER").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let extra = [
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        format!("/etc/profiles/per-user/{user}/bin"),
        format!("{home}/.nix-profile/bin"),
        "/run/current-system/sw/bin".to_string(),
    ];
    std::env::split_paths(&path)
        .chain(extra.into_iter().map(PathBuf::from))
        .map(|dir| dir.join("yt-dlp"))
        .find(|candidate| candidate.is_file())
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
    // SAFETY: the context is the leaked `RenderAsk` of `run`, alive for the
    // process; mpv calls this from its own threads.
    let ask = unsafe { &*(ctx as *const RenderAsk) };
    ask.needed.store(true, Ordering::Release);
    ask.shared.wake();
}

fn run(shared: Arc<Shared>, rx: mpsc::Receiver<Cmd>, ytdl: PathBuf) -> Result<()> {
    let mut mpv = Mpv::with_initializer(|init| {
        init.set_option("vo", "libmpv")?;
        init.set_option("hwdec", "auto-safe")?;
        init.set_option("keep-open", "no")?;
        init.set_option("idle", "yes")?;
        init.set_option("terminal", "no")?;
        init.set_option("config", "no")?;
        init.set_option("sub", "no")?;
        init.set_option("mute", "yes")?;
        init.set_option("volume", VOLUME)?;
        init.set_option("ytdl", "yes")?;
        // The same roots as the player (see `player::tls_roots_file`).
        init.set_option("tls-verify", "yes")?;
        init.set_option("tls-ca-file", crate::player::tls_roots_file())?;
        init.set_option(
            "script-opts",
            format!("ytdl_hook-ytdl_path={}", ytdl.display()),
        )?;
        // A backdrop needs no more than 1080p.
        // H.264 first: the Mac decodes it in hardware. YouTube's VP9 and AV1
        // streams decode in software and cost several times the CPU.
        init.set_option(
            "ytdl-format",
            "bestvideo[vcodec^=avc1][height<=1080]+bestaudio/\
             bestvideo[height<=1080]+bestaudio/best[height<=1080]",
        )?;
        // A short read-ahead is enough for a backdrop and keeps memory low.
        init.set_option("demuxer-max-bytes", "48MiB")?;
        init.set_option("demuxer-max-back-bytes", "8MiB")?;
        // Built-in scripts the hero has no use for; an older mpv may lack one.
        for script in [
            "osc",
            "load-stats-overlay",
            "load-osd-console",
            "load-auto-profiles",
            "load-select",
            "load-positioning",
            "load-commands",
            "load-context-menu",
        ] {
            let _ = init.set_option(script, "no");
        }
        Ok(())
    })
    .map_err(|e| anyhow!("could not create libmpv core: {e}"))?;

    // Wake the loop from mpv's threads on events and on new frames.
    {
        let shared = shared.clone();
        mpv.set_wakeup_callback(move || shared.wake());
    }
    let needs_render: &'static RenderAsk =
        Box::leak(Box::new(RenderAsk { needed: AtomicBool::new(false), shared: shared.clone() }));
    let mut renderer = create_renderer(&mpv, needs_render)?;

    mpv.observe_property("time-pos", Format::Double, 1)?;
    mpv.observe_property("duration", Format::Double, 2)?;
    // The size of the picture comes as an event: a property read on this
    // thread waits for the core, which can wait for this thread's render
    // (`render.h`, "Threading"). The crop search still reads the core
    // (`Crop::measure`), a few times per trailer.
    mpv.observe_property("dwidth", Format::Int64, 3)?;
    mpv.observe_property("dheight", Format::Int64, 4)?;
    // The size of the picture of the active load; a frame skipped for want
    // of it is drawn once the size is there.
    let mut dims = (0u32, 0u32);
    let mut dims_wanted = false;

    // The load in progress; `None` between trailers.
    let mut active: Option<u64> = None;
    let mut crop = Crop::default();
    // The bars of the trailers that played, by link.
    let mut crops: std::collections::HashMap<String, KnownCrop> = Default::default();
    let mut current_url = String::new();
    let mut muted = true;
    let mut load_started = std::time::Instant::now();

    loop {
        // What CoreVideo and the GL driver autorelease in this turn goes
        // at its end, not when the thread ends.
        let _pool = crate::macos::Pool::new();
        let mut restarted = false;
        while let Some(event) = mpv.wait_event(0.0) {
            let mut status = shared.status.lock().unwrap();
            restarted |= matches!(event, Ok(Event::PlaybackRestart));
            match event {
                Ok(Event::PropertyChange { name, change, .. }) if active.is_some() => {
                    match (name, change) {
                        // The search for bars plays a later part, unseen.
                        ("time-pos", PropertyData::Double(v)) if crop.shown() => {
                            status.position = v
                        }
                        ("duration", PropertyData::Double(v)) => status.duration = v,
                        ("dwidth", PropertyData::Int64(v)) => dims.0 = v.max(0) as u32,
                        ("dheight", PropertyData::Int64(v)) => dims.1 = v.max(0) as u32,
                        _ => {}
                    }
                }
                // A stop or a replaced file also ends a file; only a real end
                // or a failure of the active trailer counts.
                Ok(Event::EndFile(reason)) if active.is_some() => {
                    if reason == mpv_end_file_reason::Eof {
                        status.ended = true;
                        active = None;
                    } else if reason == mpv_end_file_reason::Error {
                        status.failed = true;
                        active = None;
                    }
                }
                // The trailer is back at its start after the search for
                // bars: it shows from here, and its sound starts with it.
                Ok(Event::PlaybackRestart) if !crop.started => crop.started = true,
                Ok(Event::PlaybackRestart) if crop.rewinding => {
                    crop.rewinding = false;
                    log::debug!(
                        "trailer shows {} ms after its load",
                        load_started.elapsed().as_millis()
                    );
                    crops.insert(current_url.clone(), crop.known.clone());
                    let _ = mpv.set_property("aid", if muted { "no" } else { "auto" });
                }
                Ok(Event::Shutdown) => return Ok(()),
                Ok(_) => {}
                // The wrapper reports a failed end-file as `Err`.
                Err(err) => {
                    log::debug!("trailer event error: {err}");
                    if active.take().is_some() {
                        status.failed = true;
                    }
                }
            }
        }

        // mpv sends a size only when it differs from the last one it sent:
        // a trailer that replaces one of the same size, with no round of
        // this loop in between, gets no event. Playback has started, so the
        // core has the size: it is read once, in that case alone.
        if restarted && active.is_some() && (dims.0 == 0 || dims.1 == 0) {
            dims = (
                mpv.get_property::<i64>("dwidth").unwrap_or(0).max(0) as u32,
                mpv.get_property::<i64>("dheight").unwrap_or(0).max(0) as u32,
            );
        }

        loop {
            match rx.try_recv() {
                Ok(Cmd::Load { url, load }) => {
                    active = Some(load);
                    // The size is of the file before until its events come.
                    dims = (0, 0);
                    dims_wanted = false;
                    // Bars known from an earlier play need no search.
                    crop = crops.get(&url).cloned().map(Crop::known).unwrap_or_default();
                    let _ = mpv.set_property("video-crop", crop.known.rect.as_str());
                    if crop.done {
                        let _ = mpv.set_property("aid", if muted { "no" } else { "auto" });
                        let _ = mpv.set_property("speed", 1.);
                    } else {
                        // No sound during the search.
                        let _ = mpv.set_property("aid", "no");
                        let _ = mpv.set_property("speed", CROP_SPEED);
                    }
                    current_url = url.clone();
                    load_started = std::time::Instant::now();
                    log::debug!("trailer load, bars known: {}", crop.done);
                    if let Err(err) = mpv.command("loadfile", &[url.as_str(), "replace"]) {
                        log::debug!("trailer loadfile failed: {err}");
                        shared.status.lock().unwrap().failed = true;
                        active = None;
                    }
                }
                Ok(Cmd::Stop) => {
                    active = None;
                    let _ = mpv.command("stop", &[]);
                }
                Ok(Cmd::Muted(now)) => {
                    muted = now;
                    let _ = mpv.set_property("mute", muted);
                    // A muted trailer needs no audio decode or output at all,
                    // and neither does the search for bars.
                    let audio = !muted && (active.is_none() || crop.shown());
                    let _ = mpv.set_property("aid", if audio { "auto" } else { "no" });
                }
                Ok(Cmd::Paused(paused)) => {
                    let _ = mpv.set_property("pause", paused);
                }
                // The render context goes first, then the core: `renderer`
                // is declared after `mpv`.
                Ok(Cmd::Quit) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) => break,
                // The UI is gone.
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }

        // A frame mpv asked for, or the one skipped before the size came.
        let size_arrived = dims_wanted && dims.0 > 0 && dims.1 > 0;
        if needs_render.needed.swap(false, Ordering::AcqRel) || size_arrived {
            let flags = unsafe { sys::mpv_render_context_update(renderer.ctx) };
            if (flags & (sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64) != 0 || size_arrived)
                && let Some(load) = active
            {
                dims_wanted = false;
                match render_frame(&mpv, &mut renderer, &shared, &mut crop, load, dims) {
                    Ok(true) => {}
                    Ok(false) => dims_wanted = true,
                    Err(err) => log::debug!("trailer render skipped: {err}"),
                }
            }
        }

        // Sleep until something happens: an event or a frame of mpv, or a
        // command. The wait is a bound, not the clock.
        let (lock, cvar) = &shared.wake;
        let mut flag = lock.lock().unwrap();
        if !*flag && !needs_render.needed.load(Ordering::Acquire) {
            let idle = if active.is_some() { Duration::from_millis(4) } else { Duration::from_secs(1) };
            let (guard, _) = cvar.wait_timeout(flag, idle).unwrap();
            flag = guard;
        }
        *flag = false;
        crate::perf::count_loop(crate::perf::Loop::Trailer);
    }
}

fn create_renderer(mpv: &Mpv, needs_render: &'static RenderAsk) -> Result<Renderer> {
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
    let code =
        unsafe { sys::mpv_render_context_create(&mut ctx, mpv.ctx.as_ptr(), params.as_mut_ptr()) };
    if code < 0 || ctx.is_null() {
        return Err(anyhow!("mpv_render_context_create failed ({code})"));
    }
    unsafe {
        sys::mpv_render_context_set_update_callback(
            ctx,
            Some(on_render_update),
            needs_render as *const RenderAsk as *mut c_void,
        );
    }
    Ok(Renderer { ctx, gl })
}

/// Frames looked at for black bars before the trailer shows without a crop.
const CROP_FRAMES: u32 = 120;
/// Every how many frames one is measured.
const CROP_EVERY: u32 = 8;
/// Largest bar at the top, as a part of the picture height (2.76:1 in 16:9
/// has 0.18).
const CROP_MAX: f64 = 0.19;
/// Largest bar at a side, as a part of the picture width (4:3 in 16:9 has
/// 0.125).
const CROP_MAX_SIDE: f64 = 0.13;
/// Measured frames that end the search.
const CROP_SAMPLES: usize = 4;
/// The search plays the trailer this fast, to see more of it in less time.
const CROP_SPEED: f64 = 4.;

/// The black bars found in a trailer, kept for the next time it plays.
#[derive(Clone, Default)]
struct KnownCrop {
    /// Value of mpv's `video-crop`; empty for a picture with no bars.
    rect: String,
    /// Part of the picture height cut from the top and from the bottom.
    bar: f64,
    /// Part of the picture width cut from each side.
    side: f64,
    /// Display size before the crop, to see if mpv reports the cropped size.
    full: (u32, u32),
}

/// Search for the black bars a trailer has in its picture (a wide film in a
/// 16:9 video). The trailer starts fast and without sound, and its frames
/// are measured, not shown. Once the bars are known and cut away the
/// trailer goes back to its start and shows, so the picture fills the hero
/// as a backdrop does and the sound starts with the first frame.
#[derive(Default)]
struct Crop {
    /// Frames rendered for this trailer so far.
    frames: u32,
    /// Bar sizes measured (top, side), as parts of the picture size.
    samples: Vec<(f64, f64)>,
    /// The first frame of this trailer is ready. Until then mpv draws the
    /// last frame of the trailer before, which must not be measured or shown.
    started: bool,
    /// The search is over.
    done: bool,
    /// The trailer is on its way back to the start; frames stay hidden.
    rewinding: bool,
    /// What the search found.
    known: KnownCrop,
    /// Time spent on the measurement, for the log.
    cost: Duration,
    /// When the first frame came, for the log.
    first_frame: Option<std::time::Instant>,
}

impl Crop {
    /// A trailer whose bars are known from an earlier play.
    fn known(known: KnownCrop) -> Self {
        Self {
            done: true,
            known,
            ..Default::default()
        }
    }

    /// Frames go to the hero.
    fn shown(&self) -> bool {
        self.started && self.done && !self.rewinding
    }

    /// Looks at the frame just rendered at `w` x `h`. Sets the crop in mpv
    /// when the bars are known, and sends the trailer back to its start.
    fn measure(&mut self, mpv: &Mpv, renderer: &mut Renderer, (w, h): (u32, u32), src: (u32, u32)) {
        self.frames += 1;
        let first_frame = *self.first_frame.get_or_insert_with(std::time::Instant::now);
        // Frames some time apart, so that one scene does not decide alone.
        if self.frames % CROP_EVERY != 0 {
            return;
        }
        let started = std::time::Instant::now();
        let rows = renderer.gl.black_bars();
        let columns = renderer.gl.black_sides();
        self.cost += started.elapsed();
        // A bar larger than that of the widest film format is a dark scene
        // or a logo on black.
        if let (Some(rows), Some(columns)) = (rows, columns)
            && (rows as f64) < h as f64 * CROP_MAX
            && (columns as f64) < w as f64 * CROP_MAX_SIDE
        {
            self.samples
                .push((rows as f64 / h as f64, columns as f64 / w as f64));
        }
        if self.samples.len() < CROP_SAMPLES && self.frames < CROP_FRAMES {
            return;
        }
        self.done = true;
        self.rewinding = true;
        // A dark scene looks like a larger bar, and a rating card or a logo
        // that fills the video like none at all, so the size most frames
        // agree on counts; of two such sizes, the smaller.
        let common = |values: Vec<f64>| {
            let votes = |v: f64| values.iter().filter(|o| (**o - v).abs() < 0.01).count();
            values
                .iter()
                .copied()
                .max_by(|a, b| votes(*a).cmp(&votes(*b)).then(b.total_cmp(a)))
                .unwrap_or(0.)
        };
        let bar = common(self.samples.iter().map(|s| s.0).collect());
        let side = common(self.samples.iter().map(|s| s.1).collect());
        let width = mpv.get_property::<i64>("width").unwrap_or(0);
        let height = mpv.get_property::<i64>("height").unwrap_or(0);
        if !self.samples.is_empty() && width > 0 && height > 0 {
            // Two more rows hide the soft edge of a bar.
            let rows = if bar < 0.008 { 0 } else { (bar * height as f64).ceil() as i64 + 2 };
            let columns = if side < 0.008 { 0 } else { (side * width as f64).ceil() as i64 + 2 };
            let (crop_w, crop_h) = (width - 2 * columns, height - 2 * rows);
            if (rows > 0 || columns > 0) && crop_h >= height / 3 && crop_w >= width / 3 {
                let rect = format!("{crop_w}x{crop_h}+{columns}+{rows}");
                if mpv.set_property("video-crop", rect.as_str()).is_ok() {
                    self.known = KnownCrop {
                        rect,
                        bar: rows as f64 / height as f64,
                        side: columns as f64 / width as f64,
                        full: src,
                    };
                }
            }
        }
        log::debug!(
            "trailer crop {:?} after {} frames in {} ms, reading the pixels took {:.1} ms",
            self.known.rect,
            self.frames,
            first_frame.elapsed().as_millis(),
            self.cost.as_secs_f64() * 1000.
        );
        let _ = mpv.set_property("speed", 1.);
        let _ = mpv.command("seek", &["0", "absolute"]);
    }
}

/// Renders the current frame large enough to cover the hero, but not larger
/// than the source, then publishes its GPU surface. False when the size of
/// the picture (`dims`, from the events) is not known yet: the frame is
/// drawn once it is.
fn render_frame(
    mpv: &Mpv,
    renderer: &mut Renderer,
    shared: &Shared,
    crop: &mut Crop,
    load: u64,
    dims: (u32, u32),
) -> Result<bool> {
    let (mut src_w, mut src_h) = dims;
    if src_w == 0 || src_h == 0 {
        return Ok(false);
    }
    let full = (src_w, src_h);
    // The picture is smaller after the crop; mpv may still report the full one.
    if crop.done && !crop.known.rect.is_empty() && full == crop.known.full {
        src_w = (src_w as f64 * (1. - 2. * crop.known.side)) as u32;
        src_h = (src_h as f64 * (1. - 2. * crop.known.bar)) as u32;
    }
    let target_w = shared.target_w.load(Ordering::Relaxed).max(16);
    let target_h = shared.target_h.load(Ordering::Relaxed).max(16);
    let scale = (target_w as f64 / src_w as f64)
        .max(target_h as f64 / src_h as f64)
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
    if crop.started && !crop.done {
        crop.measure(mpv, renderer, (w, h), full);
        renderer.gl.finish()?;
        return Ok(true);
    }
    if !crop.shown() {
        renderer.gl.finish()?;
        return Ok(true);
    }
    let frame = renderer.gl.finish()?;
    let mut slot = shared.frame.lock().unwrap();
    let seq = match &*slot {
        Some((id, seq, _)) if *id == load => seq + 1,
        _ => 0,
    };
    *slot = Some((load, seq, frame));
    Ok(true)
}

/// The trailer link the web plugin would pick: the best YouTube entry by its
/// name, or the first link when none is from YouTube.
pub fn best_trailer(trailers: &[crate::jellyfin::NamedRef]) -> Option<String> {
    let score = |name: &str| {
        let name = name.to_lowercase();
        let base = if name.contains("official trailer") {
            5.0
        } else if name.contains("final trailer") || name.contains("main trailer") {
            4.0
        } else if name.contains("trailer") {
            3.0
        } else if name.contains("teaser") {
            2.0
        } else {
            1.0
        };
        let special = ["sign language", "audio descri", "vertical"]
            .iter()
            .any(|word| name.contains(word));
        if special { base - 0.5 } else { base }
    };
    let is_youtube = |url: &str| url.contains("youtube.com") || url.contains("youtu.be");
    let mut best: Option<(f64, &str)> = None;
    for trailer in trailers {
        let Some(url) = trailer.url.as_deref().filter(|url| is_youtube(url)) else {
            continue;
        };
        let rank = score(trailer.name.as_deref().unwrap_or_default());
        if best.is_none_or(|(top, _)| rank > top) {
            best = Some((rank, url));
        }
    }
    best.map(|(_, url)| url.to_string())
        .or_else(|| trailers.iter().find_map(|t| t.url.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::NamedRef;

    fn entry(name: &str, url: &str) -> NamedRef {
        NamedRef {
            name: Some(name.into()),
            url: Some(url.into()),
        }
    }

    #[test]
    fn picks_the_official_trailer() {
        let trailers = [
            entry("Teaser", "https://youtu.be/a"),
            entry("Official Trailer", "https://www.youtube.com/watch?v=b"),
            entry("Official Trailer (Audio Described)", "https://youtu.be/c"),
            entry("Trailer", "https://example.com/d.mp4"),
        ];
        assert_eq!(
            best_trailer(&trailers).as_deref(),
            Some("https://www.youtube.com/watch?v=b")
        );
    }

    #[test]
    fn falls_back_to_the_first_link() {
        let trailers = [entry("Trailer", "https://example.com/d.mp4")];
        assert_eq!(
            best_trailer(&trailers).as_deref(),
            Some("https://example.com/d.mp4")
        );
        assert_eq!(best_trailer(&[]), None);
    }
}
