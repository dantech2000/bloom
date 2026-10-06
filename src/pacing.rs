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
//!   frames task of the UI (`Bloom::new`) waits until a set phase of the
//!   period after that tick ([`frame_phase`], 0.55 of the period: 9.2 ms
//!   at 60 Hz), then asks for a draw and runs gpui's frame step at once
//!   (`gpui_macos::wake_frame_sources`). The draw lands on the GPU with 8
//!   to 12 ms before the compositor's deadline for the next refresh. Before
//!   this, the frame was drawn at gpui's own step, at the tick after its
//!   own: that left the GPU about 4 ms, and one frame in seven of a 4K
//!   picture was shown a refresh late (measured: 75 to 278 of 1441 frames
//!   a minute). A draw of the tick itself never takes the frame
//!   ([`due`]): the frame is shown at the refresh after the next tick in
//!   both timings, so the sound lead below is the same.
//!
//! The picture is one refresh behind mpv's model: the sound leads by 17 ms
//! at 60 Hz, well under the 45 ms people notice. In a SyncPlay group mpv
//! does not touch the speed of the sound (`display-vdrop`), so the group
//! correction, which measures the sound, is not fought.
//!
//! Switches: `BLOOM_FRAME_PHASE_MS=<ms>` sets the phase (0: the draw at the
//! next tick, the timing before), `BLOOM_OLD_PACING=1` gives the timing of
//! two changes ago, `BLOOM_AUDIO_PACING=1` the one before this change
//! (audio sync, a blocking render call, no display link). `dev/jctl pacing`
//! measures: the ruler is the time the display showed each frame
//! ([`trace`]), the `jumps` figure of before counts the draw requests.

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

/// Where in the refresh period the UI draws a new frame, as a fraction of
/// the period after the tick of the frame. Measured at 60 Hz: the display
/// link's `outputTime - now` is 22.5 ms, so the compositor wants the GPU
/// work for a refresh done about 5.8 ms (0.35 of the period) after the
/// tick before it, and a draw at the tick has only that long. A draw at
/// 0.55 of the period comes after that deadline and well before the next
/// one (1.35 periods after the tick): the GPU has 0.8 of a period less its
/// own 2 to 5 ms. mpv's render is done 3.0 to 3.8 ms after the tick, so at
/// 60 Hz the frame waits about 5 ms for its draw; at 144 Hz (phase 3.8 ms)
/// it is drawn as soon as it is rendered. See the tests for each rate.
const PHASE_OF_PERIOD: f64 = 0.55;

/// A frame is due a little before its draw ([`due_after`]): the timer of
/// the frames task is never early, but a draw it asks for must find the
/// frame due whatever the rounding of the two clocks.
const DUE_MARGIN: Duration = Duration::from_millis(2);

