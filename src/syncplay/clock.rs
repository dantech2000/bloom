// SPDX-License-Identifier: AGPL-3.0-or-later
//! The clock of the server, as seen from this machine.
//!
//! A SyncPlay command says "at this time on the server clock, the position is
//! this". The client asks the server for its time, measures how long the
//! question took, and from that knows how far its own clock is from the
//! server's. Local time here is a count of milliseconds that never jumps:
//! it starts from the wall clock once and then follows [`Instant`], so a
//! change of the wall clock during playback does not move a deadline.

use std::{
    collections::VecDeque,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Measurements kept. The best one is used: a short round trip counts for
/// it, and its age counts against it.
const KEPT: usize = 30;
/// Two clocks drift apart; this is the error an old measurement is charged
/// with, in milliseconds for each second of its age (20 parts in a million).
const DRIFT_PER_SEC: f64 = 0.02;
/// Measurements taken one second apart after a start or a reset.
const QUICK: u32 = 4;
/// A gap this large between the wall clock and [`Instant`] means the machine
/// slept: [`Instant`] stands still during sleep on macOS.
const SLEEP_GAP_MS: f64 = 2000.;

/// One question to the server for its time. All values are milliseconds
/// since the Unix epoch; the first and last are on the local clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exchange {
    pub sent: f64,
    pub server_received: f64,
    pub server_sent: f64,
    pub received: f64,
}

impl Exchange {
    /// Server time minus local time.
    pub fn offset(&self) -> f64 {
        ((self.server_received - self.sent) + (self.server_sent - self.received)) / 2.
    }

    /// Time on the network, both ways, without the time the server took.
    pub fn round_trip(&self) -> f64 {
        (self.received - self.sent) - (self.server_sent - self.server_received)
    }
}

/// Local time that does not jump.
#[derive(Clone, Copy, Debug)]
pub struct LocalClock {
    start: Instant,
    start_ms: f64,
}

impl LocalClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            start_ms: wall_ms(),
        }
    }

    /// Milliseconds since the Unix epoch on the local clock.
    pub fn now(&self) -> f64 {
        self.at(Instant::now())
    }

    pub fn at(&self, instant: Instant) -> f64 {
        self.start_ms + instant.saturating_duration_since(self.start).as_secs_f64() * 1000.
            - self.start.saturating_duration_since(instant).as_secs_f64() * 1000.
    }

    /// The instant of a local time. A time before the clock started gives
    /// the start.
    pub fn instant(&self, ms: f64) -> Instant {
        let ahead = ms - self.start_ms;
        if ahead <= 0. {
            self.start
        } else {
            self.start + Duration::from_secs_f64(ahead / 1000.)
        }
    }

    /// True when the wall clock ran on while [`Instant`] did not: the
    /// machine slept, and every measurement is void.
    pub fn slept(&self) -> bool {
        (wall_ms() - self.now()).abs() > SLEEP_GAP_MS
    }
}

impl Default for LocalClock {
    fn default() -> Self {
        Self::new()
    }
}

fn wall_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |d| d.as_secs_f64() * 1000.)
}

/// The offset between the local clock and the server clock.
#[derive(Debug, Default)]
pub struct ServerClock {
    exchanges: VecDeque<Exchange>,
    taken: u32,
    /// Milliseconds the user adds, for an output that plays late or early.
    pub extra_offset: f64,
}

impl ServerClock {
    pub fn record(&mut self, exchange: Exchange) {
        // A reply that "arrived before the question" is a broken measurement.
        if exchange.round_trip() < 0. {
            return;
        }
        self.exchanges.push_back(exchange);
        while self.exchanges.len() > KEPT {
            self.exchanges.pop_front();
        }
        self.taken += 1;
    }

    /// Forgets the measurements; after a sleep or a new connection.
    pub fn reset(&mut self) {
        self.exchanges.clear();
        self.taken = 0;
    }

    /// True once there is a measurement to work with.
    pub fn ready(&self) -> bool {
        !self.exchanges.is_empty()
    }

    /// The measurement with the smallest possible error: half its round
    /// trip (the network may be uneven by that much) plus the drift of the
    /// clocks since it was made. On a network with a ping that changes, one
    /// good measurement stays in use for minutes, and does not give way to a
    /// worse one just because that one is newer.
    fn best(&self) -> Option<&Exchange> {
        let now = self.exchanges.back()?.received;
        let error = |e: &Exchange| {
            e.round_trip() / 2. + (now - e.received).max(0.) / 1000. * DRIFT_PER_SEC
        };
        self.exchanges.iter().min_by(|a, b| error(a).total_cmp(&error(b)))
    }

    /// Server time minus local time, in milliseconds.
    pub fn offset(&self) -> f64 {
        self.best().map_or(0., Exchange::offset) + self.extra_offset
    }

