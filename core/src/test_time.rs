//! Time for tests that run against a real clock.
//!
//! A test that waits on threads, sockets or timers runs on this Mac and on
//! a shared CI runner, which can pause a process for a few hundred
//! milliseconds. Such a test takes its short durations through `scaled`:
//! the factor is 1 on a Mac and 4 under CI (the runners set `CI`), and
//! `BLOOM_TEST_TIME_SCALE` sets it by hand (`dev/test-stress` runs the
//! suites under load with a factor of its own).
//!
//! The rules, in CLAUDE.md ("Timing in tests"): wait for a condition with a
//! deadline, never sleep and then assert; a window in which something must
//! NOT happen is scaled and is many times the time the thing normally
//! takes; a decision that depends on time is a pure function with a test
//! of its own, without a clock.

use std::{sync::OnceLock, time::Duration};

/// The factor for the time of tests in this process.
pub fn scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| {
        let set = std::env::var("BLOOM_TEST_TIME_SCALE").ok().and_then(|v| v.parse::<f64>().ok());
        match set {
            Some(factor) if factor >= 1. => factor,
            _ if std::env::var_os("CI").is_some() => 4.,
            _ => 1.,
        }
    })
}

/// `duration` times the factor of this process.
pub fn scaled(duration: Duration) -> Duration {
    duration.mul_f64(scale())
}

/// `millis` milliseconds, times the factor.
pub fn ms(millis: u64) -> Duration {
    scaled(Duration::from_millis(millis))
}

/// `seconds` seconds, times the factor; for the deadlines of a wait.
pub fn secs(seconds: f64) -> Duration {
    scaled(Duration::from_secs_f64(seconds))
}
