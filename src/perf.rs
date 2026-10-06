// SPDX-License-Identifier: AGPL-3.0-or-later
//! Render cost counters. `Bloom::render` reports each window render here;
//! the debug channel's `perf` command prints the totals since its last call.
//!
//! Three numbers matter:
//! - build: time to build the element tree in `render`,
//! - main: CPU time of the UI thread for each render, which adds GPUI's
//!   layout and paint to the build time,
//! - process: CPU of all threads (decode, network, mpv).

use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// When `main` started; the startup log lines count from here.
static STARTED: OnceLock<Instant> = OnceLock::new();

static RENDERS: AtomicU64 = AtomicU64::new(0);
static BUILD_NS: AtomicU64 = AtomicU64::new(0);
static BUILD_MAX_NS: AtomicU64 = AtomicU64::new(0);
/// Time from the start of `render` to the paint of the last element: build,
/// layout and paint of one frame.
static DRAW_NS: AtomicU64 = AtomicU64::new(0);
static DRAW_MAX_NS: AtomicU64 = AtomicU64::new(0);
static DRAWS: AtomicU64 = AtomicU64::new(0);
/// CPU time of the UI thread for the same span. Unlike `DRAW_NS` it does not
/// grow when other programs load the machine, so limits are checked on it.
static DRAW_CPU_NS: AtomicU64 = AtomicU64::new(0);
static DRAW_CPU_MAX_NS: AtomicU64 = AtomicU64::new(0);
/// CPU time of the UI thread at its latest render.
static MAIN_CPU_NS: AtomicU64 = AtomicU64::new(0);
/// Named counters, such as cards built.
static CARDS: AtomicU64 = AtomicU64::new(0);

/// The threads of the players, for a count of their rounds: an idle thread
/// must not spin (`dev/jctl perf` prints the rounds a second).
#[derive(Clone, Copy)]
pub enum Loop {
    Worker,
    Render,
    Link,
    Trailer,
}
static LOOPS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Counts one round of the loop of a thread.
pub fn count_loop(which: Loop) {
    LOOPS[which as usize].fetch_add(1, Ordering::Relaxed);
}

struct Mark {
    at: Instant,
    main_cpu_ns: u64,
    process_cpu_ns: u64,
}

static LAST: Mutex<Option<Mark>> = Mutex::new(None);

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
}

// Clock ids of macOS.
const CLOCK_PROCESS_CPUTIME_ID: i32 = 12;
const CLOCK_THREAD_CPUTIME_ID: i32 = 16;

fn cpu_ns(clock: i32) -> u64 {
    let mut time = Timespec { sec: 0, nsec: 0 };
    unsafe { clock_gettime(clock, &mut time) };
    time.sec as u64 * 1_000_000_000 + time.nsec as u64
}

/// Marks the start of the process. Call it first thing in `main`.
pub fn mark_start() {
    let _ = STARTED.set(Instant::now());
}

/// Milliseconds since `mark_start`.
pub fn since_start_ms() -> u128 {
    STARTED.get().map_or(0, |t| t.elapsed().as_millis())
}

/// Records one window render. Call it on the UI thread.
pub fn record_render(build: Duration) {
    if RENDERS.fetch_add(1, Ordering::Relaxed) == 0 && LAST.lock().unwrap().is_none() {
        log::info!("first frame {} ms after start", since_start_ms());
    }
    BUILD_NS.fetch_add(build.as_nanos() as u64, Ordering::Relaxed);
    BUILD_MAX_NS.fetch_max(build.as_nanos() as u64, Ordering::Relaxed);
    MAIN_CPU_NS.store(cpu_ns(CLOCK_THREAD_CPUTIME_ID), Ordering::Relaxed);
}

/// CPU time the calling thread has used. Take it at the start of a render
/// and give it to `record_draw`.
pub fn thread_cpu_ns() -> u64 {
    cpu_ns(CLOCK_THREAD_CPUTIME_ID)
}

