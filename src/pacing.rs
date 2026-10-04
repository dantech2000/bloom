// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frame pacing: the refresh of the display drives the video.
//!
//! A 24 fps film on a 60 Hz display needs 2 and 3 refreshes for each frame,
//! in turn. Before, mpv timed each frame by the audio clock, the render
//! call waited for that time, and the UI drew the frame at the next
//! refresh. When the time of a frame fell near a refresh, the frame made
//! that refresh one time and missed it the next: 4 refreshes, then 1, which
//! the eye sees as a stutter. Now one clock decides, a `CVDisplayLink` on
//! the display of the window:
//!
//! - mpv runs in a `display-` video-sync mode (`display-resample`, or
//!   `display-vdrop` in a SyncPlay group, see [`video_sync`]), with
//!   `display-fps-override` set to the rate of the display: libmpv has no
//!   other way to learn it (`VOCTRL_GET_DISPLAY_FPS` reaches nobody). mpv
//!   then picks the frame for each refresh itself, 2, 3, 2, 3, ... for
//!   24 fps on 60 Hz, and the render call does not wait.
//! - At each tick of the display link the worker reports the swap to mpv
//!   (`mpv_render_context_report_swap`), as mpv's own macOS output
//!   (cocoa-cb) does from its display link. mpv then hands over the frame
//!   for the next refresh and the worker renders it at once, a few
//!   milliseconds into the refresh period. A repeat of the frame before is
//!   not rendered again (`MPV_RENDER_PARAM_SKIP_RENDERING`), and no frame
//!   is rendered while the window draws nothing.
//! - A rendered frame carries the time of the tick it was made after. The
//!   UI shows it at its first draw at least half a period later, never at
//!   a draw of the tick of the frame, so where the draw of the UI lands
//!   inside the period does not matter (measured: 0 to 3 ms after our
//!   tick, or just before the next one; gpui has a display link of its
//!   own). The UI draws at every refresh while video plays, so a frame
//!   never waits for a draw request.
//!
//! The picture is one refresh behind mpv's model: the sound leads by 17 ms
//! at 60 Hz, well under the 45 ms people notice. In a SyncPlay group mpv
//! does not touch the speed of the sound (`display-vdrop`), so the group
//! correction, which measures the sound, is not fought.
//!
//! Switches: `BLOOM_OLD_PACING=1` gives the timing of two changes ago,
//! `BLOOM_AUDIO_PACING=1` the one before this change (audio sync, a
//! blocking render call, no display link). `dev/jctl pacing` measures.

use std::{
    cell::Cell,
    ffi::c_void,
    ptr,
    sync::{
        OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use gpui_kit::{Context, Window};

use crate::{app::Bloom, macos::send};

/// How the frames are timed; see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Timers of the worker and of the UI (two changes ago).
    Old,
    /// Audio sync, a blocking render call, a draw on each frame.
    Audio,
    /// The display drives: display sync and the display link.
    Display,
}

pub fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        if std::env::var_os("BLOOM_OLD_PACING").is_some() {
            Mode::Old
        } else if std::env::var_os("BLOOM_AUDIO_PACING").is_some() {
            Mode::Audio
        } else {
            Mode::Display
        }
    })
}

/// The `video-sync` option for mpv. In a group the position of the sound is
/// what the group compares, so the sound keeps its speed and the video
/// repeats or drops a frame when the two clocks drift (`display-vdrop`).
/// Alone, mpv resamples the sound by a fraction of a percent instead
/// (`display-resample`), which is not heard, and no frame is ever dropped.
pub fn video_sync(mode: Mode, in_group: bool) -> &'static str {
    match (mode, in_group) {
        (Mode::Display, false) => "display-resample",
        (Mode::Display, true) => "display-vdrop",
        _ => "audio",
    }
}

/// The refresh rate to tell mpv, in Hz: the measured one, unless it is more
/// than 0.1 Hz from the nominal one (then the measurement is poor), as
/// cocoa-cb does. `None` when the display reports no rate.
pub fn refresh_rate(measured_period_secs: f64, nominal_hz: Option<f64>) -> Option<f64> {
    let measured = (measured_period_secs > 0.).then(|| 1. / measured_period_secs);
    match (measured, nominal_hz) {
        (Some(m), Some(n)) if (m - n).abs() <= 0.1 => Some(m),
        (_, Some(n)) => Some(n),
        (Some(m), None) if (20. ..=400.).contains(&m) => Some(m),
        _ => None,
    }
}

