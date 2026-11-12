// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bitrate of a stream: the ladder of the Quality menu, and what "Auto"
//! makes of a measured connection. The app measures the link and counts
//! the stalls of the player (`src/adaptive.rs`); the rules are here, with
//! no player and no window: which rung fits a speed, when a step up has
//! room, how many stalls step the quality down.

use std::time::{Duration, Instant};

use anyhow::Result;

// ----- the quality ladder ----------------------------------------------------

/// One choice of the quality menu: bits per second, its name, and the
/// largest picture the web client pairs with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rung {
    pub bitrate: u64,
    pub label: &'static str,
    pub height: &'static str,
}

/// The ladder of the web client (`qualityOptions.js`), highest first.
pub const LADDER: [Rung; 14] = [
    Rung { bitrate: 120_000_000, label: "120 Mbps", height: "4K" },
    Rung { bitrate: 80_000_000, label: "80 Mbps", height: "4K" },
    Rung { bitrate: 60_000_000, label: "60 Mbps", height: "4K" },
    Rung { bitrate: 40_000_000, label: "40 Mbps", height: "4K" },
    Rung { bitrate: 20_000_000, label: "20 Mbps", height: "4K" },
    Rung { bitrate: 15_000_000, label: "15 Mbps", height: "1440p" },
    Rung { bitrate: 10_000_000, label: "10 Mbps", height: "1440p" },
    Rung { bitrate: 8_000_000, label: "8 Mbps", height: "1080p" },
    Rung { bitrate: 6_000_000, label: "6 Mbps", height: "1080p" },
    Rung { bitrate: 4_000_000, label: "4 Mbps", height: "720p" },
    Rung { bitrate: 3_000_000, label: "3 Mbps", height: "720p" },
    Rung { bitrate: 1_500_000, label: "1.5 Mbps", height: "720p" },
    Rung { bitrate: 720_000, label: "720 kbps", height: "480p" },
    Rung { bitrate: 420_000, label: "420 kbps", height: "360p" },
];

/// The rungs under the bitrate of a file: a cap above it changes nothing.
/// A file in a codec that packs more into its bits (hevc, av1, vp9) needs
/// more bits as h264, so the web client counts it one and a half times.
/// With no bitrate known, the whole ladder.
pub fn ladder(source_bitrate: Option<u64>, video_codec: Option<&str>) -> Vec<Rung> {
    let Some(bitrate) = source_bitrate.filter(|b| *b > 0) else {
        return LADDER.to_vec();
    };
    let efficient = matches!(video_codec, Some("hevc" | "h265" | "av1" | "vp9"));
    let reference = if efficient && bitrate <= 20_000_000 { bitrate * 3 / 2 } else { bitrate };
    LADDER.iter().copied().filter(|rung| rung.bitrate < reference).collect()
}

/// "1.5 Mbps" for a bitrate; the name of the rung when it is one.
pub fn bitrate_label(bitrate: u64) -> String {
    if let Some(rung) = LADDER.iter().find(|r| r.bitrate == bitrate) {
        return rung.label.to_string();
    }
    if bitrate >= 1_000_000 {
        format!("{:.1} Mbps", bitrate as f64 / 1_000_000.)
    } else {
        format!("{} kbps", bitrate / 1000)
    }
}

// ----- the numbers ---------------------------------------------------------------

/// The share of the measured speed the player may count on; the rest is
/// for the overhead of the network and its ups and downs. The web client
/// takes the same share.
const SAFETY: f64 = 0.7;
/// A step up needs this much more room than a step down, so the choice
/// does not flip at the edge.
const UP_MARGIN: f64 = 1.25;
/// The test downloads: the bytes asked for, and the speed the next one
/// needs. A slow link stops after the first; the whole run stays under
/// a few MB (the server rounds a body up to the next power of two).
const STEPS: [(u64, u64); 3] = [
    (500_000, 1_000_000),
    (1_000_000, 20_000_000),
    (2_000_000, u64::MAX),
];
/// Stalls count inside this window.
const WINDOW: Duration = Duration::from_secs(60);
/// Stalls in the window, or time stalled in it, that step the quality down.
/// The step is decided when playback goes on, so a stall counts once, at
/// its end, with its whole length.
const STALLS_DOWN: usize = 2;
const STALLED_DOWN: Duration = Duration::from_secs(8);
/// In a group a reload makes everyone wait, so it takes more.
const STALLS_DOWN_GROUP: usize = 3;
const STALLED_DOWN_GROUP: Duration = Duration::from_secs(15);
/// Two rungs at once when the window holds this much stalled time.
const STALLED_TWO: Duration = Duration::from_secs(20);
/// After a change of the quality the next one waits this long.
const HOLD_OFF: Duration = Duration::from_secs(45);
const HOLD_OFF_GROUP: Duration = Duration::from_secs(90);
/// The wall clock ahead of `Instant` by this much means the machine slept.
const SLEEP_GAP: Duration = Duration::from_secs(2);

