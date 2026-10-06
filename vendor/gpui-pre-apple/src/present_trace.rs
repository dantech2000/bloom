//! Bloom (vendor/README.md): when the display showed each draw, for frame
//! pacing measurements (`dev/jctl pacing`). Off until [`enable`]: before
//! that `draw` records nothing and allocates nothing. The id the next draw
//! gets is `DRAW_ID + 1`. Times are seconds of `CACurrentMediaTime` (the
//! domain of `presentedTime` and `GPUEndTime`).
//!
//! A draw has two completions that come in either order on their own
//! threads: the command buffer's completed handler (GPU times) and the
//! drawable's presented handler (the time on the glass). A record is handed
//! out ([`take_done`]) once both are in, or, after [`TIMEOUT`], with what
//! came (`complete` false): a drawable that is never presented has no
//! presented handler run. A record once handed out is never made again: a
//! completion for an id that is not in the map is dropped.
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Draw {
    /// How long `nextDrawable` waited for a free drawable, in seconds.
    pub acquire_wait: f64,
    /// When the command buffer was committed.
    pub submitted: f64,
    /// When the GPU started and finished it (0 until the completed handler ran).
    pub gpu_start: f64,
    pub gpu_end: f64,
    /// When the display showed it (0 until the presented handler ran).
    pub presented: f64,
    /// Both handlers ran; false for a record handed out at the timeout.
    pub complete: bool,
}

#[derive(Clone, Copy, Default, Debug)]
struct Pending {
    draw: Draw,
    has_gpu: bool,
    has_presented: bool,
}

impl Pending {
    fn done(&self) -> bool {
        self.has_gpu && self.has_presented
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);
pub static DRAW_ID: AtomicU64 = AtomicU64::new(0);
static DRAWS: Mutex<BTreeMap<u64, Pending>> = Mutex::new(BTreeMap::new());
/// Called with each draw once both completions are in (any thread), for
/// a consumer that wants them as they come (Bloom's phase loop). While
/// only the feedback is on, a record goes once it was handed to it.
static FEEDBACK: Mutex<Option<fn(u64, Draw)>> = Mutex::new(None);
static FEEDBACK_ON: AtomicBool = AtomicBool::new(false);
/// Records kept at most; the oldest goes when a new one comes.
const KEEP: usize = 8192;
/// A record whose second completion has not come this long after its
/// commit is handed out as it is.
pub const TIMEOUT: f64 = 2.0;

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    fn CACurrentMediaTime() -> f64;
}

