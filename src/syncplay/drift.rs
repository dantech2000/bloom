// SPDX-License-Identifier: AGPL-3.0-or-later
//! Keeps the position of the player at the position of the group.
//!
//! A scheduled start is never exact: the audio device takes a moment, a
//! start after a seek comes out some tens of milliseconds off (measured:
//! about 30 ms), and two clocks drift. A small difference is closed by a
//! short time at a speed a little off 1.0, which is not heard; a large one
//! by a jump.
//!
//! The speed runs for a set time and nothing is measured meanwhile. While
//! the speed is not 1.0, mpv reports an audio position that is off by about
//! 3.4 s times the speed change (measured: +104 ms at 0.97), though the
//! position it plays is right. A correction that followed that number would
//! push the player away from the group.

/// A correction starts above this difference, in milliseconds.
const ENGAGE_MS: f64 = 20.;
/// Above this difference a jump is better than a speed change.
const JUMP_MS: f64 = 1000.;
/// A difference is closed in this time, or a longer one when the speed
/// limit asks for it. A correction of under a second did not always take
/// effect in the player in tests.
const CLOSE_OVER_MS: f64 = 2000.;
/// Largest change of the speed, and the larger one for big differences.
const MAX_CHANGE: f64 = 0.05;
const MAX_CHANGE_FAR: f64 = 0.10;
const FAR_MS: f64 = 300.;
/// Measurements the filter looks at.
const WINDOW: usize = 9;

/// What the player must do about the difference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Correction {
    /// Play at `factor` times the normal speed for `for_ms` milliseconds,
    /// then at the normal speed again. That closes the difference.
    Speed { factor: f64, for_ms: f64 },
    /// Too far apart for a speed change: jump to the group position.
    Jump,
}

/// Decides the correction from the measured differences.
#[derive(Debug, Default)]
pub struct Drift {
    recent: Vec<f64>,
}

impl Drift {
    /// Forgets the measurements; after a start, a pause, a seek or a
    /// correction.
    pub fn reset(&mut self) {
        self.recent.clear();
    }

    /// The middle of the last measurements: one late frame does not count.
    pub fn filtered(&self) -> Option<f64> {
        if self.recent.len() < 3 {
            return None;
        }
        let mut sorted = self.recent.clone();
        sorted.sort_by(f64::total_cmp);
        Some(sorted[sorted.len() / 2])
    }

    /// Takes one measurement, made at normal speed: group position minus
    /// player position, in milliseconds; positive when the player is
    /// behind. Returns a correction when one is due.
    pub fn measure(&mut self, diff_ms: f64) -> Option<Correction> {
        self.recent.push(diff_ms);
        if self.recent.len() > WINDOW {
            self.recent.remove(0);
        }
        let diff = self.filtered()?;
        let size = diff.abs();
        if size < ENGAGE_MS {
            return None;
        }
        self.reset();
        if size >= JUMP_MS {
            return Some(Correction::Jump);
        }
        let limit = if size > FAR_MS { MAX_CHANGE_FAR } else { MAX_CHANGE };
        let change = (diff / CLOSE_OVER_MS).clamp(-limit, limit);
        // At `1 + change` the player gains `change` ms each ms.
        Some(Correction::Speed { factor: 1. + change, for_ms: diff / change })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(drift: &mut Drift, diff: f64, times: usize) -> Option<Correction> {
        (0..times).find_map(|_| drift.measure(diff))
    }

    fn speed(correction: Option<Correction>) -> (f64, f64) {
        match correction {
            Some(Correction::Speed { factor, for_ms }) => (factor, for_ms),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn small_differences_are_left_alone() {
        let mut drift = Drift::default();
        assert_eq!(feed(&mut drift, 15., 9), None);
        assert_eq!(feed(&mut drift, -18., 9), None);
    }

    #[test]
    fn a_late_player_speeds_up_and_an_early_one_slows_down() {
        let mut drift = Drift::default();
        let (factor, for_ms) = speed(feed(&mut drift, 30., 9));
        assert!((factor - 1.015).abs() < 1e-9 && (for_ms - 2000.).abs() < 1e-6);
        let (factor, for_ms) = speed(feed(&mut drift, -30., 9));
        assert!((factor - 0.985).abs() < 1e-9 && (for_ms - 2000.).abs() < 1e-6);
    }

    #[test]
    fn the_speed_has_a_limit_and_the_time_makes_up_for_it() {
        let mut drift = Drift::default();
        let (factor, for_ms) = speed(feed(&mut drift, 200., 9));
        assert!((factor - 1.05).abs() < 1e-9 && (for_ms - 4000.).abs() < 1e-6);
        let (factor, for_ms) = speed(feed(&mut drift, -800., 9));
        assert!((factor - 0.90).abs() < 1e-9 && (for_ms - 8000.).abs() < 1e-6);
    }

    #[test]
    fn the_correction_closes_the_difference() {
        for diff in [25., -60., 140., -450., 900.] {
            let (factor, for_ms) = speed(feed(&mut Drift::default(), diff, 9));
            assert!(((factor - 1.) * for_ms - diff).abs() < 1e-6, "{diff}");
        }
    }

    #[test]
    fn one_outlier_does_not_start_a_correction() {
        let mut drift = Drift::default();
        feed(&mut drift, 5., 8);
        assert_eq!(drift.measure(900.), None);
    }

    #[test]
    fn a_correction_starts_the_measurements_again() {
        let mut drift = Drift::default();
        assert!(feed(&mut drift, 80., 9).is_some());
        assert_eq!(drift.filtered(), None);
    }

    #[test]
    fn a_large_difference_is_a_jump() {
        let mut drift = Drift::default();
        assert_eq!(feed(&mut drift, 4000., 3), Some(Correction::Jump));
        assert_eq!(drift.filtered(), None);
    }
}