// ----- the decision ----------------------------------------------------------------

/// What Auto sends for a file: nothing (the file as it is) or a rung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Direct,
    Rung(u64),
}

impl Choice {
    pub fn label(self) -> String {
        match self {
            Choice::Direct => "Original".to_string(),
            Choice::Rung(bitrate) => bitrate_label(bitrate),
        }
    }

    pub fn cap(self) -> Option<u64> {
        match self {
            Choice::Direct => None,
            Choice::Rung(bitrate) => Some(bitrate),
        }
    }
}

/// The speed the player may count on, from the measured one.
pub fn usable(measured: u64) -> u64 {
    (measured as f64 * SAFETY) as u64
}

/// The highest rung under `budget`; the lowest when none fits; direct
/// play when the ladder has no rung under the file.
fn best_rung(rungs: &[Rung], budget: u64) -> Choice {
    rungs
        .iter()
        .find(|rung| rung.bitrate <= budget)
        .or(rungs.last())
        .map_or(Choice::Direct, |rung| Choice::Rung(rung.bitrate))
}

/// Decides for one file from the measured speed (bits per second) and the
/// bitrate of the file. `previous` is the last choice on this link: a step
/// down comes at once, a step up needs room (`UP_MARGIN`). The reason is
/// for the debug channel.
pub fn decide(
    measured: Option<u64>,
    file: Option<u64>,
    codec: Option<&str>,
    previous: Option<Choice>,
) -> (Choice, &'static str) {
    let Some(measured) = measured else {
        return (Choice::Direct, "no measurement");
    };
    let Some(file) = file.filter(|b| *b > 0) else {
        return (Choice::Direct, "file bitrate unknown");
    };
    let usable = usable(measured);
    let rungs = ladder(Some(file), codec);
    let fits_direct = usable >= file;
    let wanted = if fits_direct { Choice::Direct } else { best_rung(&rungs, usable) };
    match previous {
        Some(Choice::Rung(held)) => {
            let with_room = (usable as f64 / UP_MARGIN) as u64;
            if with_room >= file {
                return (Choice::Direct, "room for the file");
            }
            match best_rung(&rungs, with_room) {
                Choice::Rung(up) if up > held => (Choice::Rung(up), "room for a step up"),
                _ => match wanted {
                    Choice::Rung(down) if down < held => (wanted, "too slow for the rung"),
                    _ => (Choice::Rung(held), "kept, no room for a step up"),
                },
            }
        }
        _ if fits_direct => (Choice::Direct, "the link carries the file"),
        _ => (wanted, "the link is under the file"),
    }
}

/// The cap after `steps` rungs down from `playing` (the bitrate of the
/// transcode, or of the file with direct play); none at the bottom.
pub fn step_down(playing: u64, steps: usize) -> Option<u64> {
    let below: Vec<&Rung> = LADDER.iter().filter(|rung| rung.bitrate < playing).collect();
    if below.is_empty() {
        return None;
    }
    Some(below[steps.max(1).min(below.len()) - 1].bitrate)
}

// ----- the measurement ------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measurement {
    /// Bits per second of the last test download.
    pub bps: u64,
    /// All the bytes the run downloaded.
    pub bytes: u64,
    pub took: Duration,
}

fn speed(bytes: u64, took: Duration) -> u64 {
    (bytes as f64 * 8. / took.max(Duration::from_millis(1)).as_secs_f64()) as u64
}

/// Runs the ladder of test downloads. `fetch(size)` downloads a body of
/// `size` bytes and gives the bytes it got and how long that took. The
/// run grows only while the link is fast, and stops at the first failure
/// after a result.
pub fn measure_with(mut fetch: impl FnMut(u64) -> Result<(u64, Duration)>) -> Result<Measurement> {
    let (mut bytes, mut took, mut bps) = (0u64, Duration::ZERO, None);
    for (size, needed) in STEPS {
        match fetch(size) {
            Ok((got, time)) => {
                bytes += got;
                took += time;
                let this = speed(got, time);
                bps = Some(this);
                if this < needed {
                    break;
                }
            }
            Err(err) if bps.is_none() => return Err(err),
            Err(_) => break,
        }
    }
    Ok(Measurement { bps: bps.unwrap_or(0), bytes, took })
}

