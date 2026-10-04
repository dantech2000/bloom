// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the sender does with the commands of the user, as a state machine
//! with no AVFoundation in it, so it can be tested. The engine turns the
//! actions it returns into calls on the player.
//!
//! - A pause, play or seek while the item loads waits and runs when the
//!   item is ready; the receiver gets one position, not a burst.
//! - A seek while a seek is in flight is kept and sent when the first one
//!   has landed (the slider sends many).
//! - A stop while the item loads makes that load stale: its `ready` is
//!   ignored.

use std::time::{Duration, Instant};

/// A seek counts as landed after this long, if the position never came
/// near its target (a seek to a point past the end, say).
pub const SEEK_SETTLE: Duration = Duration::from_millis(1500);
/// The position is at the target of a seek within this many seconds.
const SEEK_NEAR: f64 = 1.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Loading { token: u64 },
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Play,
    Pause,
    Seek(f64),
    Unload,
}

#[derive(Debug)]
pub struct Control {
    pub phase: Phase,
    /// Whether the user wants the item to run.
    want_play: bool,
    /// The seek to send when the item is ready or the one in flight landed.
    pending_seek: Option<f64>,
    /// Target and time of the seek the player works on.
    in_flight: Option<(f64, Instant)>,
}

impl Default for Control {
    fn default() -> Self {
        Self { phase: Phase::Idle, want_play: true, pending_seek: None, in_flight: None }
    }
}

impl Control {
    /// A new item was handed to the player.
    pub fn send(&mut self, token: u64, start_secs: f64, play: bool) {
        self.phase = Phase::Loading { token };
        self.want_play = play;
        self.pending_seek = (start_secs > 0.).then_some(start_secs);
        self.in_flight = None;
    }

    pub fn is_loading(&self, token: u64) -> bool {
        self.phase == Phase::Loading { token }
    }

    /// The item of `token` is ready. None when that load is stale (a stop
    /// or a newer send came first).
    pub fn ready(&mut self, token: u64, now: Instant) -> Option<Vec<Action>> {
        if !self.is_loading(token) {
            return None;
        }
        self.phase = Phase::Ready;
        let mut actions = Vec::new();
        if let Some(target) = self.pending_seek.take() {
            self.in_flight = Some((target, now));
            actions.push(Action::Seek(target));
        }
        actions.push(if self.want_play { Action::Play } else { Action::Pause });
        Some(actions)
    }

    pub fn play(&mut self) -> Option<Action> {
        self.want_play = true;
        (self.phase == Phase::Ready).then_some(Action::Play)
    }

    pub fn pause(&mut self) -> Option<Action> {
        self.want_play = false;
        (self.phase == Phase::Ready).then_some(Action::Pause)
    }

    pub fn seek(&mut self, secs: f64, now: Instant) -> Option<Action> {
        let secs = secs.max(0.);
        let busy = match self.in_flight {
            Some((_, at)) => now.duration_since(at) < SEEK_SETTLE,
            None => false,
        };
        if self.phase != Phase::Ready || busy {
            self.pending_seek = Some(secs);
            return None;
        }
        self.in_flight = Some((secs, now));
        Some(Action::Seek(secs))
    }

    /// A look at the position: lets a landed seek go and sends the seek
    /// that waited behind it.
    pub fn tick(&mut self, now: Instant, position: Option<f64>) -> Option<Action> {
        let (target, at) = self.in_flight?;
        let near = position.is_some_and(|p| (p - target).abs() < SEEK_NEAR);
        if !near && now.duration_since(at) < SEEK_SETTLE {
            return None;
        }
        self.in_flight = None;
        if self.phase != Phase::Ready {
            return None;
        }
        let next = self.pending_seek.take()?;
        self.in_flight = Some((next, now));
        Some(Action::Seek(next))
    }

    /// A seek is in flight or waits.
    pub fn seeking(&self) -> bool {
        self.in_flight.is_some() || self.pending_seek.is_some()
    }

    pub fn stop(&mut self) -> Vec<Action> {
        self.phase = Phase::Idle;
        self.pending_seek = None;
        self.in_flight = None;
        vec![Action::Unload]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_while_loading_wait_for_ready() {
        let mut c = Control::default();
        let t0 = Instant::now();
        c.send(1, 90., true);
        assert_eq!(c.pause(), None);
        assert_eq!(c.seek(120., t0), None);
        assert_eq!(c.ready(1, t0), Some(vec![Action::Seek(120.), Action::Pause]));
        assert_eq!(c.play(), Some(Action::Play));
    }

    #[test]
    fn seeks_in_flight_are_coalesced() {
        let mut c = Control::default();
        let t0 = Instant::now();
        c.send(1, 0., true);
        assert_eq!(c.ready(1, t0), Some(vec![Action::Play]));
        assert_eq!(c.seek(10., t0), Some(Action::Seek(10.)));
        // Two more seeks before the first lands: only the last one stays.
        assert_eq!(c.seek(20., t0 + Duration::from_millis(100)), None);
        assert_eq!(c.seek(30., t0 + Duration::from_millis(200)), None);
        assert!(c.seeking());
        // Not near 10 yet, not timed out: nothing happens.
        assert_eq!(c.tick(t0 + Duration::from_millis(300), Some(2.)), None);
        // The first seek landed: the last one goes out.
        assert_eq!(c.tick(t0 + Duration::from_millis(400), Some(10.3)), Some(Action::Seek(30.)));
        assert_eq!(c.tick(t0 + Duration::from_millis(500), Some(10.5)), None);
        // The timeout lets a seek go whose target the position never reached.
        assert_eq!(c.tick(t0 + Duration::from_millis(400) + SEEK_SETTLE, Some(12.)), None);
        assert!(!c.seeking());
        // Then a new seek goes out at once.
        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(c.seek(40., t1), Some(Action::Seek(40.)));
    }

    #[test]
    fn stop_while_loading_makes_the_load_stale() {
        let mut c = Control::default();
        let t0 = Instant::now();
        c.send(1, 0., true);
        assert_eq!(c.stop(), vec![Action::Unload]);
        assert_eq!(c.phase, Phase::Idle);
        assert_eq!(c.ready(1, t0), None);
        // A newer send also makes the old token stale.
        c.send(2, 0., true);
        assert_eq!(c.ready(1, t0), None);
        assert!(c.is_loading(2));
        assert_eq!(c.ready(2, t0), Some(vec![Action::Play]));
        assert_eq!(c.ready(2, t0), None);
    }

    #[test]
    fn stop_clears_a_waiting_seek() {
        let mut c = Control::default();
        let t0 = Instant::now();
        c.send(1, 0., true);
        c.ready(1, t0);
        c.seek(10., t0);
        c.seek(20., t0);
        c.stop();
        assert!(!c.seeking());
        assert_eq!(c.tick(t0 + SEEK_SETTLE, Some(10.)), None);
    }
}