/// Whether the UI, at its draw at `now_ns`, shows a frame made after the
/// tick at `tick_ns`. A frame is shown at the first draw at least half a
/// period after its tick, never at a draw of its own tick: the worker
/// makes it a few milliseconds into the period, and the draw of the UI
/// can land before or after that moment. (The ticks of gpui's own display
/// link and of ours are not in step, so counts of ticks cannot be
/// compared; times can.) A frame made with audio timing has no tick (0)
/// and is shown at once.
pub fn due(tick_ns: u64, now_ns: u64, period_ns: u64) -> bool {
    tick_ns == 0 || now_ns.saturating_sub(tick_ns) >= period_ns / 2
}

/// Whether a frame shown at `now_ns` missed the draw it was meant for.
pub fn late(tick_ns: u64, now_ns: u64, period_ns: u64) -> bool {
    tick_ns != 0 && now_ns.saturating_sub(tick_ns) >= period_ns * 3 / 2
}

/// Histograms for `pacing clock`, in bins of 1 ms: where in the period the
/// draws of the UI land after our tick, and the times between our ticks.
static PHASES: std::sync::Mutex<[u32; 24]> = std::sync::Mutex::new([0; 24]);
static INTERVALS: std::sync::Mutex<[u32; 24]> = std::sync::Mutex::new([0; 24]);

fn bin(list: &std::sync::Mutex<[u32; 24]>, ns: u64) {
    let bin = (ns / 1_000_000).min(23) as usize;
    list.lock().unwrap()[bin] += 1;
}

pub fn note_phase(ns: u64) {
    bin(&PHASES, ns);
}

/// The UI took frames before and took none for this long: the window
/// draws nothing (covered, or on another space), and no frame is rendered
/// for it until it draws again.
const UI_GONE: Duration = Duration::from_millis(250);

/// Whether somebody draws the frames: `seen_ns` is the clock of the last
/// draw, 0 before the first one (a test with no UI, or a window that has
/// not drawn yet, gets its frames).
pub fn ui_watching(seen_ns: u64, now_ns: u64) -> bool {
    seen_ns == 0 || now_ns.saturating_sub(seen_ns) < UI_GONE.as_nanos() as u64
}

// ----- counters for `dev/jctl pacing` -----------------------------------------

/// Frames the UI held for the next tick, because its draw came after the
/// frame of the same tick.
static HELD: AtomicU64 = AtomicU64::new(0);
/// Frames the UI showed two or more ticks after their own: a late draw.
static LATE: AtomicU64 = AtomicU64::new(0);
/// Repeats mpv asked for that were not rendered again.
static REPEATS: AtomicU64 = AtomicU64::new(0);
/// Frames not rendered because the window drew nothing.
static UNSEEN: AtomicU64 = AtomicU64::new(0);
/// Frames not rendered because the size of the picture was not known yet.
static NO_SIZE: AtomicU64 = AtomicU64::new(0);
/// Frames rendered in display sync, and frames rendered with audio timing.
static SYNCED: AtomicU64 = AtomicU64::new(0);
static UNSYNCED: AtomicU64 = AtomicU64::new(0);

pub fn count_held() {
    HELD.fetch_add(1, Ordering::Relaxed);
}
pub fn count_late() {
    LATE.fetch_add(1, Ordering::Relaxed);
}
pub fn count_repeat() {
    REPEATS.fetch_add(1, Ordering::Relaxed);
}
pub fn count_unseen() {
    UNSEEN.fetch_add(1, Ordering::Relaxed);
}
pub fn count_no_size() {
    NO_SIZE.fetch_add(1, Ordering::Relaxed);
}
pub fn count_rendered(display_synced: bool) {
    if display_synced { &SYNCED } else { &UNSYNCED }.fetch_add(1, Ordering::Relaxed);
}

/// The counters since the last call.
pub fn report() -> String {
    format!(
        "{:?} pacing: held={} late={} repeats_skipped={} unseen={} no_size={} rendered synced={} audio={} | draw phase by ms {:?} | tick intervals by ms {:?}",
        mode(),
        HELD.swap(0, Ordering::Relaxed),
        LATE.swap(0, Ordering::Relaxed),
        REPEATS.swap(0, Ordering::Relaxed),
        UNSEEN.swap(0, Ordering::Relaxed),
        NO_SIZE.swap(0, Ordering::Relaxed),
        SYNCED.swap(0, Ordering::Relaxed),
        UNSYNCED.swap(0, Ordering::Relaxed),
        std::mem::take(&mut *PHASES.lock().unwrap()),
        std::mem::take(&mut *INTERVALS.lock().unwrap()),
    )
}