/// Starts the recording; it runs until the process ends.
pub fn enable() {
    ENABLED.store(true, Ordering::Release);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Sets the consumer of complete records, and turns the recording on for
/// it; `None` turns the feedback off (the trace, if on, goes on).
pub fn set_feedback(f: Option<fn(u64, Draw)>) {
    FEEDBACK_ON.store(f.is_some(), Ordering::Release);
    *FEEDBACK.lock() = f;
}

/// Whether `draw` records anything: the trace or the feedback is on.
pub fn active() -> bool {
    enabled() || FEEDBACK_ON.load(Ordering::Relaxed)
}

/// A completion came for `id`: `fill` puts it in; a record with both
/// completions goes to the feedback (with no lock held, the consumer may
/// take locks of its own), and out of the map unless the trace keeps it
/// for a report.
fn completion(id: u64, fill: impl FnOnce(&mut Pending)) {
    let done = {
        let mut draws = DRAWS.lock();
        let Some(p) = draws.get_mut(&id) else { return };
        let was_done = p.done();
        fill(p);
        if was_done || !p.done() {
            return;
        }
        let mut draw = p.draw;
        draw.complete = true;
        if !enabled() {
            draws.remove(&id);
        }
        draw
    };
    let feedback = *FEEDBACK.lock();
    if let Some(f) = feedback {
        f(id, done);
    }
}

/// Seconds, in the domain of `presentedTime` (`mach_absolute_time`).
pub fn media_time() -> f64 {
    unsafe { CACurrentMediaTime() }
}

/// The id the next draw gets.
pub fn next_draw_id() -> u64 {
    DRAW_ID.load(Ordering::Acquire) + 1
}

/// A draw was committed: starts its record.
pub fn submitted(id: u64, submitted: f64, acquire_wait: f64) {
    let mut draws = DRAWS.lock();
    while draws.len() >= KEEP {
        draws.pop_first();
    }
    draws.insert(
        id,
        Pending {
            draw: Draw { submitted, acquire_wait, ..Default::default() },
            ..Default::default()
        },
    );
}

/// The command buffer's completed handler ran.
pub fn completed(id: u64, gpu_start: f64, gpu_end: f64) {
    completion(id, |p| {
        p.draw.gpu_start = gpu_start;
        p.draw.gpu_end = gpu_end;
        p.has_gpu = true;
    });
}

/// The drawable's presented handler ran.
pub fn presented(id: u64, presented: f64) {
    completion(id, |p| {
        p.draw.presented = presented;
        p.has_presented = true;
    });
}

/// Takes the draws with both completions in, and those older than
/// [`TIMEOUT`] with what they have (`complete` false).
pub fn take_done() -> Vec<(u64, Draw)> {
    take_done_at(media_time())
}

pub fn take_done_at(now: f64) -> Vec<(u64, Draw)> {
    let mut draws = DRAWS.lock();
    let done: Vec<u64> = draws
        .iter()
        .filter(|(_, p)| p.done() || now - p.draw.submitted >= TIMEOUT)
        .map(|(id, _)| *id)
        .collect();
    done.into_iter()
        .filter_map(|id| {
            draws.remove(&id).map(|p| {
                let mut draw = p.draw;
                draw.complete = p.done();
                (id, draw)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share one static map: each uses ids of its own, and
    /// turns the trace on (it never goes off), so records stay for a take.
    #[test]
    fn a_record_is_handed_out_once_both_completions_are_in_either_order() {
        enable();
        // GPU first, then presented.
        submitted(1_000, 10.0, 0.001);
        completed(1_000, 10.001, 10.003);
        assert!(take_done_at(10.01).iter().all(|(id, _)| *id != 1_000), "not before the presented handler");
        presented(1_000, 10.02);
        let got: Vec<_> = take_done_at(10.03).into_iter().filter(|(id, _)| *id == 1_000).collect();
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].1,
            Draw { acquire_wait: 0.001, submitted: 10.0, gpu_start: 10.001, gpu_end: 10.003, presented: 10.02, complete: true }
        );

        // Presented first, a take in between, then the GPU times: the take
        // hands out nothing, and the record keeps both when it comes.
        submitted(1_001, 20.0, 0.0);
        presented(1_001, 20.02);
        assert!(take_done_at(20.03).iter().all(|(id, _)| *id != 1_001), "not before the completed handler");
        completed(1_001, 20.001, 20.004);
        let got: Vec<_> = take_done_at(20.04).into_iter().filter(|(id, _)| *id == 1_001).collect();
        assert_eq!(got.len(), 1);
        assert!(got[0].1.complete);
        assert_eq!(got[0].1.gpu_end, 20.004);
        assert_eq!(got[0].1.presented, 20.02);
    }

    #[test]
    fn a_completion_after_the_record_went_does_not_make_it_again() {
        enable();
        submitted(2_000, 30.0, 0.0);
        completed(2_000, 30.001, 30.003);
        presented(2_000, 30.02);
        assert_eq!(take_done_at(30.03).into_iter().filter(|(id, _)| *id == 2_000).count(), 1);
        // A stray completion, and one for an id that never started.
        completed(2_000, 30.001, 30.003);
        presented(2_001, 30.05);
        assert!(take_done_at(100.0).iter().all(|(id, _)| *id != 2_000 && *id != 2_001));
    }

    #[test]
    fn the_feedback_gets_each_draw_once_with_both_completions() {
        static GOT: Mutex<Vec<(u64, Draw)>> = Mutex::new(Vec::new());
        fn take(id: u64, draw: Draw) {
            if (4_000..5_000).contains(&id) {
                GOT.lock().push((id, draw));
            }
        }
        set_feedback(Some(take));
        assert!(active());
        submitted(4_000, 70.0, 0.0);
        presented(4_000, 70.02);
        assert!(GOT.lock().is_empty(), "not before the second completion");
        completed(4_000, 70.001, 70.003);
        submitted(4_001, 71.0, 0.0);
        completed(4_001, 71.001, 71.003);
        presented(4_001, 71.02);
        // Stray completions after.
        presented(4_000, 70.02);
        completed(4_001, 71.001, 71.003);
        let got = GOT.lock().clone();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, 4_000);
        assert!(got[0].1.complete && got[0].1.presented == 70.02 && got[0].1.gpu_end == 70.003);
        assert_eq!(got[1].0, 4_001);
        set_feedback(None);
    }

    #[test]
    fn a_record_with_one_completion_is_handed_out_incomplete_at_the_timeout() {
        enable();
        // Presented, never completed (the GPU times are 0).
        submitted(3_000, 40.0, 0.0);
        presented(3_000, 40.02);
        assert!(take_done_at(40.0 + TIMEOUT - 0.1).iter().all(|(id, _)| *id != 3_000));
        let got: Vec<_> = take_done_at(40.0 + TIMEOUT).into_iter().filter(|(id, _)| *id == 3_000).collect();
        assert_eq!(got.len(), 1);
        assert!(!got[0].1.complete);
        assert_eq!(got[0].1.gpu_end, 0.);
        assert_eq!(got[0].1.presented, 40.02);
        // Completed, never presented (a drawable that was not shown).
        submitted(3_001, 50.0, 0.0);
        completed(3_001, 50.001, 50.003);
        let got: Vec<_> = take_done_at(50.0 + TIMEOUT).into_iter().filter(|(id, _)| *id == 3_001).collect();
        assert_eq!(got.len(), 1);
        assert!(!got[0].1.complete);
        assert_eq!(got[0].1.presented, 0.);
    }

    #[test]
    fn the_map_is_bounded() {
        // The lowest ids of all the tests, so the pops here take only
        // these records.
        for i in 0..KEEP as u64 + 10 {
            submitted(i, 60.0, 0.0);
        }
        assert!(DRAWS.lock().len() <= KEEP);
        // The oldest went first.
        assert!(!DRAWS.lock().contains_key(&0));
        assert!(DRAWS.lock().contains_key(&(KEEP as u64 + 9)));
    }
}
