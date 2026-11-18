//! "Auto" quality on a slow link (the Mac's src/adaptive.rs, its request):
//! a few test downloads from the server measure the link, and core's rules
//! (`bitrate::decide`) say whether a file plays as it is or at a rung.

use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Context;
use bloom_core::bitrate::{Choice, Stepper, decide, measure_with, step_down};

use crate::{BloomError, Server, run};

/// One test download can take this long; what came by then counts.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

#[uniffi::export]
impl Server {
    /// Measures the link to the server in bits per second (a few MB at
    /// most, `GET /Playback/BitrateTest`, as the web client does).
    pub async fn measure_link(&self) -> Result<u64, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let measurement = measure_with(|size| {
                let url = client.url("/Playback/BitrateTest", &[("size", size.to_string())]);
                let mut response = client
                    .agent()
                    .get(&url)
                    .config()
                    .timeout_global(Some(STEP_TIMEOUT))
                    .build()
                    .header("Authorization", client.auth_header())
                    .header("Cache-Control", "no-cache, no-store")
                    .call()
                    .context("bitrate test")?;
                let started = Instant::now();
                let mut reader = response.body_mut().with_config().limit(size * 2 + 4096).reader();
                let mut buffer = [0u8; 64 * 1024];
                let mut bytes = 0u64;
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => bytes += n as u64,
                        Err(_) if bytes > 0 => break,
                        Err(err) => return Err(err).context("bitrate test body"),
                    }
                }
                Ok((bytes, started.elapsed()))
            })?;
            Ok(measurement.bps)
        })
        .await
    }
}

/// What Auto sends for a file on a measured link: none (the file as it is)
/// or a cap. `previous` is the cap Auto chose on this link before.
#[uniffi::export]
pub fn auto_cap(measured_bps: Option<u64>, file_bps: Option<u64>, codec: Option<String>, previous: Option<u64>) -> Option<u64> {
    let previous = previous.map(Choice::Rung);
    decide(measured_bps, file_bps, codec.as_deref(), previous).0.cap()
}

/// The cap a stream is asked with: the one the user chose (none for
/// Auto), and on cellular the cellular limit as well, the lower of the two
/// (spec Q11). `cellular` none is "Original": no limit of its own.
#[uniffi::export]
pub fn request_cap(chosen: Option<u64>, cellular: Option<u64>, on_cellular: bool) -> Option<u64> {
    match (chosen, cellular.filter(|_| on_cellular)) {
        (Some(chosen), Some(cellular)) => Some(chosen.min(cellular)),
        (chosen, cellular) => chosen.or(cellular),
    }
}

/// The phone moved to cellular while a stream plays: the cap to load
/// again with when what plays is over the cellular limit (or its bitrate
/// is not known), else none.
#[uniffi::export]
pub fn cellular_step(playing_bps: Option<u64>, cellular: Option<u64>) -> Option<u64> {
    let cap = cellular?;
    playing_bps.is_none_or(|bps| bps > cap).then_some(cap)
}

/// A pause for data shorter than this is a hiccup, not a stall (the Mac's
/// `MIN_STALL`).
const MIN_STALL: Duration = Duration::from_secs(1);

/// Without a stall for this long, and with a measurement newer than the
/// step down, the quality goes back up while the item plays (the Mac's
/// `RECOVERY`).
const RECOVERY: Duration = Duration::from_secs(10 * 60);

/// A step back up while the item plays: the cap to load again with (none
/// for the file as it is) and its name for the toast.
#[derive(uniffi::Record, Debug, PartialEq, Eq)]
pub struct StepUp {
    pub cap: Option<u64>,
    pub label: String,
}

/// Stalls of the player under Auto (the Mac's `adaptive_tick`): repeated
/// pauses for data step the quality down a rung (core's `Stepper` and
/// `step_down`), and the cap they set holds for later loads until a
/// measurement newer than the step says otherwise. Ten minutes without a
/// stall after such a measurement step it back up while the item plays.
#[derive(uniffi::Object, Default)]
pub struct StallWatch {
    inner: Mutex<Watch>,
}

#[derive(Default)]
struct Watch {
    stepper: Stepper,
    /// When the pause for data that runs now began.
    since: Option<Instant>,
    /// The cap set after stalls, and when.
    stepped: Option<(u64, Instant)>,
    /// When the link was last measured.
    measured_at: Option<Instant>,
    /// When the last pause for data ended (a hiccup too, as on the Mac).
    last_stall_end: Option<Instant>,
}

impl Watch {
    /// Whether a measurement came after the step down.
    fn measured_since_step(&self) -> bool {
        matches!((self.stepped, self.measured_at), (Some((_, step)), Some(measured)) if measured > step)
    }
}