/// Records the end of a frame's paint; `started` is when its render began
/// and `started_cpu` is `thread_cpu_ns` of that moment.
pub fn record_draw(started: Instant, started_cpu: u64) {
    let draw = started.elapsed().as_nanos() as u64;
    let cpu = thread_cpu_ns().saturating_sub(started_cpu);
    // For the pacing trace: what kept the main thread busy.
    let now = crate::player::clock_ns();
    crate::pacing::trace::main_job("draw", now.saturating_sub(draw), now);
    DRAW_CPU_NS.fetch_add(cpu, Ordering::Relaxed);
    DRAW_CPU_MAX_NS.fetch_max(cpu, Ordering::Relaxed);
    DRAWS.fetch_add(1, Ordering::Relaxed);
    DRAW_NS.fetch_add(draw, Ordering::Relaxed);
    DRAW_MAX_NS.fetch_max(draw, Ordering::Relaxed);
}

/// Times between the draw requests for the video frames, in microseconds;
/// the last 4000. A draw request is not the frame on the glass: the
/// `jumps` of `pacing_report` are a secondary figure, the ruler is
/// `pacing::trace` (the time the display showed each frame).
static VIDEO_GAPS: Mutex<(Option<Instant>, Vec<u32>)> = Mutex::new((None, Vec::new()));

/// Waits from "mpv has a frame" to "the UI is told to draw it", in
/// microseconds; the last 4000. This is measured also when the window is
/// hidden and draws nothing.
static VIDEO_WAITS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Times from "mpv has a frame" to "the frame is rendered", in
/// microseconds; the last 4000.
static VIDEO_RENDERS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Records the render time of one frame, on the thread of the player.
pub fn video_frame_rendered(ms: f64) {
    let mut list = VIDEO_RENDERS.lock().unwrap();
    if list.len() >= 4000 {
        list.remove(0);
    }
    list.push((ms * 1000.).clamp(0., 4e9) as u32);
}

fn quantiles(name: &str, list: &Mutex<Vec<u32>>) -> String {
    let mut list = std::mem::take(&mut *list.lock().unwrap());
    if list.is_empty() {
        return format!("{name}: no frames");
    }
    list.sort_unstable();
    let at = |q: f64| list[((list.len() - 1) as f64 * q) as usize] as f64 / 1000.;
    let mean = list.iter().map(|us| *us as f64).sum::<f64>() / list.len() as f64 / 1000.;
    format!(
        "{name}: frames={} mean={mean:.2}ms p5={:.2} p50={:.2} p95={:.2} max={:.2}",
        list.len(),
        at(0.05),
        at(0.5),
        at(0.95),
        at(1.0),
    )
}

/// Records the wait of one frame.
pub fn video_frame_noticed(wait_ms: f64) {
    let mut waits = VIDEO_WAITS.lock().unwrap();
    if waits.len() >= 4000 {
        waits.remove(0);
    }
    waits.push((wait_ms * 1000.).clamp(0., 4e9) as u32);
}

fn waits_report() -> String {
    format!(
        "{} | {}",
        quantiles("ask to rendered", &VIDEO_RENDERS),
        quantiles("rendered to draw request", &VIDEO_WAITS)
    )
}

/// Records that a render asked to draw a new video frame (the time of the
/// request on the main thread, not of the picture on the glass).
pub fn video_frame_shown() {
    let now = Instant::now();
    let mut gaps = VIDEO_GAPS.lock().unwrap();
    if let Some(last) = gaps.0.replace(now) {
        let gap = now.duration_since(last).as_micros().min(u32::MAX as u128) as u32;
        if gaps.1.len() >= 4000 {
            gaps.1.remove(0);
        }
        gaps.1.push(gap);
    }
}