// ----- the display link -------------------------------------------------------

/// What the display link writes at each tick, read from any thread.
pub struct ClockState {
    /// Ticks since the start; 0 before the first.
    pub ticks: AtomicU64,
    /// `crate::player::clock_ns` of the last tick.
    pub tick_ns: AtomicU64,
    /// The period the display reported at the last tick, in nanoseconds.
    pub period_ns: AtomicU64,
    /// The display the link follows.
    pub display: AtomicU32,
    on_tick: Box<dyn Fn() + Send + Sync>,
}

/// A `CVDisplayLink` that calls `on_tick` at each refresh of the display.
/// The link is never released: `CVDisplayLinkStop` returns before its
/// thread ends, and a release can race with a last callback (gpui leaks
/// its links for the same reason).
pub struct DisplayClock {
    link: *mut sys::CVDisplayLink,
    state: &'static ClockState,
}

// The link is a thread-safe CoreVideo object; the state is atomics.
unsafe impl Send for DisplayClock {}

unsafe extern "C" fn on_display_tick(
    _link: *mut sys::CVDisplayLink,
    _now: *const sys::CVTimeStamp,
    output: *const sys::CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut c_void,
) -> i32 {
    let state = unsafe { &*(context as *const ClockState) };
    let now = crate::player::clock_ns();
    let last = state.tick_ns.swap(now, Ordering::AcqRel);
    if last != 0 {
        bin(&INTERVALS, now - last);
    }
    if !output.is_null() {
        let output = unsafe { &*output };
        if output.flags & sys::VIDEO_REFRESH_PERIOD_VALID != 0 && output.video_time_scale > 0 {
            let period = output.video_refresh_period as f64 / output.video_time_scale as f64;
            state.period_ns.store((period * 1e9) as u64, Ordering::Relaxed);
        }
    }
    state.ticks.fetch_add(1, Ordering::AcqRel);
    (state.on_tick)();
    0
}

impl DisplayClock {
    /// Starts a link on the main display; `set_display` moves it.
    pub fn start(on_tick: Box<dyn Fn() + Send + Sync>) -> Result<Self> {
        let state: &'static ClockState = Box::leak(Box::new(ClockState {
            ticks: AtomicU64::new(0),
            tick_ns: AtomicU64::new(0),
            period_ns: AtomicU64::new(0),
            display: AtomicU32::new(0),
            on_tick,
        }));
        let mut link: *mut sys::CVDisplayLink = ptr::null_mut();
        unsafe {
            let code = sys::CVDisplayLinkCreateWithActiveCGDisplays(&mut link);
            if code != 0 || link.is_null() {
                return Err(anyhow!("CVDisplayLinkCreateWithActiveCGDisplays failed ({code})"));
            }
            let code = sys::CVDisplayLinkSetOutputCallback(
                link,
                on_display_tick,
                state as *const ClockState as *mut c_void,
            );
            if code != 0 {
                return Err(anyhow!("CVDisplayLinkSetOutputCallback failed ({code})"));
            }
        }
        let mut clock = Self { link, state };
        clock.set_display(unsafe { sys::CGMainDisplayID() });
        let code = unsafe { sys::CVDisplayLinkStart(link) };
        if code != 0 {
            return Err(anyhow!("CVDisplayLinkStart failed ({code})"));
        }
        Ok(clock)
    }

    pub fn state(&self) -> &'static ClockState {
        self.state
    }

    /// Follows another display; nothing happens for the same one.
    pub fn set_display(&mut self, display: u32) {
        if display == 0 || self.state.display.load(Ordering::Relaxed) == display {
            return;
        }
        let code = unsafe { sys::CVDisplayLinkSetCurrentCGDisplay(self.link, display) };
        if code == 0 {
            self.state.display.store(display, Ordering::Relaxed);
        } else {
            log::warn!("CVDisplayLinkSetCurrentCGDisplay({display}) failed ({code})");
        }
    }

    /// The refresh rate of the display, in Hz.
    pub fn fps(&self) -> Option<f64> {
        let measured = unsafe { sys::CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.link) };
        let nominal = unsafe { sys::CVDisplayLinkGetNominalOutputVideoRefreshPeriod(self.link) };
        let nominal = (nominal.flags & sys::TIME_IS_INDEFINITE == 0
            && nominal.time_value > 0
            && nominal.time_scale > 0)
            .then(|| nominal.time_scale as f64 / nominal.time_value as f64);
        refresh_rate(measured, nominal)
    }

    pub fn describe(&self) -> String {
        format!(
            "display={} fps={:?} period={:.3}ms ticks={}",
            self.state.display.load(Ordering::Relaxed),
            self.fps(),
            self.state.period_ns.load(Ordering::Relaxed) as f64 / 1e6,
            self.state.ticks.load(Ordering::Relaxed),
        )
    }
}