/// `BLOOM_FRAME_PHASE_MS`: a phase in milliseconds instead of the fraction,
/// for an A/B on other hardware; 0 is the timing before (the draw at the
/// next tick). It must be below the period of the display.
fn phase_override() -> Option<u64> {
    static PHASE: OnceLock<Option<u64>> = OnceLock::new();
    *PHASE.get_or_init(|| {
        std::env::var("BLOOM_FRAME_PHASE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
    })
}

/// How long after the tick of a frame the UI draws it, in nanoseconds, for
/// a display of this period; `None` draws it at the next tick (the timing
/// before). Computed for each frame from the period the display link
/// reports, so it follows the window to a display of another rate.
pub fn frame_phase(period_ns: u64) -> Option<u64> {
    match phase_override() {
        Some(0) => None,
        Some(ms) => Some(ms * 1_000_000),
        None => Some(phase::current(period_ns)),
    }
}

/// How long after its tick a frame is due at a draw: [`DUE_MARGIN`] before
/// the draw the frames task asks for (at most an eighth of the period, and
/// never under a quarter of it), else half a period. Either way after the
/// compositor's first deadline at 60 Hz, so a frame cannot land a refresh
/// early at a draw of its own tick that comes late.
pub fn due_after(period_ns: u64) -> u64 {
    match frame_phase(period_ns) {
        Some(phase) => due_before(phase, period_ns),
        None => period_ns / 2,
    }
}

/// The due time for a draw at `phase_ns` after the tick.
fn due_before(phase_ns: u64, period_ns: u64) -> u64 {
    let margin = (DUE_MARGIN.as_nanos() as u64).min(period_ns / 8);
    phase_ns.saturating_sub(margin).max(period_ns / 4)
}

/// The phase measured, not modelled: the time the display showed each
/// frame moves the draw.
///
/// [`frame_phase`] is a model of the compositor at 60 Hz (the deadline
/// for a refresh is at the refresh before it, see the tests of this
/// module). On a display of another rate the model may be wrong in either
/// direction: a draw too late is shown a refresh late (`lag` 2), a draw
/// too early a refresh early (`lag` 0, ahead of the sound). The loop here
/// keeps the draw where the frames come out at `lag` 1 on the display the
/// window is on: each frame's presented time, with the GPU end of its
/// draw, is one observation ([`PhaseLoop::observe`]).
///
/// - A frame at `lag` 2 from a clean draw (asked for at the phase, taken
///   by the GPU at once, GPU work of the usual length) says the deadline
///   is earlier than the model: the phase steps earlier. A late frame
///   from a draw that was itself late, or waited for the GPU, or took
///   long on it, says nothing about the deadline and moves nothing. (A
///   span of late frames after a stall of the compositor, see the traces
///   of 2026-10-06, takes the phase to the floor, where it stays: nothing
///   is lost at the floor.)
/// - A frame at `lag` 0 says the phase is too early: it steps later.
/// - The step is a sixteenth of the period, with at least
///   [`PhaseLoop::SETTLE`] frames between steps (a step shows in the
///   frames drawn after it, two refreshes on); so the loop cannot
///   oscillate between two frames, and converges within a second.
/// - The phase stays in `[quarter period, 0.8 period]`: the frame must be
///   due after gpui's own draw of the tick, and drawn before the next
///   tick. It does not relax back toward the model by itself: a relaxation
///   is a late frame each time it is wrong. It starts from the model again
///   at a new period (another display) and at each playback start.
/// - `BLOOM_FRAME_PHASE_MS` fixes the phase: the loop is off.
pub mod phase {
    use std::sync::Mutex;

    /// Where a frame came out against the refresh of its tick, in
    /// refreshes (1 is the design); when the GPU work of its draw ended,
    /// relative to that refresh (`outputTime`), in nanoseconds; and
    /// whether the draw was clean: asked for within [`DRAW_SLACK`] of the
    /// phase, on the GPU within [`GPU_WAIT`] of its commit, and done in
    /// [`GPU_USUAL`] or less.
    #[derive(Clone, Copy, Debug)]
    pub struct Observation {
        pub seq: u64,
        pub lag: i32,
        pub gpu_end_rel_vsync: i64,
        pub clean: bool,
    }

    pub const DRAW_SLACK: u64 = 1_500_000;
    pub const GPU_WAIT: u64 = 2_000_000;
    pub const GPU_USUAL: u64 = 6_000_000;

    #[derive(Debug)]
    pub struct PhaseLoop {
        period_ns: u64,
        phase_ns: u64,
        /// The latest GPU end (relative to the refresh) of a frame at
        /// lag 1, decayed by [`Self::DECAY`] at each frame: what the
        /// deadline is known to be later than (for the report).
        made_it: Option<i64>,
        /// The seq of the frame observed at the last step, and how many
        /// steps were taken (for the report).
        stepped_at: Option<u64>,
        pub steps_earlier: u32,
        pub steps_later: u32,
    }

    impl PhaseLoop {
        /// Frames between steps.
        pub const SETTLE: u64 = 3;
        /// How much `made_it` relaxes at each frame: a lag-1 frame with a
        /// GPU end 5 ms before the deadline stops counting after 100 frames.
        const DECAY: i64 = 50_000;

        pub fn new(period_ns: u64) -> Self {
            Self {
                period_ns,
                phase_ns: Self::default_phase(period_ns),
                made_it: None,
                stepped_at: None,
                steps_earlier: 0,
                steps_later: 0,
            }
        }

        fn default_phase(period_ns: u64) -> u64 {
            (period_ns as f64 * super::PHASE_OF_PERIOD) as u64
        }

        pub fn floor(period_ns: u64) -> u64 {
            period_ns / 4
        }

        pub fn ceiling(period_ns: u64) -> u64 {
            period_ns * 4 / 5
        }

        pub fn phase(&self) -> u64 {
            self.phase_ns
        }

        pub fn period(&self) -> u64 {
            self.period_ns
        }

        /// Starts from the model again for this period.
        pub fn reset(&mut self, period_ns: u64) {
            *self = Self::new(period_ns);
        }

        pub fn observe(&mut self, o: Observation) {
            let step = (self.period_ns / 16).max(1);
            let settled = self.stepped_at.is_none_or(|at| o.seq > at + Self::SETTLE);
            match o.lag {
                1 => {
                    self.made_it = Some(match self.made_it {
                        Some(m) => (m - Self::DECAY).max(o.gpu_end_rel_vsync),
                        None => o.gpu_end_rel_vsync,
                    });
                }
                lag if lag >= 2 => {
                    // Evidence of an earlier deadline only from a clean
                    // draw: a draw that was late or slow would have missed
                    // the modelled deadline too.
                    if o.clean && settled && self.phase_ns > Self::floor(self.period_ns) {
                        self.phase_ns = self.phase_ns.saturating_sub(step).max(Self::floor(self.period_ns));
                        self.stepped_at = Some(o.seq);
                        self.steps_earlier += 1;
                    }
                }
                _ => {
                    if settled && self.phase_ns < Self::ceiling(self.period_ns) {
                        self.phase_ns = (self.phase_ns + step).min(Self::ceiling(self.period_ns));
                        self.stepped_at = Some(o.seq);
                        self.steps_later += 1;
                        // What made it before may not with the later draw.
                        self.made_it = None;
                    }
                }
            }
        }
    }

    static LOOP: Mutex<Option<PhaseLoop>> = Mutex::new(None);

    /// The phase for this period: the loop's, started from the model at
    /// a new period; the fixed one under `BLOOM_FRAME_PHASE_MS`.
    pub fn current(period_ns: u64) -> u64 {
        if let Some(fixed) = super::phase_override() {
            return fixed * 1_000_000;
        }
        let mut guard = LOOP.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(l) if l.period() == period_ns => l.phase(),
            _ => {
                let l = PhaseLoop::new(period_ns);
                let phase = l.phase();
                *guard = Some(l);
                phase
            }
        }
    }

    /// A playback starts: from the model again, and with no frame of the
    /// playback before waiting for the times of its draw.
    pub fn reset() {
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = None;
        for slot in IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
            slot.draw_id = 0;
            slot.period_ns = 0;
        }
    }

    pub fn observe(o: Observation) {
        if super::phase_override().is_some() {
            return;
        }
        if let Some(l) = LOOP.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            l.observe(o);
        }
    }

    /// A draw in flight: its id, the frame's seq, the refresh of the
    /// frame's tick, the period, and how late after the phase the draw
    /// came. A draw's completions come two refreshes after it; eight
    /// entries hold a second of frames.
    #[derive(Clone, Copy, Default)]
    struct InFlight {
        draw_id: u64,
        seq: u64,
        vsync_ns: u64,
        period_ns: u64,
        draw_after_phase_ns: u64,
        /// When the draw took the frame, on the clock of the ticks.
        adopt_ns: u64,
    }

    static IN_FLIGHT: Mutex<[InFlight; 8]> =
        Mutex::new([InFlight { draw_id: 0, seq: 0, vsync_ns: 0, period_ns: 0, draw_after_phase_ns: 0, adopt_ns: 0 }; 8]);

    /// Whether the times of a finished draw can be those of the draw that
    /// took the frame `f`. The frame is noted under the number the next
    /// draw will get; when that draw does not happen (no drawable), a later
    /// draw gets the number, and its times say nothing about this frame.
    /// The draw of a frame is submitted within a refresh of taking it, is
    /// for the display the loop runs on, and is shown zero to three
    /// refreshes after the frame's own.
    fn is_the_draw_of(f: &InFlight, submitted_ns: i64, lag: i32, loop_period_ns: Option<u64>) -> bool {
        let after = submitted_ns - f.adopt_ns as i64;
        (-1_000_000..=f.period_ns as i64).contains(&after)
            && (0..=3).contains(&lag)
            && loop_period_ns.is_none_or(|period| period == f.period_ns)
    }

    #[cfg(test)]
    mod feedback_tests {
        use super::*;

        const PERIOD: u64 = 16_666_667;

        fn frame() -> InFlight {
            InFlight { draw_id: 7, seq: 1, vsync_ns: 0, period_ns: PERIOD, draw_after_phase_ns: 0, adopt_ns: 1_000_000_000 }
        }

        /// The number noted for a frame can go to a later draw, when the
        /// draw of the frame got no drawable: its times are not the frame's.
        #[test]
        fn the_times_of_another_draw_do_not_count_for_a_frame() {
            let f = frame();
            let at = |ms: f64| f.adopt_ns as i64 + (ms * 1e6) as i64;
            assert!(is_the_draw_of(&f, at(0.7), 1, Some(PERIOD)), "the draw of the frame itself");
            assert!(is_the_draw_of(&f, at(0.7), 2, None), "late by a refresh, before a loop exists");
            assert!(!is_the_draw_of(&f, at(40.), 1, Some(PERIOD)), "a draw two refreshes later took the number");
            assert!(!is_the_draw_of(&f, at(-5.), 1, Some(PERIOD)), "submitted before the frame was taken");
            assert!(!is_the_draw_of(&f, at(0.7), 9, Some(PERIOD)), "shown far from the frame's refresh");
            assert!(!is_the_draw_of(&f, at(0.7), -1, Some(PERIOD)), "shown before the frame's refresh");
            assert!(!is_the_draw_of(&f, at(0.7), 1, Some(8_333_333)), "the loop runs on another display now");
        }
    }

    /// A draw at `now_ns` took the frame `seq` of the tick at `tick_ns`
    /// (`Bloom::sync_frame`).
    pub fn adopted(seq: u64, draw_id: u64, tick_ns: u64, vsync_ns: u64, period_ns: u64, now_ns: u64) {
        if vsync_ns == 0 || period_ns == 0 {
            return;
        }
        let phase = current(period_ns);
        let entry = InFlight {
            draw_id,
            seq,
            vsync_ns,
            period_ns,
            draw_after_phase_ns: now_ns.saturating_sub(tick_ns).saturating_sub(phase),
            adopt_ns: now_ns,
        };
        let mut ring = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        ring[(draw_id % 8) as usize] = entry;
    }

    /// The presented and GPU end times of a draw came (any thread; set as
    /// `present_trace`'s feedback by the render thread).
    pub fn on_presented(draw_id: u64, draw: gpui_apple::present_trace::Draw) {
        let f = {
            let ring = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
            ring[(draw_id % 8) as usize]
        };
        if f.draw_id != draw_id || f.period_ns == 0 || draw.presented == 0. || draw.gpu_end == 0. || draw.submitted == 0. {
            return;
        }
        let presented = super::trace::media_to_clock_ns(draw.presented) as i64;
        let gpu_end = super::trace::media_to_clock_ns(draw.gpu_end) as i64;
        let submitted = super::trace::media_to_clock_ns(draw.submitted) as i64;
        let lag = ((presented - f.vsync_ns as i64) as f64 / f.period_ns as f64).round() as i32;
        let loop_period = LOOP.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|l| l.period());
        if !is_the_draw_of(&f, submitted, lag, loop_period) {
            return;
        }
        let secs = |s: f64| (s * 1e9) as u64;
        let clean = f.draw_after_phase_ns <= DRAW_SLACK
            && secs(draw.gpu_start - draw.submitted) <= GPU_WAIT
            && secs(draw.gpu_end - draw.gpu_start) <= GPU_USUAL;
        observe(Observation { seq: f.seq, lag, gpu_end_rel_vsync: gpu_end - f.vsync_ns as i64, clean });
    }

    pub fn describe() -> String {
        match LOOP.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(l) => format!(
                "phase loop: phase={:.2}ms (model {:.2}) steps earlier={} later={} made_it={:?}",
                l.phase_ns as f64 / 1e6,
                PhaseLoop::default_phase(l.period_ns) as f64 / 1e6,
                l.steps_earlier,
                l.steps_later,
                l.made_it.map(|m| m as f64 / 1e6),
            ),
            None => "phase loop: not started".into(),
        }
    }
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
/// tick at `tick_ns`. A frame is shown at the first draw [`due_after`] its
/// tick, never at a draw of its own tick: the worker makes it a few
/// milliseconds into the period, and gpui's own draw of the tick can land
/// before or after that moment. (The ticks of gpui's own display link and
/// of ours are not in step, so counts of ticks cannot be compared; times
/// can.) A frame made with audio timing has no tick (0) and is shown at
/// once.
pub fn due(tick_ns: u64, now_ns: u64, period_ns: u64) -> bool {
    tick_ns == 0 || now_ns.saturating_sub(tick_ns) >= due_after(period_ns)
}