    /// Time one message takes to the server, in milliseconds.
    pub fn ping(&self) -> f64 {
        self.best().map_or(0., |e| e.round_trip() / 2.)
    }

    pub fn to_server(&self, local_ms: f64) -> f64 {
        local_ms + self.offset()
    }

    pub fn to_local(&self, server_ms: f64) -> f64 {
        server_ms - self.offset()
    }

    /// Time to the next question: a quick series first, then a slow beat.
    /// In a group the beat is faster, because the two clocks drift apart by
    /// some milliseconds in a few minutes.
    pub fn next_poll(&self, in_group: bool) -> Duration {
        if self.taken < QUICK {
            Duration::from_secs(1)
        } else if in_group {
            Duration::from_secs(20)
        } else {
            Duration::from_secs(60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An exchange with a server that is `offset` ms ahead, `up` ms away on
    /// the way there and `down` ms on the way back.
    fn exchange(sent: f64, offset: f64, up: f64, down: f64) -> Exchange {
        let server = sent + up + offset;
        Exchange {
            sent,
            server_received: server,
            server_sent: server + 1.,
            received: sent + up + 1. + down,
        }
    }

    #[test]
    fn offset_and_round_trip_of_a_symmetric_exchange() {
        let e = exchange(1_000., 37_000., 20., 20.);
        assert!((e.offset() - 37_000.).abs() < 1e-6);
        assert!((e.round_trip() - 40.).abs() < 1e-6);
    }

    #[test]
    fn the_exchange_with_the_shortest_round_trip_wins() {
        let mut clock = ServerClock::default();
        // An uneven network makes the offset wrong by half the difference.
        clock.record(exchange(0., 500., 90., 10.));
        clock.record(exchange(1_000., 500., 5., 5.));
        clock.record(exchange(2_000., 500., 10., 70.));
        assert!((clock.offset() - 500.).abs() < 1e-6);
        assert!((clock.ping() - 5.).abs() < 1e-6);
        assert!((clock.to_local(clock.to_server(1234.)) - 1234.).abs() < 1e-6);
    }

    #[test]
    fn a_good_measurement_stays_in_use_until_its_age_costs_more() {
        let mut clock = ServerClock::default();
        // A quick answer, then slower ones a minute apart.
        clock.record(exchange(0., 100., 2., 2.));
        for minute in 1..=5 {
            clock.record(exchange(minute as f64 * 60_000., 120., 15., 15.));
        }
        // Five minutes old: 2 ms of round trip and 6 ms of drift beat 15 ms.
        assert!((clock.offset() - 100.).abs() < 1e-6);
        for minute in 6..=12 {
            clock.record(exchange(minute as f64 * 60_000., 120., 15., 15.));
        }
        // Twelve minutes old: the drift costs more than the slower answer.
        assert!((clock.offset() - 120.).abs() < 1e-6);
    }

    #[test]
    fn keeps_a_limited_number_and_drops_broken_ones() {
        let mut clock = ServerClock::default();
        clock.record(exchange(0., 100., 1., 1.));
        for n in 1..=30 {
            clock.record(exchange(n as f64 * 1000., 200., 10., 10.));
        }
        // The first, best one is gone from the list.
        assert!((clock.offset() - 200.).abs() < 1e-6);
        let before = clock.offset();
        clock.record(Exchange {
            sent: 10.,
            server_received: 0.,
            server_sent: 50.,
            received: 20.,
        });
        assert_eq!(clock.offset(), before);
    }

    #[test]
    fn polls_fast_at_first_and_faster_in_a_group() {
        let mut clock = ServerClock::default();
        assert!(!clock.ready());
        for n in 0..4 {
            assert_eq!(clock.next_poll(true), Duration::from_secs(1));
            clock.record(exchange(n as f64, 0., 1., 1.));
        }
        assert!(clock.ready());
        assert_eq!(clock.next_poll(true), Duration::from_secs(20));
        assert_eq!(clock.next_poll(false), Duration::from_secs(60));
        clock.reset();
        assert_eq!(clock.next_poll(false), Duration::from_secs(1));
    }

    #[test]
    fn extra_offset_shifts_both_ways() {
        let mut clock = ServerClock::default();
        clock.record(exchange(0., 100., 5., 5.));
        clock.extra_offset = 40.;
        assert!((clock.to_server(0.) - 140.).abs() < 1e-6);
        assert!((clock.to_local(140.)).abs() < 1e-6);
    }

    #[test]
    fn local_clock_maps_instants_both_ways() {
        let clock = LocalClock::new();
        let later = Instant::now() + Duration::from_millis(250);
        let ms = clock.at(later);
        let back = clock.instant(ms);
        let error = if back > later { back - later } else { later - back };
        assert!(error < Duration::from_micros(50));
        assert!(!clock.slept());
    }
}