/// How even the draw requests for the video frames came since the last
/// call: the gaps counted by how many refreshes of the display (its period
/// in nanoseconds) each one took. A 24 fps video on a 60 Hz display is even
/// when the gaps are 2 and 3 refreshes, in turn. This times the request on
/// the main thread, not the picture on the glass: a draw that the GPU
/// finishes after the compositor's deadline is shown a refresh late with
/// an even request gap. `pacing::trace` measures that; `jumps` stays as a
/// secondary figure.
pub fn pacing_report(period_ns: u64) -> String {
    let mut gaps = VIDEO_GAPS.lock().unwrap();
    let list = std::mem::take(&mut gaps.1);
    gaps.0 = None;
    // A pause or a seek is not a matter of pacing.
    let list: Vec<f64> = list.into_iter().map(|us| us as f64 / 1000.).filter(|ms| *ms < 500.).collect();
    if list.is_empty() {
        return format!("pacing: no video frames drawn (hidden window?) | {} | {}", waits_report(), crate::pacing::report());
    }
    let refresh_ms = if period_ns == 0 { 1000. / 60. } else { period_ns as f64 / 1e6 };
    let mut by_refreshes = [0usize; 7];
    for ms in &list {
        let n = (ms / refresh_ms).round().clamp(1., 6.) as usize;
        by_refreshes[n] += 1;
    }
    let mean = list.iter().sum::<f64>() / list.len() as f64;
    // A change of the request gap by two refreshes or more from one frame
    // to the next.
    let jumps = list
        .windows(2)
        .filter(|pair| ((pair[0] - pair[1]).abs() / refresh_ms).round() >= 2.)
        .count();
    let max = list.iter().cloned().fold(0., f64::max);
    format!(
        "pacing (draw requests): frames={} mean={mean:.1}ms max={max:.0}ms jumps={jumps} ({:.1}%) | refreshes 1:{} 2:{} 3:{} 4:{} 5:{} 6+:{} | {}",
        list.len() + 1,
        jumps as f64 * 100. / list.len() as f64,
        by_refreshes[1],
        by_refreshes[2],
        by_refreshes[3],
        by_refreshes[4],
        by_refreshes[5],
        by_refreshes[6],
        format!("{} | {}", waits_report(), crate::pacing::report()),
    )
}

/// Counts one card built for a render.
pub fn count_card() {
    CARDS.fetch_add(1, Ordering::Relaxed);
}

/// Totals since the last call, as one line. Call it on the UI thread.
pub fn report() -> String {
    let now = Mark {
        at: Instant::now(),
        main_cpu_ns: cpu_ns(CLOCK_THREAD_CPUTIME_ID),
        process_cpu_ns: cpu_ns(CLOCK_PROCESS_CPUTIME_ID),
    };
    let renders = RENDERS.swap(0, Ordering::Relaxed);
    let build_ns = BUILD_NS.swap(0, Ordering::Relaxed);
    let build_max_ns = BUILD_MAX_NS.swap(0, Ordering::Relaxed);
    let draws = DRAWS.swap(0, Ordering::Relaxed).max(1);
    let draw_ns = DRAW_NS.swap(0, Ordering::Relaxed);
    let draw_max_ns = DRAW_MAX_NS.swap(0, Ordering::Relaxed);
    let draw_cpu_ns = DRAW_CPU_NS.swap(0, Ordering::Relaxed);
    let draw_cpu_max_ns = DRAW_CPU_MAX_NS.swap(0, Ordering::Relaxed);
    let cards = CARDS.swap(0, Ordering::Relaxed);
    let loops: Vec<u64> = LOOPS.iter().map(|l| l.swap(0, Ordering::Relaxed)).collect();
    let previous = LAST.lock().unwrap().replace(Mark {
        at: now.at,
        main_cpu_ns: now.main_cpu_ns,
        process_cpu_ns: now.process_cpu_ns,
    });
    let Some(previous) = previous else {
        return "perf: counters started".to_string();
    };
    let seconds = now.at.duration_since(previous.at).as_secs_f64().max(0.001);
    let main_ms = (now.main_cpu_ns - previous.main_cpu_ns) as f64 / 1e6;
    let process_ms = (now.process_cpu_ns - previous.process_cpu_ns) as f64 / 1e6;
    let per = |total: f64| if renders == 0 { 0. } else { total / renders as f64 };
    format!(
        "perf: {seconds:.1}s renders={renders} ({:.1}/s) build={:.2}ms/render (max {:.2}) \
         draw={:.2}ms/frame (max {:.2}) frame_cpu={:.2}ms/frame (max {:.2}) main={:.2}ms/render cards={:.0}/render \
         main_cpu={:.1}% process_cpu={:.1}% loops/s worker={:.1} render={:.1} link={:.1} trailer={:.1}",
        renders as f64 / seconds,
        per(build_ns as f64 / 1e6),
        build_max_ns as f64 / 1e6,
        draw_ns as f64 / 1e6 / draws as f64,
        draw_max_ns as f64 / 1e6,
        draw_cpu_ns as f64 / 1e6 / draws as f64,
        draw_cpu_max_ns as f64 / 1e6,
        per(main_ms),
        per(cards as f64),
        main_ms / seconds / 10.,
        process_ms / seconds / 10.,
        loops[0] as f64 / seconds,
        loops[1] as f64 / seconds,
        loops[2] as f64 / seconds,
        loops[3] as f64 / seconds,
    )
}