/// Whether a frame shown at `now_ns` missed the draw it was meant for.
pub fn late(tick_ns: u64, now_ns: u64, period_ns: u64) -> bool {
    tick_ns != 0 && now_ns.saturating_sub(tick_ns) >= period_ns * 3 / 2
}

/// Histograms for `pacing clock`, in bins of 1 ms: where in the period the
/// draws of the UI land after our tick, and the times between our ticks.
static PHASES: std::sync::Mutex<[u32; 24]> = std::sync::Mutex::new([0; 24]);
static INTERVALS: std::sync::Mutex<[u32; 24]> = std::sync::Mutex::new([0; 24]);
/// `outputTime - now` of the display link callback, and how long after
/// its `now` the callback ran (with the trace on).
static LEADS: std::sync::Mutex<[u32; 24]> = std::sync::Mutex::new([0; 24]);
static WAKES: std::sync::Mutex<[u32; 24]> = std::sync::Mutex::new([0; 24]);

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
        "{:?} pacing: held={} late={} repeats_skipped={} unseen={} no_size={} rendered synced={} audio={} | draw phase by ms {:?} | tick intervals by ms {:?} | link lead by ms {:?} | link wake by ms {:?}",
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
        std::mem::take(&mut *LEADS.lock().unwrap()),
        std::mem::take(&mut *WAKES.lock().unwrap()),
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
    /// The refresh the last tick is for (its `outputTime`), in `clock_ns`.
    pub vsync_ns: AtomicU64,
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
    running: bool,
}

// The link is a thread-safe CoreVideo object; the state is atomics.
unsafe impl Send for DisplayClock {}