// ----- the stalls -------------------------------------------------------------------

/// The stalls of the last minute, and the hold-off after a change.
#[derive(Debug, Default)]
pub struct Stepper {
    stalls: Vec<(Instant, Duration)>,
    last_change: Option<Instant>,
}

impl Stepper {
    pub const fn new() -> Self {
        Self { stalls: Vec::new(), last_change: None }
    }

    /// A stall that began at `at` and ended after `length`.
    pub fn stall(&mut self, at: Instant, length: Duration) {
        self.stalls.push((at, length));
    }

    /// A stall counts for `WINDOW` after its end: one that lasts longer
    /// than the window is still there when playback goes on, which is
    /// when the step is decided.
    fn prune(&mut self, now: Instant) {
        self.stalls.retain(|(at, length)| now.saturating_duration_since(*at + *length) < WINDOW);
    }

    pub fn count(&mut self, now: Instant) -> usize {
        self.prune(now);
        self.stalls.len()
    }

    pub fn stalled(&mut self, now: Instant) -> Duration {
        self.prune(now);
        self.stalls.iter().map(|(_, length)| *length).sum()
    }

    pub fn hold_off_left(&self, now: Instant, in_group: bool) -> Duration {
        let hold = if in_group { HOLD_OFF_GROUP } else { HOLD_OFF };
        match self.last_change {
            Some(at) => hold.saturating_sub(now.saturating_duration_since(at)),
            None => Duration::ZERO,
        }
    }

    /// The rungs to go down now by the stalls in the window: 0 when the
    /// player keeps up, or inside the hold-off. A step clears the window
    /// and starts the hold-off.
    pub fn step(&mut self, now: Instant, in_group: bool) -> usize {
        if !self.hold_off_left(now, in_group).is_zero() {
            return 0;
        }
        let (count, stalled) = (self.count(now), self.stalled(now));
        let (need_count, need_time) = if in_group {
            (STALLS_DOWN_GROUP, STALLED_DOWN_GROUP)
        } else {
            (STALLS_DOWN, STALLED_DOWN)
        };
        if count < need_count && stalled < need_time {
            return 0;
        }
        let steps = if stalled >= STALLED_TWO { 2 } else { 1 };
        self.stalls.clear();
        self.last_change = Some(now);
        steps
    }

    /// Marks a change made for another reason (a step up).
    pub fn changed(&mut self, now: Instant) {
        self.stalls.clear();
        self.last_change = Some(now);
    }

    pub fn clear(&mut self) {
        self.stalls.clear();
    }
}