impl Drop for DisplayClock {
    fn drop(&mut self) {
        // Stopped, not released: see the struct doc.
        unsafe { sys::CVDisplayLinkStop(self.link) };
    }
}

/// The display the window is on, as a `CGDirectDisplayID`.
pub fn display_of(window: &Window) -> Option<u32> {
    let ns_window = crate::pip::ns_window(window)?;
    let screen = send!(crate::macos::Id, ns_window, c"screen");
    if screen.is_null() {
        return None;
    }
    let description = send!(crate::macos::Id, screen, c"deviceDescription");
    let key = send!(
        crate::macos::Id, crate::macos::class(c"NSString"), c"stringWithUTF8String:",
        c"NSScreenNumber".as_ptr() => *const std::ffi::c_char
    );
    let number = send!(crate::macos::Id, description, c"objectForKey:", key => crate::macos::Id);
    if number.is_null() {
        return None;
    }
    Some(send!(u32, number, c"unsignedIntValue"))
}

thread_local! {
    static DISPLAY_CHECKED: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Tells the player which display the window is on, at most once a second.
pub fn note_window(window: &Window, player: &crate::player::Player) {
    let due = DISPLAY_CHECKED.with(|checked| {
        let due = checked.get().is_none_or(|at| at.elapsed() >= Duration::from_secs(1));
        if due {
            checked.set(Some(Instant::now()));
        }
        due
    });
    if due && let Some(display) = display_of(window) {
        player.set_display(display);
    }
}

impl Bloom {
    /// `pacing`: the numbers since the last call. `pacing clock`: the
    /// display link. `pacing file <path>`: plays a file on this Mac, muted,
    /// for the other frame rates (make one with ffmpeg). `pacing speed <x>`
    /// sets the playback speed. `pacing hide` takes the window off the
    /// screen and `pacing show` brings it back: a window that draws nothing.
    pub fn debug_pacing(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" => crate::perf::pacing_report(),
            "clock" => self.player.pacing_state(),
            "speed" => match arg.parse::<f32>() {
                Ok(speed) => {
                    self.set_speed(speed, cx);
                    format!("speed {speed}")
                }
                Err(_) => "error: pacing speed <x>".into(),
            },
            "hide" | "show" => {
                let Some(ns_window) = crate::pip::ns_window(window) else {
                    return "error: no window".into();
                };
                // Off the screen and back behind the other windows, so no
                // focus moves.
                if verb == "hide" {
                    send!((), ns_window, c"orderOut:", ptr::null_mut() => crate::macos::Id);
                } else {
                    send!((), ns_window, c"orderBack:", ptr::null_mut() => crate::macos::Id);
                }
                let visible = send!(bool, ns_window, c"isVisible");
                format!("{verb}: visible={visible}")
            }
            "file" if !arg.is_empty() => {
                let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
                    return "error: not signed in".into();
                };
                self.muted = true;
                self.player.set_muted(true);
                self.player.play(crate::player::PlayRequest {
                    client,
                    item_id: "local-pacing-test".into(),
                    url: arg.to_string(),
                    title: arg.rsplit('/').next().unwrap_or(arg).to_string(),
                    start_secs: 0.,
                    paused: false,
                    token: 0,
                    play_session_id: None,
                    play_method: "DirectPlay".into(),
                    media_source_id: String::new(),
                    subtitles: Vec::new(),
                });
                self.player.set_muted(true);
                self.player_status = self.player.status();
                self.player_open = true;
                self.start_player_poll(cx);
                cx.notify();
                format!("playing {arg}")
            }
            _ => "error: pacing [clock | file <path>]".into(),
        }
    }
}

#[allow(non_snake_case, non_upper_case_globals)]
mod sys {
    use std::ffi::c_void;