unsafe extern "C" fn on_display_tick(
    _link: *mut sys::CVDisplayLink,
    cv_now: *const sys::CVTimeStamp,
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
        let vsync = trace::host_to_clock_ns(output.host_time);
        state.vsync_ns.store(vsync, Ordering::Release);
        if trace::enabled() {
            if !cv_now.is_null() {
                let cv_now = trace::host_to_clock_ns(unsafe { (*cv_now).host_time });
                bin(&LEADS, vsync.saturating_sub(cv_now));
                bin(&WAKES, now.saturating_sub(cv_now));
            }
        }
    }
    state.ticks.fetch_add(1, Ordering::AcqRel);
    crate::perf::count_loop(crate::perf::Loop::Link);
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
            vsync_ns: AtomicU64::new(0),
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
        let mut clock = Self { link, state, running: false };
        clock.set_display(unsafe { sys::CGMainDisplayID() });
        let code = unsafe { sys::CVDisplayLinkStart(link) };
        if code != 0 {
            return Err(anyhow!("CVDisplayLinkStart failed ({code})"));
        }
        clock.running = true;
        Ok(clock)
    }

    pub fn state(&self) -> &'static ClockState {
        self.state
    }

    /// Runs the link, or holds it while nothing plays: a link that ticks
    /// for no frame wakes the render thread sixty times a second.
    pub fn set_running(&mut self, on: bool) {
        if self.running == on {
            return;
        }
        let code = unsafe {
            if on { sys::CVDisplayLinkStart(self.link) } else { sys::CVDisplayLinkStop(self.link) }
        };
        if code == 0 {
            self.running = on;
        } else {
            log::warn!("CVDisplayLink{}: failed ({code})", if on { "Start" } else { "Stop" });
        }
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
        let period = self.state.period_ns.load(Ordering::Relaxed);
        format!(
            "display={} fps={:?} period={:.3}ms phase={:?}ms due_after={:.2}ms ticks={}",
            self.state.display.load(Ordering::Relaxed),
            self.fps(),
            period as f64 / 1e6,
            frame_phase(period).map(|ns| ns as f64 / 1e6),
            due_after(period) as f64 / 1e6,
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
    /// `pacing`: the numbers since the last call; the first call turns the
    /// trace on (see [`trace`]), so the second call is the first window.
    /// `pacing clock`: the display link. `pacing file <path>`: plays a file
    /// on this Mac, muted, for the other frame rates (make one with
    /// ffmpeg). `pacing speed <x>` sets the playback speed. `pacing hide`
    /// takes the window off the screen and `pacing show` brings it back: a
    /// window that draws nothing.
    pub fn debug_pacing(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" => {
                let started = crate::player::clock_ns();
                let (_, period) = self.player.clock_time();
                let report = format!(
                    "{}\n  {}\n  {}\n  {}",
                    crate::perf::pacing_report(period),
                    trace::report(period, 60),
                    trace::other_draws_report(period),
                    phase::describe()
                );
                trace::enable();
                // The report runs on the main thread: how long it held it.
                let ended = crate::player::clock_ns();
                trace::main_job("pacing report", started, ended);
                format!("{report}\n  report took {:.2} ms", (ended - started) as f64 / 1e6)
            }
            // A stall of the main thread of <ms>, to see what one late
            // draw does to the frames after it.
            "stall" => match arg.parse::<u64>() {
                Ok(ms) => {
                    let until = Instant::now() + Duration::from_millis(ms);
                    while Instant::now() < until {
                        std::hint::spin_loop();
                    }
                    format!("stalled {ms} ms")
                }
                Err(_) => "error: pacing stall <ms>".into(),
            },
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
            // One draw outside the frame rhythm, to see what the
            // compositor does with it.
            "poke" => {
                cx.notify();
                "poked".into()
            }
            // `pacing screen`: the displays and their frames; `pacing
            // screen <display id>` moves the window onto that display
            // (a test with a virtual display of another refresh rate).
            "screen" => screen_verb(arg, window),
            _ => "error: pacing [clock | file <path> | poke | screen [<display>]]".into(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NsRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// The `NSScreen`s with their display ids and frames, or the window moved
/// onto the one with the id given, 20 points in from its corner and no
/// larger than it.
fn screen_verb(arg: &str, window: &Window) -> String {
    let screens = send!(crate::macos::Id, crate::macos::class(c"NSScreen"), c"screens");
    let count = send!(usize, screens, c"count");
    let key = send!(
        crate::macos::Id, crate::macos::class(c"NSString"), c"stringWithUTF8String:",
        c"NSScreenNumber".as_ptr() => *const std::ffi::c_char
    );
    let wanted: Option<u32> = arg.parse().ok();
    let mut out = Vec::new();
    for i in 0..count {
        let screen = send!(crate::macos::Id, screens, c"objectAtIndex:", i => usize);
        let description = send!(crate::macos::Id, screen, c"deviceDescription");
        let number = send!(crate::macos::Id, description, c"objectForKey:", key => crate::macos::Id);
        let id = if number.is_null() { 0 } else { send!(u32, number, c"unsignedIntValue") };
        let frame = send!(NsRect, screen, c"frame");
        out.push(format!("display {id}: {}x{} at ({}, {})", frame.w, frame.h, frame.x, frame.y));
        if wanted == Some(id) {
            let Some(ns_window) = crate::pip::ns_window(window) else {
                return "error: no window".into();
            };
            let current = send!(NsRect, ns_window, c"frame");
            let target = NsRect {
                x: frame.x + 20.,
                y: frame.y + 20.,
                w: current.w.min(frame.w - 40.),
                h: current.h.min(frame.h - 40.),
            };
            send!((), ns_window, c"setFrame:display:animate:", target => NsRect, true => bool, false => bool);
            return format!("moved to display {id}: {}x{} at ({}, {})", target.w, target.h, target.x, target.y);
        }
    }
    if wanted.is_some() {
        out.push("error: no such display".into());
    }
    out.join("\n")
}

/// A trace of each frame from mpv to the glass, with the time the display
/// showed it (ground truth from the drawable's presented handler, see
/// `gpui_apple::present_trace`), for `dev/jctl pacing`. Off until the
/// first `pacing` command or `BLOOM_PACING_TRACE=1`: the hooks in the frame
/// path then cost one atomic read each. The report counts, for each frame,
/// how many refreshes after the refresh of its tick it was shown (`lag`;
/// 1 is the design), says why the late ones were late, and lists them.
pub mod trace {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicBool, Ordering},
        },
    };

    use crate::player::clock_ns;

    static ENABLED: AtomicBool = AtomicBool::new(false);

    pub fn enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    /// Turns the trace on; it runs until the process ends.
    pub fn enable() {
        ENABLED.store(true, Ordering::Release);
        gpui_apple::present_trace::enable();
    }

    /// `BLOOM_PACING_TRACE=1` turns the trace on from the start.
    /// `BLOOM_PACING_LOG=<path>` also writes every settled frame to that
    /// file (see [`log_line`]), drained once a second by a thread of its
    /// own, for a long run with no debug command in it.
    pub fn enable_from_env() {
        if std::env::var_os("BLOOM_PACING_TRACE").is_some() {
            enable();
        }
        if let Some(path) = std::env::var_os("BLOOM_PACING_LOG") {
            match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => {
                    let _ = LOG.set(Mutex::new(file));
                    enable();
                    std::thread::Builder::new()
                        .name("pacing-log".into())
                        .spawn(|| {
                            loop {
                                std::thread::sleep(std::time::Duration::from_secs(1));
                                take_settled();
                            }
                        })
                        .expect("spawn pacing-log thread");
                }
                Err(err) => log::warn!("BLOOM_PACING_LOG {}: {err}", path.to_string_lossy()),
            }
        }
    }

    /// The file of `BLOOM_PACING_LOG`, when set.
    static LOG: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

    /// One line of the log: `F` and the times of a frame in nanoseconds of
    /// `clock_ns` (seq, tick index, tick, vsync, swap, asked, done, wake,
    /// adopt, held, draw id, drawable wait, submitted, GPU start, GPU end,
    /// presented, mpv target), `O` and the times of a draw without a frame
    /// (submitted, GPU end, presented, drawable wait), or `T` with the wall
    /// clock in nanoseconds since the epoch and `clock_ns` at that moment.
    fn log_line(line: &str) {
        use std::io::Write;
        if let Some(log) = LOG.get() {
            let mut file = log.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(file, "{line}");
        }
    }

    fn log_on() -> bool {
        LOG.get().is_some()
    }

    /// Takes the frames up to the last one whose draw was presented: the
    /// ones before it that no draw took were skipped for good. The rest
    /// wait for the next call: their draw or its presented handler has
    /// not come yet. (A rule that took a frame with no draw as settled
    /// took the newest frame in the milliseconds before its draw, which
    /// then found no record: a frame "skipped" and a draw with no frame,
    /// once a call.) Writes them to the log when it is on.
    fn take_settled() -> Vec<FrameRec> {
        fill_presented();
        let recs: Vec<FrameRec> = {
            let mut recs = RECS.lock().unwrap_or_else(|e| e.into_inner());
            let settled = recs.iter().rposition(|r| r.presented_ns != 0).map_or(0, |i| i + 1);
            recs.drain(..settled).collect()
        };
        if log_on() {
            let wall = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            log_line(&format!("T {wall} {}", clock_ns()));
            for r in &recs {
                log_line(&format!(
                    "F {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
                    r.seq,
                    r.tick_idx,
                    r.tick_ns,
                    r.vsync_ns,
                    r.swap_ns,
                    r.asked_ns,
                    r.done_ns,
                    r.wake_ns,
                    r.adopt_ns,
                    r.held,
                    r.draw_id,
                    r.acquire_wait_ns,
                    r.submitted_ns,
                    r.gpu_start_ns,
                    r.gpu_end_ns,
                    r.presented_ns,
                    r.target
                ));
            }
        }
        recs
    }

    #[derive(Clone, Default, Debug)]
    pub struct FrameRec {
        pub seq: u64,
        /// The tick the frame was made after: its index, its callback time,
        /// and the refresh it is for.
        pub tick_idx: u64,
        pub tick_ns: u64,
        pub vsync_ns: u64,
        /// When the render thread reported the swap for that tick.
        pub swap_ns: u64,
        /// When mpv asked for the frame, and when the render was done.
        pub asked_ns: u64,
        pub done_ns: u64,
        /// mpv's target time for the frame (its own clock).
        pub target: i64,
        /// When the UI task woke for it, and when a draw took it.
        pub wake_ns: u64,
        pub adopt_ns: u64,
        /// The draw before the one that took it.
        pub prev_draw_ns: u64,
        /// Draws that saw it before it was due.
        pub held: u32,
        pub draw_id: u64,
        /// When the display showed the draw that took it (0: not yet known),
        /// when the draw was committed, how long `nextDrawable` waited for
        /// it, and when the GPU started and finished it.
        pub presented_ns: u64,
        pub submitted_ns: u64,
        pub acquire_wait_ns: u64,
        pub gpu_start_ns: u64,
        pub gpu_end_ns: u64,
    }

    static RECS: Mutex<VecDeque<FrameRec>> = Mutex::new(VecDeque::new());
    const KEEP: usize = 8192;

    /// Draws that took no frame of the player (the home page with its
    /// trailer, a grid, a resize): their commit, GPU end and presented
    /// times and the drawable wait, in `clock_ns`.
    static OTHER_DRAWS: Mutex<Vec<(u64, u64, u64, u64)>> = Mutex::new(Vec::new());

    /// What the main thread ran, by name, with its start and end: to see
    /// what kept it busy when it woke late for a frame.
    static JOBS: Mutex<VecDeque<(&'static str, u64, u64)>> = Mutex::new(VecDeque::new());
    const KEEP_JOBS: usize = 1024;

    #[repr(C)]
    struct TimebaseInfo {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
        fn mach_absolute_time() -> u64;
    }

    /// `host - offset = clock_ns`, where host is in nanoseconds of
    /// `mach_absolute_time` (the domain of `CACurrentMediaTime`).
    fn offsets() -> &'static (i128, f64) {
        static OFFSETS: OnceLock<(i128, f64)> = OnceLock::new();
        OFFSETS.get_or_init(|| {
            let mut info = TimebaseInfo { numer: 1, denom: 1 };
            // SAFETY: a plain query of the clock's scale into a local struct.
            unsafe { mach_timebase_info(&mut info) };
            let scale = info.numer as f64 / info.denom as f64;
            let a = unsafe { mach_absolute_time() };
            let c = clock_ns();
            let host_ns = (a as f64 * scale) as i128;
            (host_ns - c as i128, scale)
        })
    }

    pub fn host_to_clock_ns(host: u64) -> u64 {
        let (offset, scale) = *offsets();
        ((host as f64 * scale) as i128 - offset).max(0) as u64
    }

    pub fn media_to_clock_ns(secs: f64) -> u64 {
        let (offset, _) = *offsets();
        ((secs * 1e9) as i128 - offset).max(0) as u64
    }

    /// The render thread made a frame.
    pub fn rendered(rec: FrameRec) {
        if !enabled() {
            return;
        }
        let mut recs = RECS.lock().unwrap_or_else(|e| e.into_inner());
        if recs.len() >= KEEP {
            recs.pop_front();
        }
        recs.push_back(rec);
    }

    fn with(seq: u64, f: impl FnOnce(&mut FrameRec)) {
        if !enabled() {
            return;
        }
        let mut recs = RECS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rec) = recs.iter_mut().rev().find(|r| r.seq == seq) {
            f(rec);
        }
    }

    /// A draw saw the frame before it was due.
    pub fn held(seq: u64) {
        with(seq, |r| r.held += 1);
    }

    /// A draw took the frame; `draw_id` is the id that draw gets.
    pub fn adopted(seq: u64, wake_ns: u64, adopt_ns: u64, prev_draw_ns: u64, draw_id: u64) {
        with(seq, |r| {
            r.wake_ns = wake_ns;
            r.adopt_ns = adopt_ns;
            r.prev_draw_ns = prev_draw_ns;
            r.draw_id = draw_id;
        });
    }

    /// The main thread ran `name` from `start_ns` to `end_ns`; jobs under
    /// a millisecond are not kept.
    pub fn main_job(name: &'static str, start_ns: u64, end_ns: u64) {
        if !enabled() || end_ns.saturating_sub(start_ns) < 1_000_000 {
            return;
        }
        let mut jobs = JOBS.lock().unwrap_or_else(|e| e.into_inner());
        if jobs.len() >= KEEP_JOBS {
            jobs.pop_front();
        }
        jobs.push_back((name, start_ns, end_ns));
    }

    /// The jobs of the main thread that overlap `from..to`, as text with
    /// times in ms from `base`.
    fn jobs_between(from: u64, to: u64, base: u64) -> String {
        let jobs = JOBS.lock().unwrap_or_else(|e| e.into_inner());
        let found: Vec<String> = jobs
            .iter()
            .filter(|(_, s, e)| *e > from && *s < to)
            .map(|(n, s, e)| format!("{n} {:+.1}..{:+.1}", ms(*s, base), ms(*e, base)))
            .collect();
        if found.is_empty() { "none of ours".into() } else { found.join(", ") }
    }

    /// Draws handed over without one of their two completions (see
    /// `present_trace::take_done`), since the last report.
    static INCOMPLETE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Fills in the presented and GPU times known so far.
    fn fill_presented() {
        let shown = gpui_apple::present_trace::take_done();
        if shown.is_empty() {
            return;
        }
        let mut recs = RECS.lock().unwrap_or_else(|e| e.into_inner());
        let mut others = OTHER_DRAWS.lock().unwrap_or_else(|e| e.into_inner());
        for (id, draw) in shown {
            if !draw.complete {
                INCOMPLETE.fetch_add(1, Ordering::Relaxed);
            }
            let mut taken = false;
            for rec in recs.iter_mut().filter(|r| r.draw_id == id) {
                rec.presented_ns = media_to_clock_ns(draw.presented);
                rec.submitted_ns = media_to_clock_ns(draw.submitted);
                rec.acquire_wait_ns = (draw.acquire_wait * 1e9) as u64;
                rec.gpu_start_ns = media_to_clock_ns(draw.gpu_start);
                rec.gpu_end_ns = media_to_clock_ns(draw.gpu_end);
                taken = true;
            }
            if !taken {
                let other = (
                    media_to_clock_ns(draw.submitted),
                    media_to_clock_ns(draw.gpu_end),
                    media_to_clock_ns(draw.presented),
                    (draw.acquire_wait * 1e9) as u64,
                );
                if log_on() {
                    log_line(&format!("O {} {} {} {}", other.0, other.1, other.2, other.3));
                }
                if others.len() < KEEP {
                    others.push(other);
                }
            }
        }
    }

    /// The draws since the last call that took no frame of the player:
    /// how many refreshes after their commit the display showed them
    /// (with gpui's step at the tick, 1 is on time and 2 a refresh late),
    /// their GPU time, and how long they waited for a drawable.
    pub fn other_draws_report(period_ns: u64) -> String {
        fill_presented();
        let draws = std::mem::take(&mut *OTHER_DRAWS.lock().unwrap_or_else(|e| e.into_inner()));
        if draws.is_empty() {
            return "other draws: none".into();
        }
        let period = period_ns.max(1) as f64;
        let mut by_refreshes = [0u32; 5]; // 0, 1, 2, 3, 4+
        let mut acquire = [0u32; 8];
        let mut gpu: Vec<f64> = Vec::new();
        let mut wait: Vec<f64> = Vec::new();
        for (submitted, gpu_end, presented, acquire_wait) in &draws {
            let k = ((presented.saturating_sub(*submitted) as f64 / period) - 0.3).round().clamp(0., 4.) as usize;
            by_refreshes[k] += 1;
            acquire[(acquire_wait / 1_000_000).min(7) as usize] += 1;
            gpu.push(gpu_end.saturating_sub(*submitted) as f64 / 1e6);
            wait.push(*acquire_wait as f64 / 1e6);
        }
        gpu.sort_by(|a, b| a.total_cmp(b));
        wait.sort_by(|a, b| a.total_cmp(b));
        let at = |list: &[f64], q: f64| list[((list.len() - 1) as f64 * q) as usize];
        format!(
            "other draws: {} shown by refreshes after commit [0,1,2,3,4+]={:?} | commit to GPU end ms p50={:.1} p95={:.1} max={:.1} | drawable wait by ms {:?} p95={:.2} max={:.2}",
            draws.len(),
            by_refreshes,
            at(&gpu, 0.5),
            at(&gpu, 0.95),
            at(&gpu, 1.),
            acquire,
            at(&wait, 0.95),
            at(&wait, 1.),
        )
    }

    fn ms(ns: u64, from: u64) -> f64 {
        (ns as f64 - from as f64) / 1e6
    }

    /// The frames since the last call, classified. `detail` lists the odd
    /// ones with their times (ms, relative to the tick of the frame).
    pub fn report(period_ns: u64, detail: usize) -> String {
        if !enabled() {
            return "trace: off (on from the next window)".into();
        }
        let period = period_ns.max(1) as f64;
        let recs = take_settled();
        if recs.is_empty() {
            return "trace: no frames".into();
        }
        let n = recs.len();
        // Where each frame was shown against the refresh of its tick: 1.0
        // is the design (the next refresh).
        let mut lag_bins = [0u32; 6]; // <=0, 1, 2, 3, 4, 5+
        let mut lag_sum = 0.;
        let mut skipped = 0; // rendered, never drawn
        let mut cadence = [0u32; 7]; // tick deltas 0..=6
        let mut late_from = [0u32; 5]; // render, wake, draw, held, gpu
        let mut acquire = [0u32; 8]; // nextDrawable wait by ms, 7+
        let mut gt_gaps: Vec<f64> = Vec::new();
        let mut lines = Vec::new();
        let mut last_presented = 0u64;
        for (i, r) in recs.iter().enumerate() {
            if i > 0 {
                let d = r.tick_idx.saturating_sub(recs[i - 1].tick_idx).min(6) as usize;
                cadence[d] += 1;
            }
            if r.adopt_ns == 0 {
                skipped += 1;
                continue;
            }
            if r.presented_ns == 0 {
                continue;
            }
            acquire[(r.acquire_wait_ns / 1_000_000).min(7) as usize] += 1;
            if last_presented != 0 {
                gt_gaps.push(ms(r.presented_ns, last_presented));
            }
            last_presented = r.presented_ns;
            let lag = (r.presented_ns as f64 - r.vsync_ns as f64) / period;
            lag_sum += lag;
            let bin = lag.round().clamp(0., 5.) as usize;
            lag_bins[bin] += 1;
            let sample = bin == 1 && i % 97 == 0;
            if bin != 1 || sample {
                // Why: the render was not done at the time of the draw (the
                // phase after the tick, or the next tick with the timing
                // before), the UI woke after it, the frame was held, the
                // draw came late, or none of these (the GPU took long).
                let (draw_at, slack) = match super::frame_phase(period_ns) {
                    Some(phase) => (r.tick_ns + phase, period_ns / 4),
                    None => (r.tick_ns + period_ns, period_ns / 2),
                };
                let cause = if r.done_ns > draw_at {
                    0
                } else if r.wake_ns > draw_at {
                    1
                } else if r.held > 0 {
                    3
                } else if r.adopt_ns > draw_at + slack {
                    2
                } else {
                    4
                };
                if !sample {
                    late_from[cause] += 1;
                }
                if lines.len() < detail {
                    let mut line = format!(
                        "{}seq={} tick={} dtick={} lag={lag:.2} vsync={:+.1} swap={:+.1} asked={:+.1} done={:+.1} wake={:+.1} adopt={:+.1} prev_draw={:+.1} held={} acq={:.1} sub={:+.1} gpu={:+.1}..{:+.1} shown={:+.1} target_d={:.1}",
                        if sample { "ok " } else { "LATE " },
                        r.seq,
                        r.tick_idx,
                        if i > 0 { r.tick_idx as i64 - recs[i - 1].tick_idx as i64 } else { 0 },
                        ms(r.vsync_ns, r.tick_ns),
                        ms(r.swap_ns, r.tick_ns),
                        ms(r.asked_ns, r.tick_ns),
                        ms(r.done_ns, r.tick_ns),
                        ms(r.wake_ns, r.tick_ns),
                        ms(r.adopt_ns, r.tick_ns),
                        ms(r.prev_draw_ns, r.tick_ns),
                        r.held,
                        r.acquire_wait_ns as f64 / 1e6,
                        ms(r.submitted_ns, r.tick_ns),
                        ms(r.gpu_start_ns, r.tick_ns),
                        ms(r.gpu_end_ns, r.tick_ns),
                        ms(r.presented_ns, r.tick_ns),
                        if i > 0 { (r.target - recs[i - 1].target) as f64 / 1e6 } else { 0. },
                    );
                    // A wake more than 8 ms after the render: what the main
                    // thread ran meanwhile.
                    if !sample && r.wake_ns > r.done_ns + 8_000_000 {
                        line.push_str(" main: ");
                        line.push_str(&jobs_between(r.done_ns, r.wake_ns, r.tick_ns));
                    }
                    lines.push(line);
                }
            }
        }
        let shown = lag_bins.iter().sum::<u32>().max(1) as f64;
        let mut by_refreshes = [0usize; 7];
        for g in &gt_gaps {
            let k = (g / (period / 1e6)).round().clamp(1., 6.) as usize;
            by_refreshes[k] += 1;
        }
        let gt_jumps = gt_gaps
            .windows(2)
            .filter(|p| ((p[0] - p[1]).abs() / (period / 1e6)).round() >= 2.)
            .count();
        let mut out = format!(
            "trace: frames={n} shown={} skipped={skipped} incomplete={} lag mean={:.3} bins[<=0,1,2,3,4,5+]={:?} late_from[render,wake,draw,held,gpu]={:?} | cadence dtick[0..6]={:?} | presented refreshes 1:{} 2:{} 3:{} 4:{} 5:{} 6+:{} gt_jumps={gt_jumps} | drawable wait by ms {:?}",
            shown as u32,
            INCOMPLETE.swap(0, Ordering::Relaxed),
            lag_sum / shown,
            lag_bins,
            late_from,
            cadence,
            by_refreshes[1],
            by_refreshes[2],
            by_refreshes[3],
            by_refreshes[4],
            by_refreshes[5],
            by_refreshes[6],
            acquire,
        );
        for line in lines {
            out.push_str("\n    ");
            out.push_str(&line);
        }
        out
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
    fn a_frame_is_shown_at_the_first_draw_due_after_its_tick() {
        let ms = |n: u64| n * 1_000_000;
        let period = ms(16);
        // A draw of the tick of the frame, before or after the frame.
        assert!(!due(ms(100), ms(100), period));
        assert!(!due(ms(100), ms(105), period));
        // The draw the frames task asks for, 0.55 of the period after the
        // tick (8.8 ms), and the draw of the next tick, early or late.
        assert!(due(ms(100), ms(109), period));
        assert!(due(ms(100), ms(116), period));
        assert!(due(ms(100), ms(122), period));
        assert!(!late(ms(100), ms(122), period));
        // The draw after that: the frame missed one.
        assert!(late(ms(100), ms(133), period));
        // A frame with audio timing has no tick.
        assert!(due(0, ms(1), period));
        assert!(!late(0, ms(500), period));
    }

    /// The timing model, from the measurements at 60 Hz (verify-pacing,
    /// 2026-10-05). Times in ms after the tick T of a frame:
    /// - mpv's render is done at T + 3.0 to 3.8 (p95).
    /// - gpui's own step runs at T + 0 to 2 (and at T + period + 0 to 2).
    /// - A draw's GPU work starts 0.3 to 0.7 ms after the commit and takes
    ///   2.2 to 4.8 ms on the player page.
    /// - The compositor shows a draw at the refresh after the first
    ///   deadline its GPU work meets; the deadline for the refresh of
    ///   tick T+1 (the design: `lag` 1) was at T + 22.0 to 22.4, that is
    ///   one period plus 0.33 of a period, and the deadline for the
    ///   refresh of tick T is at T + 0.33 period (5.5 ms).
    /// Whether the 0.33 is a fraction of the period or a fixed 5.5 ms on a
    /// faster display is not known: this Mac has a 60 Hz display, and 120
    /// Hz is reasoned here, not measured. The test checks both models.
    #[test]
    fn the_phase_leaves_the_gpu_time_at_every_refresh_rate() {
        const RENDER_DONE_P95: f64 = 3.8;
        const STEP_LATE: f64 = 2.0;
        const SUBMIT_TO_GPU: f64 = 0.7;
        const GPU_MIN: f64 = 2.2;
        const GPU_P95: f64 = 4.8;
        const DEADLINE_OF_PERIOD: f64 = 0.33;
        const DEADLINE_FIXED_MS: f64 = 5.5;
        // (Hz, refreshes a 24 fps frame lasts: 2 and 3 in turn at 60 Hz,
        // 2 and 3 at 50 (25 fps: 2), 4 and 5 at 100, 5 at 120, 6 at 144.)
        for (hz, frame_refreshes) in [(50., 2.08), (60., 2.5), (100., 4.17), (120., 5.), (144., 6.)] {
            let period = 1000. / hz;
            let period_ns = (period * 1e6) as u64;
            let phase = frame_phase(period_ns).expect("a phase by default") as f64 / 1e6;
            let due = due_after(period_ns) as f64 / 1e6;
            assert!((phase - period * PHASE_OF_PERIOD).abs() < 0.01, "{hz} Hz: phase {phase}");
            // The draw comes after the frame is due, and a draw of the
            // tick itself that runs late does not take the frame.
            assert!(due < phase, "{hz} Hz: due {due} before the draw at {phase}");
            assert!(due > STEP_LATE, "{hz} Hz: a late step at {STEP_LATE} must not take the frame (due {due})");
            // The draw's GPU work ends after the deadline of the refresh of
            // the tick itself, so the frame is never shown a refresh early
            // (it would be ahead of the sound): draw at the phase, or at
            // the earliest at `due`.
            let earliest_gpu_end = due + SUBMIT_TO_GPU + GPU_MIN;
            for (model, first_deadline) in [("fraction", period * DEADLINE_OF_PERIOD), ("fixed", DEADLINE_FIXED_MS)] {
                assert!(
                    earliest_gpu_end > first_deadline,
                    "{hz} Hz ({model}): a draw at {due} could be shown a refresh early (GPU end {earliest_gpu_end}, deadline {first_deadline})"
                );
                // The GPU budget: from the commit at the phase (or once the
                // render is done, when that is later) to the deadline of
                // the design refresh. The old timing's budget is from the
                // step at the next tick.
                let draw_at = phase.max(RENDER_DONE_P95);
                let deadline = period + first_deadline;
                let budget = deadline - draw_at - SUBMIT_TO_GPU;
                let old_budget = deadline - (period + STEP_LATE) - SUBMIT_TO_GPU;
                eprintln!(
                    "{hz} Hz ({model}): period {period:.2} ms, a 24 fps frame lasts {frame_refreshes} refreshes, draw at +{draw_at:.1}, deadline +{deadline:.1}: GPU budget {budget:.1} ms (old timing {old_budget:.1})"
                );
                assert!(budget > old_budget, "{hz} Hz ({model}): no gain over the old timing");
                // A typical draw lands on the intended refresh at every
                // rate; the slow ones (p95) at 120 Hz and below in both
                // models, and at 144 Hz with the fixed deadline.
                assert!(budget >= GPU_MIN + 1., "{hz} Hz ({model}): budget {budget:.1} too small for a typical draw");
                if hz <= 120. || model == "fixed" {
                    assert!(budget >= GPU_P95, "{hz} Hz ({model}): budget {budget:.1} under the p95 draw");
                }
            }
        }
    }

    #[test]
    fn the_phase_follows_the_period_of_the_display() {
        let at = |hz: f64| frame_phase((1e9 / hz) as u64).unwrap() as f64 / 1e6;
        assert!((at(60.) - 9.17).abs() < 0.01);
        assert!((at(120.) - 4.58).abs() < 0.01);
        assert!((at(50.) - 11.0).abs() < 0.01);
        assert!((at(144.) - 3.82).abs() < 0.01);
        // The due margin shrinks with the period: 2 ms at 60 and 50 Hz,
        // an eighth of the period above.
        let due = |hz: f64| due_after((1e9 / hz) as u64) as f64 / 1e6;
        assert!((due(60.) - 7.17).abs() < 0.01);
        assert!((due(50.) - 9.0).abs() < 0.01);
        assert!((due(120.) - (4.58 - 1.04)).abs() < 0.02);
        assert!((due(144.) - (3.82 - 0.87)).abs() < 0.02);
    }

    /// A display for the phase loop: refresh period, and where the
    /// compositor's deadline for the refresh of a tick is, after the tick
    /// (5.5 ms at 60 Hz here: the GPU work of a draw must end by then to
    /// come out at that refresh, by the one after it for the next). The
    /// link's `outputTime` of a tick is one period after that deadline
    /// plus 0.8 ms (23.0 ms lead at 60 Hz, as measured).
    struct Sim {
        period: f64,
        deadline_first: f64,
        /// The lead changes over the run (a step of a millisecond, as the
        /// link's did), or the deadline comes a period earlier for a
        /// stretch (a span).
        lead_step_at: Option<usize>,
        span: Option<std::ops::Range<usize>>,
        rand: u64,
    }

    #[derive(Default, Debug)]
    struct SimResult {
        late: usize,
        early: usize,
        /// Late frames no phase could have saved: GPU work that would have
        /// missed from the earliest draw (the floor) too.
        unavoidable: usize,
        /// Late frames of a 14 ms draw: not evidence of the deadline, the
        /// loop leaves them.
        spikes: usize,
        /// The last frame at which the phase moved (the loop converged
        /// there), and the phase used at the end.
        last_change_frame: usize,
        final_phase_ms: f64,
        steps: (u32, u32),
    }

    impl Sim {
        fn new(hz: f64, deadline_first: f64) -> Self {
            Self { period: 1000. / hz, deadline_first, lead_step_at: None, span: None, rand: 0x2545_F491_4F6C_DD1D }
        }

        fn rand(&mut self) -> f64 {
            // xorshift
            self.rand ^= self.rand << 13;
            self.rand ^= self.rand >> 7;
            self.rand ^= self.rand << 17;
            (self.rand % 10_000) as f64 / 10_000.
        }

        fn gpu_ms(&mut self, k: usize) -> f64 {
            // 2.2 typical, 4.8 at p95, 14 one frame in 300.
            if k % 300 == 150 {
                14.0
            } else if self.rand() < 0.05 {
                4.8
            } else {
                2.2 + self.rand() * 0.8
            }
        }

        /// Runs `frames` frames of 24 fps video; with `with_loop` the
        /// phase follows the loop, else the model's phase.
        fn run(&mut self, frames: usize, with_loop: bool) -> SimResult {
            let p = self.period;
            let period_ns = (p * 1e6) as u64;
            let mut l = phase::PhaseLoop::new(period_ns);
            let model = PhaseLoop_default(period_ns) as f64 / 1e6;
            let mut r = SimResult::default();
            let mut pending: Vec<phase::Observation> = Vec::new();
            let mut tick = 0.;
            let mut phase_seen = l.phase();
            for k in 0..frames {
                // The observation of the frame before this one comes
                // in (its draw was presented two refreshes on).
                for o in pending.drain(..) {
                    if with_loop {
                        l.observe(o);
                    }
                }
                let phase = if with_loop { l.phase() as f64 / 1e6 } else { model };
                if l.phase() != phase_seen {
                    r.last_change_frame = k;
                }
                phase_seen = l.phase();
                // A step in the lead: the link's callback a millisecond
                // earlier against the real refresh, the deadline where
                // it was.
                let mut deadline_first = self.deadline_first;
                if self.lead_step_at.is_some_and(|at| k >= at) {
                    deadline_first += 1.0;
                }
                let lead = p + deadline_first + 0.8;
                let vsync = tick + lead;
                if self.span.as_ref().is_some_and(|s| s.contains(&k)) {
                    deadline_first -= p;
                }
                let done = tick + 3.0 + self.rand() * 0.8;
                let jitter = self.rand() * 0.3;
                let draw = (tick + phase).max(done) + jitter;
                let gpu = self.gpu_ms(k);
                let gpu_end = draw + 0.5 + gpu;
                // Shown at the first refresh whose deadline the GPU end
                // met: the deadline of the refresh of the tick is at
                // tick + deadline_first, the next one a period later.
                let lag = ((gpu_end - (tick + deadline_first)) / p).ceil() as i32;
                let floor = phase::PhaseLoop::floor(period_ns) as f64 / 1e6;
                let earliest_end = (tick + floor).max(done) + jitter + 0.5 + gpu;
                let lag_at_earliest = ((earliest_end - (tick + deadline_first)) / p).ceil() as i32;
                if lag >= 2 {
                    r.late += 1;
                    if lag_at_earliest >= 2 {
                        r.unavoidable += 1;
                    } else if gpu > 10. {
                        r.spikes += 1;
                    }
                } else if lag <= 0 {
                    r.early += 1;
                }
                pending.push(phase::Observation {
                    seq: k as u64,
                    lag,
                    gpu_end_rel_vsync: ((gpu_end - vsync) * 1e6) as i64,
                    clean: gpu <= phase::GPU_USUAL as f64 / 1e6,
                });
                tick += p * if k % 2 == 0 { 2. } else { 3. };
            }
            r.final_phase_ms = if with_loop { l.phase() as f64 / 1e6 } else { model };
            r.steps = (l.steps_earlier, l.steps_later);
            r
        }
    }

    #[allow(non_snake_case)]
    fn PhaseLoop_default(period_ns: u64) -> u64 {
        (period_ns as f64 * PHASE_OF_PERIOD) as u64
    }

    #[test]
    fn the_phase_loop_holds_lag_one_on_every_display_the_model_fits() {
        // The two models of the deadline on other displays: a third of
        // the period, or 5.5 ms as at 60 Hz.
        for hz in [50., 60., 100., 120., 144.] {
            let p = 1000. / hz;
            for (model, first) in [("fraction", p * 0.33), ("fixed", 5.5)] {
                let r = Sim::new(hz, first).run(2400, true);
                eprintln!("{hz} Hz ({model}): {r:?}");
                assert_eq!(r.early, 0, "{hz} Hz ({model}): frames a refresh early");
                assert!(
                    r.late <= r.unavoidable + r.spikes + 6,
                    "{hz} Hz ({model}): late {} unavoidable {} spikes {}",
                    r.late,
                    r.unavoidable,
                    r.spikes
                );
                // No step back and forth: the phase only moves earlier,
                // one late frame at a time (at 144 Hz the model misses
                // one slow draw in twenty, and the loop steps as it sees
                // them).
                assert_eq!(r.steps.1, 0, "{hz} Hz ({model}): stepped later");
            }
        }
    }

    #[test]
    fn the_phase_loop_holds_lag_one_where_a_fixed_phase_does_not() {
        // A 120 Hz display whose deadline comes 1.3 ms after the tick:
        // the draw at 0.55 of the period misses it one time in twenty.
        let mut sim = Sim::new(120., 1.3);
        let fixed = sim.run(2400, false);
        let mut sim = Sim::new(120., 1.3);
        let looped = sim.run(2400, true);
        eprintln!("120 Hz early deadline: fixed {fixed:?}\n  loop {looped:?}");
        assert!(fixed.late > 40, "the fixed phase should miss: {}", fixed.late);
        assert!(looped.late <= looped.unavoidable + looped.spikes + 6, "{looped:?}");
        assert!(looped.final_phase_ms < fixed.final_phase_ms);
        assert!(looped.steps.0 >= 1 && looped.steps.1 == 0);

        // A 60 Hz display whose deadline comes 13 ms after the tick (a
        // link that ticks early): the draw at 0.55 lands a refresh early
        // every frame; the loop moves it later until it does not.
        let mut sim = Sim::new(60., 13.0);
        let fixed = sim.run(2400, false);
        let mut sim = Sim::new(60., 13.0);
        let looped = sim.run(2400, true);
        eprintln!("60 Hz late deadline: fixed {fixed:?}\n  loop {looped:?}");
        assert!(fixed.early > 1000, "{fixed:?}");
        // Converged within the first seconds: the first step at the
        // first early frame, a second one when a fast draw still lands
        // early; then it stays.
        assert!(looped.early < 24, "{looped:?}");
        assert!(looped.last_change_frame < 240, "{looped:?}");
        assert!(looped.steps.0 == 0, "{looped:?}");
        assert!(looped.final_phase_ms > fixed.final_phase_ms);
    }

    #[test]
    fn the_phase_loop_ignores_slow_draws_and_a_drifting_link() {
        // A step of a millisecond in the link's lead, as seen in the
        // traces: nothing to correct.
        let mut sim = Sim::new(60., 5.5);
        sim.lead_step_at = Some(800);
        let r = sim.run(2400, true);
        eprintln!("60 Hz lead step: {r:?}");
        assert_eq!(r.steps, (0, 0), "{r:?}");
        assert_eq!(r.late, r.unavoidable + r.spikes, "{r:?}");
        assert_eq!(r.early, 0);
        // A span: the compositor wants the frame a period earlier for 72
        // frames (3 s). No phase reaches that; the loop goes to the floor
        // and stops there, with no step back and forth after.
        let mut sim = Sim::new(60., 5.5);
        sim.span = Some(1000..1072);
        let r = sim.run(2400, true);
        eprintln!("60 Hz span: {r:?}");
        let floor = phase::PhaseLoop::floor((1e9 / 60.) as u64) as f64 / 1e6;
        assert!((r.final_phase_ms - floor).abs() < 0.01, "{r:?}");
        assert_eq!(r.early, 0);
        assert!(r.late >= 72 && r.late <= r.unavoidable + r.spikes + 2, "{r:?}");
        assert!(r.steps.0 <= 5, "a step per settle time, then the floor: {r:?}");
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