/// True when the wall clock went on by more than the clock of the process
/// did between two polls: the machine slept in between.
pub fn slept(wall_passed: Duration, mono_passed: Duration) -> bool {
    wall_passed.saturating_sub(mono_passed) > SLEEP_GAP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_stops_under_the_source_bitrate() {
        let rungs = ladder(Some(3_794_694), Some("h264"));
        assert_eq!(rungs.len(), 4);
        assert_eq!(rungs[0].label, "3 Mbps");
        assert_eq!(rungs[3].label, "420 kbps");
        assert_eq!(rungs[0].height, "720p");
        // An efficient codec counts one and a half times.
        let rungs = ladder(Some(3_000_000), Some("hevc"));
        assert_eq!(rungs[0].label, "4 Mbps");
        assert_eq!(ladder(None, None).len(), 14);
        assert_eq!(ladder(Some(0), None).len(), 14);
        assert_eq!(bitrate_label(1_500_000), "1.5 Mbps");
        assert_eq!(bitrate_label(3_794_694), "3.8 Mbps");
        assert_eq!(bitrate_label(640_000), "640 kbps");
    }

    /// A stall longer than the window still counts when playback goes on,
    /// with its whole length: twenty seconds in one stall are two steps.
    #[test]
    fn a_long_stall_counts_whole_and_after_its_end() {
        let mut stepper = Stepper::new();
        let began = Instant::now();
        stepper.stall(began, Duration::from_secs(90));
        let end = began + Duration::from_secs(90);
        assert_eq!(stepper.count(end), 1, "the stall was dropped as old");
        assert_eq!(stepper.stalled(end), Duration::from_secs(90));
        assert_eq!(stepper.step(end, false), 2);
    }

    /// A sleep is the wall clock ahead of the process clock between two
    /// polls; a clock set by a second is none.
    #[test]
    fn a_sleep_is_a_gap_between_two_polls() {
        let poll = Duration::from_millis(250);
        assert!(!slept(poll, poll));
        assert!(!slept(poll + Duration::from_secs(1), poll), "a clock set by a second");
        assert!(slept(Duration::from_secs(600), poll));
        // The wall clock set back is no sleep.
        assert!(!slept(Duration::ZERO, poll));
    }

    const M: u64 = 1_000_000;

    #[test]
    fn measurement_grows_only_on_a_fast_link() {
        // A fast link: every step runs, the result is the last one.
        let mut asked = Vec::new();
        let fast = measure_with(|size| {
            asked.push(size);
            Ok((size, Duration::from_millis(size / 10_000)))
        })
        .unwrap();
        assert_eq!(asked, [500_000, 1_000_000, 2_000_000]);
        assert_eq!(fast.bytes, 3_500_000);
        assert_eq!(fast.bps, 80 * M);
        // A slow link stops after the first step: 500 KB in 8 s.
        let mut asked = Vec::new();
        let slow = measure_with(|size| {
            asked.push(size);
            Ok((size, Duration::from_secs(8)))
        })
        .unwrap();
        assert_eq!(asked, [500_000]);
        assert_eq!(slow.bytes, 500_000);
        assert_eq!(slow.bps, 500_000);
        // A middle link (10 Mbps) stops after the second.
        let mut asked = Vec::new();
        let middle = measure_with(|size| {
            asked.push(size);
            Ok((size, Duration::from_millis(size * 8 / 10_000)))
        })
        .unwrap();
        assert_eq!(asked.len(), 2);
        assert_eq!(middle.bps, 10 * M);
        // A failure after a result keeps the result; one before is an error.
        let kept = measure_with(|size| {
            if size > 500_000 { anyhow::bail!("timeout") }
            Ok((size, Duration::from_millis(100)))
        })
        .unwrap();
        assert_eq!(kept.bps, 40 * M);
        assert!(measure_with(|_| anyhow::bail!("down")).is_err());
    }

    #[test]
    fn decision_table() {
        let file = Some(9_900_000);
        let cases: Vec<(&str, Option<u64>, Option<u64>, Option<Choice>, Choice)> = vec![
            ("no measurement is the file", None, file, None, Choice::Direct),
            ("no file bitrate is the file", Some(3 * M), None, None, Choice::Direct),
            ("a fast link is the file", Some(50 * M), file, None, Choice::Direct),
            // 15 Mbps × 0.7 = 10.5 Mbps ≥ 9.9 Mbps.
            ("just enough is the file", Some(15 * M), file, None, Choice::Direct),
            // 14 Mbps × 0.7 = 9.8 Mbps < 9.9 Mbps: the rung under 9.8.
            ("just under takes the 8 Mbps rung", Some(14 * M), file, None, Choice::Rung(8 * M)),
            ("3 Mbps takes 1.5 Mbps", Some(3 * M), file, None, Choice::Rung(1_500_000)),
            ("a crawl takes the lowest rung", Some(100_000), file, None, Choice::Rung(420_000)),
            // The ladder has no rung under a tiny file: the file as it is.
            ("a tiny file is the file", Some(100_000), Some(300_000), None, Choice::Direct),
            // 1 Mbps × 0.7 = 700 kbps; hevc counts the file one and a half
            // times, so the 720 kbps rung is in the ladder: it fits.
            ("hevc ladder", Some(1_030_000), Some(600_000), None, Choice::Direct),
        ];
        for (name, measured, file, previous, want) in cases {
            let (got, why) = decide(measured, file, None, previous);
            assert_eq!(got, want, "{name} ({why})");
        }
        let (got, _) = decide(Some(1_030_000), Some(600_000), Some("hevc"), None);
        assert_eq!(got, Choice::Direct);
        let (got, _) = decide(Some(1_000_000), Some(700_000), Some("hevc"), None);
        assert_eq!(got, Choice::Direct);
        // 1 Mbps × 0.7 = 700 kbps: the 720 kbps rung does not fit, the
        // lowest one plays.
        let (got, _) = decide(Some(1_000_000), Some(1_000_000), Some("hevc"), None);
        assert_eq!(got, Choice::Rung(420_000));
    }

    #[test]
    fn hysteresis_at_the_edge() {
        let file = Some(9_900_000);
        // From the file: a step down comes as soon as the link is under it.
        let (got, _) = decide(Some(14 * M), file, None, Some(Choice::Direct));
        assert_eq!(got, Choice::Rung(8 * M));
        // From a rung: back to the file needs 25% of room. 15 Mbps × 0.7
        // = 10.5 ≥ 9.9 fits, but 10.5 / 1.25 = 8.4 does not.
        let (got, why) = decide(Some(15 * M), file, None, Some(Choice::Rung(8 * M)));
        assert_eq!(got, Choice::Rung(8 * M), "{why}");
        // 18 Mbps × 0.7 / 1.25 = 10.08 ≥ 9.9: the file.
        let (got, _) = decide(Some(18 * M), file, None, Some(Choice::Rung(8 * M)));
        assert_eq!(got, Choice::Direct);
        // A step up by one rung needs the room too: 9 Mbps × 0.7 = 6.3
        // would take the 6 Mbps rung, but 6.3 / 1.25 = 5.04 holds 4 Mbps.
        let (got, _) = decide(Some(9 * M), file, None, Some(Choice::Rung(4 * M)));
        assert_eq!(got, Choice::Rung(4 * M));
        // 11 Mbps × 0.7 / 1.25 = 6.16: up to 6 Mbps.
        let (got, _) = decide(Some(11 * M), file, None, Some(Choice::Rung(4 * M)));
        assert_eq!(got, Choice::Rung(6 * M));
        // A step down from a rung comes at once.
        let (got, _) = decide(Some(5 * M), file, None, Some(Choice::Rung(4 * M)));
        assert_eq!(got, Choice::Rung(3 * M));
        // The same speed again keeps the rung: no flip.
        let (got, _) = decide(Some(5 * M), file, None, Some(Choice::Rung(3 * M)));
        assert_eq!(got, Choice::Rung(3 * M));
    }

    #[test]
    fn rungs_down_from_what_plays() {
        assert_eq!(step_down(9_900_000, 1), Some(8 * M));
        assert_eq!(step_down(9_900_000, 2), Some(6 * M));
        assert_eq!(step_down(8 * M, 1), Some(6 * M));
        assert_eq!(step_down(420_000, 1), None);
        // More steps than rungs stop at the bottom.
        assert_eq!(step_down(1_500_000, 5), Some(420_000));
    }

    #[test]
    fn stalls_in_a_window_with_a_hold_off() {
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let mut stepper = Stepper::new();
        // One hiccup is no reason.
        stepper.stall(at(1), Duration::from_secs(3));
        assert_eq!(stepper.step(at(5), false), 0);
        // A second stall inside the window: one rung.
        stepper.stall(at(20), Duration::from_secs(3));
        assert_eq!(stepper.step(at(24), false), 1);
        // The window is clear and the hold-off runs: more stalls do nothing.
        stepper.stall(at(30), Duration::from_secs(3));
        stepper.stall(at(40), Duration::from_secs(3));
        assert_eq!(stepper.step(at(44), false), 0);
        assert_eq!(stepper.hold_off_left(at(44), false), Duration::from_secs(25));
        // After the hold-off those stalls are still in the window: a step.
        assert_eq!(stepper.step(at(70), false), 1);
        // Stalls further apart than the window do not add up.
        let mut stepper = Stepper::new();
        stepper.stall(at(0), Duration::from_secs(3));
        stepper.stall(at(70), Duration::from_secs(3));
        assert_eq!(stepper.step(at(75), false), 0);
        // One long stall counts by its length; a very long one is two rungs.
        let mut stepper = Stepper::new();
        stepper.stall(at(0), Duration::from_secs(9));
        assert_eq!(stepper.step(at(10), false), 1);
        let mut stepper = Stepper::new();
        stepper.stall(at(0), Duration::from_secs(21));
        assert_eq!(stepper.step(at(22), false), 2);
    }

    #[test]
    fn a_group_takes_more() {
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let mut stepper = Stepper::new();
        stepper.stall(at(1), Duration::from_secs(3));
        stepper.stall(at(10), Duration::from_secs(3));
        assert_eq!(stepper.step(at(14), true), 0);
        stepper.stall(at(20), Duration::from_secs(3));
        assert_eq!(stepper.step(at(24), true), 1);
        assert_eq!(stepper.hold_off_left(at(24), true), Duration::from_secs(90));
    }
}