    pub enum CVDisplayLink {}

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CVSMPTETime {
        pub subframes: i16,
        pub subframe_divisor: i16,
        pub counter: u32,
        pub time_type: u32,
        pub flags: u32,
        pub hours: i16,
        pub minutes: i16,
        pub seconds: i16,
        pub frames: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CVTimeStamp {
        pub version: u32,
        pub video_time_scale: i32,
        pub video_time: i64,
        pub host_time: u64,
        pub rate_scalar: f64,
        pub video_refresh_period: i64,
        pub smpte_time: CVSMPTETime,
        pub flags: u64,
        pub reserved: u64,
    }

    /// `kCVTimeStampVideoRefreshPeriodValid`.
    pub const VIDEO_REFRESH_PERIOD_VALID: u64 = 1 << 3;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CVTime {
        pub time_value: i64,
        pub time_scale: i32,
        pub flags: i32,
    }

    /// `kCVTimeIsIndefinite`.
    pub const TIME_IS_INDEFINITE: i32 = 1;

    pub type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
        link: *mut CVDisplayLink,
        now: *const CVTimeStamp,
        output: *const CVTimeStamp,
        flags_in: u64,
        flags_out: *mut u64,
        context: *mut c_void,
    ) -> i32;

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        pub fn CVDisplayLinkCreateWithActiveCGDisplays(link: *mut *mut CVDisplayLink) -> i32;
        pub fn CVDisplayLinkSetCurrentCGDisplay(link: *mut CVDisplayLink, display: u32) -> i32;
        pub fn CVDisplayLinkSetOutputCallback(
            link: *mut CVDisplayLink,
            callback: CVDisplayLinkOutputCallback,
            context: *mut c_void,
        ) -> i32;
        pub fn CVDisplayLinkStart(link: *mut CVDisplayLink) -> i32;
        pub fn CVDisplayLinkStop(link: *mut CVDisplayLink) -> i32;
        pub fn CVDisplayLinkGetActualOutputVideoRefreshPeriod(link: *mut CVDisplayLink) -> f64;
        pub fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link: *mut CVDisplayLink) -> CVTime;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGMainDisplayID() -> u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sync_mode_follows_the_group() {
        assert_eq!(video_sync(Mode::Display, false), "display-resample");
        assert_eq!(video_sync(Mode::Display, true), "display-vdrop");
        assert_eq!(video_sync(Mode::Audio, false), "audio");
        assert_eq!(video_sync(Mode::Old, true), "audio");
    }

    #[test]
    fn the_measured_rate_counts_when_it_is_close_to_the_nominal_one() {
        let close = refresh_rate(1. / 59.98, Some(60.)).unwrap();
        assert!((close - 59.98).abs() < 1e-6);
        // A poor measurement: the nominal rate.
        assert_eq!(refresh_rate(1. / 58.5, Some(60.)), Some(60.));
        assert_eq!(refresh_rate(0., Some(120.)), Some(120.));
        // No nominal rate: the measurement, when it is a possible one.
        assert_eq!(refresh_rate(1. / 60., None), Some(60.));
        assert_eq!(refresh_rate(1. / 5., None), None);
        assert_eq!(refresh_rate(0., None), None);
    }

    #[test]
    fn a_frame_is_shown_at_the_first_draw_half_a_period_after_its_tick() {
        let ms = |n: u64| n * 1_000_000;
        let period = ms(16);
        // A draw of the tick of the frame, before or after the frame.
        assert!(!due(ms(100), ms(100), period));
        assert!(!due(ms(100), ms(105), period));
        // The draw of the next tick, early or late in its period.
        assert!(due(ms(100), ms(116), period));
        assert!(due(ms(100), ms(122), period));
        assert!(!late(ms(100), ms(122), period));
        // The draw after that: the frame missed one.
        assert!(late(ms(100), ms(133), period));
        // A frame with audio timing has no tick.
        assert!(due(0, ms(1), period));
        assert!(!late(0, ms(500), period));
    }

    #[test]
    fn frames_are_rendered_until_the_ui_has_been_gone_for_a_while() {
        let ms = |n: u64| n * 1_000_000;
        assert!(ui_watching(0, ms(5000)));
        assert!(ui_watching(ms(1000), ms(1100)));
        assert!(!ui_watching(ms(1000), ms(1300)));
        assert!(ui_watching(ms(1000), ms(900)));
    }
}