impl StallWatch {
    fn lock(&self) -> std::sync::MutexGuard<'_, Watch> {
        self.inner.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn buffering_at(&self, on: bool, now: Instant) {
        let mut watch = self.lock();
        match (on, watch.since) {
            (true, None) => watch.since = Some(now),
            (false, Some(since)) => {
                let length = now.saturating_duration_since(since);
                if length >= MIN_STALL {
                    watch.stepper.stall(since, length);
                }
                watch.since = None;
                watch.last_stall_end = Some(now);
            }
            _ => {}
        }
    }

    fn step_at(&self, playing_bps: Option<u64>, in_group: bool, now: Instant) -> Option<u64> {
        let mut watch = self.lock();
        // A stall still running is decided when it ends, with its length.
        if watch.since.is_some() {
            return None;
        }
        let steps = watch.stepper.step(now, in_group);
        if steps == 0 {
            return None;
        }
        let cap = playing_bps.and_then(|bps| step_down(bps, steps))?;
        watch.stepped = Some((cap, now));
        Some(cap)
    }

    fn measured_at(&self, now: Instant) {
        self.lock().measured_at = Some(now);
    }

    /// The Mac's recovery, in its order: not in a group, the hold-off over, a measurement newer than the step down,
    /// and ten minutes since the later of the step and the last stall.
    /// Then the link decides as for a new load; a higher cap than the one
    /// of the stalls (or the file) is a step up.
    fn step_up_at(
        &self,
        measured_bps: Option<u64>,
        file_bps: Option<u64>,
        codec: Option<String>,
        previous: Option<u64>,
        in_group: bool,
        now: Instant,
    ) -> Option<StepUp> {
        let mut watch = self.lock();
        if watch.since.is_some() || in_group {
            return None;
        }
        // A step down is decided at the end of a stall (`step`); ten
        // minutes without one leave no stall in its one-minute window.
        let (cap, at) = watch.stepped?;
        if !watch.stepper.hold_off_left(now, in_group).is_zero() || !watch.measured_since_step() {
            return None;
        }
        let calm_since = watch.last_stall_end.map_or(at, |end| end.max(at));
        if now.saturating_duration_since(calm_since) < RECOVERY {
            return None;
        }
        let (choice, _) = decide(measured_bps, file_bps, codec.as_deref(), previous.map(Choice::Rung));
        if choice.cap().is_some_and(|up| up <= cap) {
            return None;
        }
        watch.stepped = None;
        watch.stepper.changed(now);
        Some(StepUp { cap: choice.cap(), label: choice.label() })
    }
}

#[uniffi::export]
impl StallWatch {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The player began (`true`) or ended (`false`) a pause for data
    /// while it played. A pause counts at its end, with its whole length.
    pub fn buffering(&self, on: bool) {
        self.buffering_at(on, Instant::now());
    }

    /// The cap to load again with after the stalls of the last minute, or
    /// none. `playing_bps` is the bitrate that plays (the cap of the
    /// transcode, or the file's). Inside the hold-off after a step, or at
    /// the lowest rung, none.
    pub fn step(&self, playing_bps: Option<u64>, in_group: bool) -> Option<u64> {
        self.step_at(playing_bps, in_group, Instant::now())
    }

    /// Asked about every 10 s while the item plays under Auto (not paused,
    /// not waiting for data, not scrubbing, not a downloaded file): the
    /// step back up, if one is due. Never in a SyncPlay group (the Mac's
    /// rule). The measurement comes from `measured`, which runs only while
    /// nothing plays: a pause or the end of an item after the step down.
    pub fn step_up(
        &self,
        measured_bps: Option<u64>,
        file_bps: Option<u64>,
        codec: Option<String>,
        previous: Option<u64>,
        in_group: bool,
    ) -> Option<StepUp> {
        self.step_up_at(measured_bps, file_bps, codec, previous, in_group, Instant::now())
    }

    /// What Auto sends with the cap of the stalls over `decided` (the cap
    /// from the measured link, `auto_cap`): the lower one; none when the
    /// file is under the cap anyway. A measurement newer than the step
    /// lifts the cap of the stalls (the Mac's `auto_cap`).
    pub fn cap(&self, decided: Option<u64>, file_bps: Option<u64>) -> Option<u64> {
        let mut watch = self.lock();
        if watch.measured_since_step() {
            watch.stepped = None;
        }
        let Some((held, _)) = watch.stepped else {
            return decided;
        };
        if decided.is_some_and(|cap| cap <= held) {
            return decided;
        }
        match file_bps {
            Some(file) if held >= file => None,
            _ => Some(held),
        }
    }

    /// The cap after stalls, if one holds.
    pub fn held(&self) -> Option<u64> {
        self.lock().stepped.map(|(cap, _)| cap)
    }

    /// The link was measured: the next load decides from it, and a step
    /// up may follow while the item plays.
    pub fn measured(&self) {
        self.measured_at(Instant::now());
    }

    /// A new network path: the cap of stalls and the measurement belong
    /// to the old one.
    pub fn forget(&self) {
        let mut watch = self.lock();
        watch.stepped = None;
        watch.measured_at = None;
    }

    /// Another item, or a manual quality: the stalls so far count no more.
    pub fn clear(&self) {
        let mut watch = self.lock();
        watch.stepper.clear();
        watch.since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: u64 = 1_000_000;

    fn stall(watch: &StallWatch, at: Instant, secs: u64) -> Instant {
        watch.buffering_at(true, at);
        let end = at + Duration::from_secs(secs);
        watch.buffering_at(false, end);
        end
    }

    #[test]
    fn two_stalls_step_down_a_rung_and_the_cap_holds() {
        let watch = StallWatch::default();
        let start = Instant::now();
        let end = stall(&watch, start, 2);
        assert_eq!(watch.step_at(Some(10 * M), false, end), None, "one stall is not enough");
        let end = stall(&watch, end + Duration::from_secs(5), 2);
        let cap = watch.step_at(Some(10 * M), false, end).expect("a step down");
        assert!(cap < 10 * M);
        assert_eq!(watch.held(), Some(cap));
        // The cap of the stalls wins over a link that looks fast, and a
        // lower cap of the link wins over it.
        assert_eq!(watch.cap(None, Some(10 * M)), Some(cap));
        assert_eq!(watch.cap(Some(M), Some(10 * M)), Some(M));
        // A file under the cap plays as it is.
        assert_eq!(watch.cap(None, Some(cap / 2)), None);
        // The hold-off: more stalls at once change nothing.
        let end = stall(&watch, end, 3);
        let end = stall(&watch, end, 3);
        assert_eq!(watch.step_at(Some(cap), false, end), None);
        // A measurement before the step does not lift it; a newer one does.
        watch.measured_at(start);
        assert_eq!(watch.cap(None, Some(10 * M)), Some(cap));
        watch.measured_at(end + Duration::from_secs(1));
        assert_eq!(watch.cap(None, Some(10 * M)), None);
        assert_eq!(watch.held(), None);
    }

    /// Two stalls step 10 Mbps down; returns the watch, the cap and the
    /// time of the step.
    fn stepped_down() -> (StallWatch, u64, Instant) {
        let watch = StallWatch::default();
        let end = stall(&watch, Instant::now(), 2);
        let end = stall(&watch, end, 2);
        let cap = watch.step_at(Some(10 * M), false, end).expect("a step down");
        (watch, cap, end)
    }

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn steps_up_after_ten_calm_minutes_and_a_newer_measurement() {
        let (watch, cap, at) = stepped_down();
        let file = Some(10 * M);
        // A fast link, measured at a pause after the step.
        watch.measured_at(at + MIN);
        assert_eq!(watch.step_up_at(Some(100 * M), file, None, Some(cap), false, at + 9 * MIN), None, "nine minutes are too few");
        let up = watch.step_up_at(Some(100 * M), file, None, Some(cap), false, at + 10 * MIN).expect("a step up");
        assert_eq!(up, StepUp { cap: None, label: "Original".into() });
        assert_eq!(watch.held(), None, "the cap of the stalls is gone");
        // Once is enough: nothing more to step up from.
        assert_eq!(watch.step_up_at(Some(100 * M), file, None, None, false, at + 20 * MIN), None);
    }

    #[test]
    fn a_link_that_carries_only_part_steps_up_to_its_rung() {
        let (watch, cap, at) = stepped_down();
        watch.measured_at(at + MIN);
        // 16 Mbps measured is 11.2 usable: the 10 Mbps rung, over the
        // 8 Mbps of the stalls and under the file.
        let file = Some(20 * M);
        let up = watch.step_up_at(Some(16 * M), file, None, None, false, at + 11 * MIN).expect("a step up");
        assert_eq!((cap, up.cap, up.label.as_str()), (8 * M, Some(10 * M), "10 Mbps"));
        // A link no faster than the cap: no step, and the cap stays.
        let (watch, cap, at) = stepped_down();
        watch.measured_at(at + MIN);
        assert_eq!(watch.step_up_at(Some(cap), file, None, None, false, at + 11 * MIN), None);
        assert_eq!(watch.held(), Some(cap));
    }

    #[test]
    fn no_step_up_without_a_newer_measurement_in_a_group_or_after_a_stall() {
        let (watch, cap, at) = stepped_down();
        let file = Some(10 * M);
        // Measured before the step (or never): the cap holds.
        assert_eq!(watch.step_up_at(Some(100 * M), file, None, Some(cap), false, at + 30 * MIN), None);
        watch.measured_at(at - MIN);
        assert_eq!(watch.step_up_at(Some(100 * M), file, None, Some(cap), false, at + 30 * MIN), None);
        watch.measured_at(at + MIN);
        // In a SyncPlay group: never (the Mac's rule).
        assert_eq!(watch.step_up_at(Some(100 * M), file, None, Some(cap), true, at + 30 * MIN), None);
        // A hiccup restarts the ten minutes.
        watch.buffering_at(true, at + 5 * MIN);
        watch.buffering_at(false, at + 5 * MIN + Duration::from_millis(300));
        assert_eq!(watch.step_up_at(Some(100 * M), file, None, Some(cap), false, at + 14 * MIN), None);
        // A pause for data that runs now: nothing.
        watch.buffering_at(true, at + 15 * MIN + Duration::from_secs(30));
        assert_eq!(watch.step_up_at(Some(100 * M), file, None, Some(cap), false, at + 16 * MIN), None);
        watch.buffering_at(false, at + 15 * MIN + Duration::from_secs(30) + Duration::from_millis(300));
        assert!(watch.step_up_at(Some(100 * M), file, None, Some(cap), false, at + 26 * MIN).is_some());
    }

    #[test]
    fn a_new_path_forgets_the_cap_and_the_measurement() {
        let (watch, cap, at) = stepped_down();
        watch.measured_at(at + MIN);
        watch.forget();
        assert_eq!(watch.held(), None);
        assert_eq!(watch.step_up_at(Some(100 * M), Some(10 * M), None, Some(cap), false, at + 30 * MIN), None);
    }

    #[test]
    fn the_cellular_limit_is_the_lower_cap_on_cellular_alone() {
        assert_eq!(request_cap(None, None, true), None, "Auto and Original");
        assert_eq!(request_cap(None, Some(4 * M), true), Some(4 * M));
        assert_eq!(request_cap(None, Some(4 * M), false), None, "Wi-Fi: no cellular limit");
        assert_eq!(request_cap(Some(8 * M), Some(4 * M), true), Some(4 * M));
        assert_eq!(request_cap(Some(2 * M), Some(4 * M), true), Some(2 * M));
        assert_eq!(request_cap(Some(8 * M), None, true), Some(8 * M));
        assert_eq!(request_cap(Some(8 * M), Some(4 * M), false), Some(8 * M));
        // A move to cellular mid-stream.
        assert_eq!(cellular_step(Some(20 * M), Some(4 * M)), Some(4 * M));
        assert_eq!(cellular_step(Some(4 * M), Some(4 * M)), None, "already under it");
        assert_eq!(cellular_step(None, Some(4 * M)), Some(4 * M), "an unknown bitrate takes the limit");
        assert_eq!(cellular_step(Some(20 * M), None), None, "Original");
    }

    #[test]
    fn hiccups_and_a_running_stall_do_not_step() {
        let watch = StallWatch::default();
        let start = Instant::now();
        // Under a second each: hiccups.
        let mut at = start;
        for _ in 0..4 {
            watch.buffering_at(true, at);
            at += Duration::from_millis(500);
            watch.buffering_at(false, at);
        }
        assert_eq!(watch.step_at(Some(10 * M), false, at), None);
        // A long stall decides at its end, with its length.
        watch.buffering_at(true, at);
        assert_eq!(watch.step_at(Some(10 * M), false, at + Duration::from_secs(30)), None);
        watch.buffering_at(false, at + Duration::from_secs(25));
        let cap = watch.step_at(Some(10 * M), false, at + Duration::from_secs(25)).expect("a step");
        // 20 s or more stalled: two rungs at once.
        assert_eq!(Some(cap), step_down(10 * M, 2));
    }

    #[test]
    fn a_group_needs_more_and_the_bottom_has_no_step() {
        let watch = StallWatch::default();
        let start = Instant::now();
        let end = stall(&watch, start, 2);
        let end = stall(&watch, end, 2);
        assert_eq!(watch.step_at(Some(10 * M), true, end), None, "a group needs three");
        let end = stall(&watch, end, 2);
        assert!(watch.step_at(Some(10 * M), true, end).is_some());
        let low = StallWatch::default();
        let end = stall(&low, start, 5);
        let end = stall(&low, end, 5);
        assert_eq!(low.step_at(Some(1000), false, end), None, "nothing under the lowest rung");
        assert_eq!(low.held(), None);
        low.clear();
    }
}
