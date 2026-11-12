// SPDX-License-Identifier: AGPL-3.0-or-later
//! "Auto" quality that measures the connection. The app downloads a few
//! test bodies from the server (`GET /Playback/BitrateTest?size=`, as the
//! web client does), keeps the speed it saw, and "Auto" plays the file as
//! it is only when the link carries its bitrate; otherwise the highest rung
//! of the ladder that fits. While an item plays, stalls of the player step
//! the quality down; a fresh measurement lets it come back up.
//!
//! The decision runs on the thread of `stream::negotiate`, without the
//! app, so the state is a global. A manual choice in the Quality menu
//! (`stream::max_bitrate`) always wins over everything here.

use std::{
    io::Read as _,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use gpui_kit::Context;

use crate::{
    app::Bloom,
    jellyfin::Client,
    player::PlayState,
    realtime::SocketEvent,
    stream,
};

pub use bloom_core::bitrate::Choice;
use bloom_core::bitrate::{Measurement, Stepper, bitrate_label, decide, measure_with, slept, step_down};

// ----- the numbers ---------------------------------------------------------------

/// Whether Auto lowers the quality on a slow connection when the user has
/// not chosen. Off: the app plays the file as it is whenever that is
/// possible, and does not compress by default.
pub const DEFAULT_ON: bool = false;

/// A measurement counts as fresh for this long.
const FRESH: Duration = Duration::from_secs(10 * 60);
/// A value kept from an earlier run counts for this long.
const KEPT_FOR: u64 = 24 * 3600;
/// One test download may take this long; what came in by then counts.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
/// A pause for data shorter than this is a hiccup, not a stall.
const MIN_STALL: Duration = Duration::from_secs(1);
/// Without a stall for this long, and with a measurement newer than the
/// step down, the quality goes back up while the item plays.
const RECOVERY: Duration = Duration::from_secs(10 * 60);
/// The length counted for a stall the debug channel feeds.
const FAKE_STALL: Duration = Duration::from_secs(3);

// ----- the measurement ------------------------------------------------------------

#[easy_ext::ext(AdaptiveApi)]
pub(crate) impl Client {
    /// Measures the link to the server. Downloads a few MB at most.
    fn measure_link(&self) -> Result<Measurement> {
        measure_with(|size| self.timed_download(size))
    }

    /// One test download: the bytes that came and the time from the
    /// headers to the end. At the timeout, what came by then counts.
    fn timed_download(&self, size: u64) -> Result<(u64, Duration)> {
        let url = self.url("/Playback/BitrateTest", &[("size", size.to_string())]);
        let mut response = self
            .agent()
            .get(&url)
            .config()
            .timeout_global(Some(STEP_TIMEOUT))
            .build()
            .header("Authorization", self.auth_header())
            .header("Cache-Control", "no-cache, no-store")
            .call()
            .context("bitrate test")?;
        let started = Instant::now();
        // The server rounds its buffer up to a power of two, so the body
        // can be larger than asked.
        let mut reader = response.body_mut().with_config().limit(size * 2 + 4096).reader();
        let mut buffer = [0u8; 64 * 1024];
        let mut bytes = 0u64;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes += n as u64,
                Err(err) if bytes > 0 => {
                    log::info!("link: test download cut short after {bytes} bytes: {err}");
                    break;
                }
                Err(err) => return Err(err).context("bitrate test body"),
            }
        }
        Ok((bytes, started.elapsed()))
    }
}

// ----- the state --------------------------------------------------------------------

/// Where the speed of the link comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// A test download of this run.
    Measured,
    /// The config, from an earlier run.
    Kept,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Measured => "measured",
            Source::Kept => "kept",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Link {
    bps: u64,
    at: Instant,
    bytes: u64,
    from: Source,
}

struct Adaptive {
    enabled: bool,
    link: Option<Link>,
    fake: Option<u64>,
    measuring: bool,
    /// A measurement is due as soon as the link is free, for this reason.
    wanted: Option<&'static str>,
    /// What Auto chose for the item that loaded last, and why.
    choice: Option<Choice>,
    reason: &'static str,
    /// A cap set after stalls, and when; it holds until a measurement
    /// newer than it says otherwise.
    stepped: Option<(u64, Instant)>,
    stepper: Stepper,
    /// When the pause for data that runs now began.
    buffering_since: Option<Instant>,
    last_stall_end: Option<Instant>,
    /// For the sleep check: `Instant` stands still while the Mac sleeps.
    clock: Option<(Instant, SystemTime)>,
    was_open: bool,
    /// The item the stalls belong to.
    item: String,
    last_toast: String,
}

impl Adaptive {
    const fn new() -> Self {
        Self {
            enabled: DEFAULT_ON,
            link: None,
            fake: None,
            measuring: false,
            wanted: None,
            choice: None,
            reason: "",
            stepped: None,
            stepper: Stepper::new(),
            buffering_since: None,
            last_stall_end: None,
            clock: None,
            was_open: false,
            item: String::new(),
            last_toast: String::new(),
        }
    }

    /// The speed the decision reads: the fake one of a test, else the
    /// measured one.
    fn measured(&self) -> Option<u64> {
        self.fake.or(self.link.map(|link| link.bps))
    }

    /// A stall of the player, from mpv or from the debug channel.
    fn on_stall(&mut self, at: Instant, length: Duration) {
        self.stepper.stall(at, length);
        self.last_stall_end = Some(at + length);
    }
}

static STATE: Mutex<Adaptive> = Mutex::new(Adaptive::new());

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Takes the switch and the last measurement from the config.
pub fn install(config: &crate::config::Config) {
    let mut state = STATE.lock().unwrap();
    state.enabled = config.adaptive_quality.unwrap_or(DEFAULT_ON);
    if let (Some(bps), Some(wall)) = (config.link_bps, config.link_measured_at) {
        let age = unix_now().saturating_sub(wall);
        if age <= KEPT_FOR {
            let at = Instant::now()
                .checked_sub(Duration::from_secs(age))
                .unwrap_or_else(Instant::now);
            state.link = Some(Link { bps, at, bytes: 0, from: Source::Kept });
        }
    }
}

/// The cap Auto sends for a file, or none for the file as it is. Records
/// the choice for the menu and the debug channel.
pub fn auto_cap(file: Option<u64>, codec: Option<&str>) -> Option<u64> {
    let mut state = STATE.lock().unwrap();
    if !state.enabled {
        state.choice = Some(Choice::Direct);
        state.reason = "off";
        return None;
    }
    let (mut choice, mut reason) = decide(state.measured(), file, codec, state.choice);
    if let Some((cap, at)) = state.stepped {
        let newer = state.fake.is_some() || state.link.is_some_and(|link| link.at > at);
        if newer {
            state.stepped = None;
        } else if choice.cap().is_none_or(|c| c > cap) {
            // The cap of the stalls holds; above the file it changes nothing.
            choice = match file {
                Some(file) if cap < file => Choice::Rung(cap),
                _ => Choice::Direct,
            };
            reason = "the cap after stalls";
        }
    }
    state.choice = Some(choice);
    state.reason = reason;
    choice.cap()
}

/// What Auto chose for the item that loaded last: the file as it is with
/// the switch off, or before a first load.
pub fn auto_choice() -> Choice {
    let state = STATE.lock().unwrap();
    state.choice.filter(|_| state.enabled).unwrap_or(Choice::Direct)
}

/// "Original" or the rung, for the Auto entry of the Quality menu.
pub fn auto_label() -> String {
    auto_choice().label()
}

/// "48.2 Mbps · 2 min ago", for the Playback info card.
pub fn link_line() -> String {
    let state = STATE.lock().unwrap();
    if let Some(fake) = state.fake {
        return format!("{} (test value)", bitrate_label(fake));
    }
    match state.link {
        Some(link) => {
            let age = link.at.elapsed().as_secs();
            let when = if age < 60 {
                "just now".to_string()
            } else if age < 3600 {
                format!("{} min ago", age / 60)
            } else {
                format!("{} h ago", age / 3600)
            };
            let from = if link.from == Source::Kept { " · from the last run" } else { "" };
            format!("{} · {when}{from}", bitrate_label(link.bps))
        }
        None if state.measuring => "measuring".to_string(),
        None => "not measured".to_string(),
    }
}

/// One line for the debug channel. `in_group` chooses the hold-off shown.
pub fn describe(in_group: bool) -> String {
    let mut state = STATE.lock().unwrap();
    let now = Instant::now();
    let link = match state.link {
        Some(link) => format!(
            "link={} link_age={} link_bytes={} link_from={}",
            link.bps / 1000,
            link.at.elapsed().as_secs(),
            link.bytes,
            link.from.name()
        ),
        None => "link=none link_age=- link_bytes=0 link_from=none".to_string(),
    };
    let auto = match (state.enabled, stream::max_bitrate(), state.choice) {
        (false, _, _) => "off".to_string(),
        (_, Some(_), _) => "manual".to_string(),
        (_, None, Some(Choice::Direct)) => "direct".to_string(),
        (_, None, Some(Choice::Rung(b))) => format!("rung {}", b / 1000),
        (_, None, None) => "none".to_string(),
    };
    let hold = state.stepper.hold_off_left(now, in_group).as_secs();
    let (count, stalled) = (state.stepper.count(now), state.stepper.stalled(now).as_secs());
    format!(
        "{link} fake={} measuring={} wanted={} auto={auto} why={:?} stalls={count} stalled={stalled} \
         stepped={} holdoff={hold} enabled={} toast={:?}",
        state.fake.map_or("off".to_string(), |b| (b / 1000).to_string()),
        state.measuring,
        state.wanted.unwrap_or("no"),
        state.reason,
        state.stepped.map_or("none".to_string(), |(b, _)| (b / 1000).to_string()),
        state.enabled,
        state.last_toast,
    )
}

// ----- the app ----------------------------------------------------------------------

impl Bloom {
    /// At sign-in: the first measurement of this run.
    pub fn adaptive_signed_in(&mut self, cx: &mut Context<Self>) {
        self.adaptive_measure("sign-in", cx);
    }

    /// The socket opened again: the network may have changed.
    pub fn adaptive_socket(&mut self, event: &SocketEvent) {
        if let SocketEvent::Open { again: true } = event {
            STATE.lock().unwrap().wanted = Some("the socket opened again");
        }
    }

    /// Settings > Playback: "Lower the quality on a slow connection".
    pub fn toggle_adaptive_quality(&mut self, cx: &mut Context<Self>) {
        let on = !self.config.adaptive_quality.unwrap_or(DEFAULT_ON);
        self.config.adaptive_quality = Some(on);
        self.save_config(cx);
        STATE.lock().unwrap().enabled = on;
        // Auto with the switch off is the file as it is: load it again.
        if self.player_open && stream::max_bitrate().is_none() {
            self.stream_reload(None, None);
        }
        cx.notify();
    }

    /// Starts a measurement on a background thread. False while one runs
    /// or without a session.
    pub fn adaptive_measure(&mut self, why: &'static str, cx: &mut Context<Self>) -> bool {
        if self.session.is_none() {
            return false;
        }
        {
            let mut state = STATE.lock().unwrap();
            // With the feature off nothing uses the value, so the app does
            // not spend the download. The debug command still measures.
            if state.measuring || (!state.enabled && why != "debug") {
                return false;
            }
            state.measuring = true;
            state.wanted = None;
        }
        log::info!("link: measuring ({why})");
        self.fetch(
            cx,
            |client| client.measure_link(),
            |this, result, cx| {
                let mut state = STATE.lock().unwrap();
                state.measuring = false;
                match result {
                    Ok(m) => {
                        let wall = unix_now();
                        log::info!(
                            "link: {} kbps ({} bytes in {:.2} s)",
                            m.bps / 1000,
                            m.bytes,
                            m.took.as_secs_f64()
                        );
                        state.link = Some(Link {
                            bps: m.bps,
                            at: Instant::now(),
                            bytes: m.bytes,
                            from: Source::Measured,
                        });
                        drop(state);
                        this.config.link_bps = Some(m.bps);
                        this.config.link_measured_at = Some(wall);
                        this.save_config(cx);
                        cx.notify();
                    }
                    Err(err) => log::warn!("link: measurement failed: {err:#}"),
                }
            },
        );
        true
    }

    /// Follows the player: stalls step the quality down, a recovery steps
    /// it up, and a measurement runs when the link is free. Called with
    /// every poll of the player.
    pub fn adaptive_tick(&mut self, cx: &mut Context<Self>) {
        // A reload that waited for a load runs first (`stream`).
        self.stream_poll();
        let now = Instant::now();
        let status = &self.player_status;
        let playing = self.player_open && status.state == PlayState::Playing;
        let in_group = self.sync.in_group();
        let manual = stream::max_bitrate().is_some();
        let mut state = STATE.lock().unwrap();
        // With the switch off nothing reads the stalls or a measurement.
        if !state.enabled {
            state.stepper.clear();
            state.buffering_since = None;
            state.wanted = None;
            state.clock = None;
            state.was_open = self.player_open;
            return;
        }

        // After a sleep the network is often another one. The wall clock
        // went on while the Mac slept and `Instant` did not: the two are
        // compared from one poll to the next, so a clock that is set a
        // little now and then does not add up to a sleep.
        let wall = SystemTime::now();
        if let Some((mono, before)) = state.clock.replace((now, wall)) {
            let wall_passed = wall.duration_since(before).unwrap_or(Duration::ZERO);
            if slept(wall_passed, now.duration_since(mono)) {
                log::info!("link: the machine slept; a measurement is due");
                state.wanted = Some("the machine slept");
                state.stepper.clear();
                state.buffering_since = None;
            }
        }

        // A pause for data counts at its end, with its whole length.
        match (playing && status.buffering, state.buffering_since) {
            (true, None) => state.buffering_since = Some(now),
            (false, Some(since)) => {
                let length = now.duration_since(since);
                if length >= MIN_STALL {
                    state.on_stall(since, length);
                }
                state.buffering_since = None;
                state.last_stall_end = Some(now);
            }
            _ => {}
        }

        // Stalls count for Auto alone, and for the item that plays now: a
        // stall under a manual limit must not step Auto down later.
        let item = self.playing.as_ref().map_or("", |i| i.id.as_str());
        if state.item != item {
            state.item = item.to_string();
            state.stepper.clear();
        }
        if manual {
            state.stepper.clear();
        }

        let mut reload: Option<String> = None;
        if playing && !self.scrubbing && !manual && !status.buffering {
            let resolved = self.playing.as_ref().and_then(|item| stream::current_of(&item.id));
            let playing_bps = resolved
                .as_ref()
                .and_then(|r| r.target.as_ref().map(|t| t.bitrate).or(r.source_bitrate));
            let steps = state.stepper.step(now, in_group);
            if steps > 0 {
                match playing_bps.and_then(|bps| step_down(bps, steps)) {
                    Some(cap) => {
                        state.stepped = Some((cap, now));
                        state.wanted = Some("stalls");
                        let mut text = format!("Slow connection: switched to {}", bitrate_label(cap));
                        if in_group {
                            text.push_str(". The group waits while this player loads.");
                        }
                        reload = Some(text);
                    }
                    None => log::info!("link: stalls at the lowest rung; nothing lower"),
                }
            } else if let Some((cap, at)) = state.stepped
                && !in_group
                && state.stepper.hold_off_left(now, in_group).is_zero()
                && state.link.is_some_and(|link| link.at > at)
                && state
                    .last_stall_end
                    .is_none_or(|end| now.duration_since(end.max(at)) >= RECOVERY)
            {
                // The link was measured again after the step down, and
                // the player kept up for a long time: what fits now?
                let file = resolved.as_ref().and_then(|r| r.source_bitrate);
                let codec = resolved
                    .as_ref()
                    .and_then(|r| r.video().and_then(|v| v.codec.clone()));
                let (choice, _) = decide(state.measured(), file, codec.as_deref(), state.choice);
                if choice.cap().is_none_or(|c| c > cap) {
                    state.stepped = None;
                    state.stepper.changed(now);
                    reload = Some(format!("Connection recovered: Auto · {}", choice.label()));
                }
            }
        }

        // A measurement needs the link for itself: never while the video
        // plays. Between items, a stale value is renewed for the next one.
        let closed_now = state.was_open && !self.player_open;
        state.was_open = self.player_open;
        let free = !self.player_open || (status.paused && !status.buffering);
        let stale = state.link.is_none_or(|link| link.at.elapsed() > FRESH);
        let due = match state.wanted {
            Some(_) => free,
            None => closed_now && stale,
        };
        let why = state.wanted.unwrap_or("between items");
        drop(state);

        if let Some(text) = reload {
            log::info!("link: {text}");
            STATE.lock().unwrap().last_toast = text.clone();
            self.stream_reload(None, None);
            self.toast("Quality", text, cx);
            self.rebuild_track_menus(cx);
        }
        if due {
            self.adaptive_measure(why, cx);
        }
    }

    /// The verbs of `quality` that belong here: `measure`,
    /// `fake-speed <kbps|off>`, `fake-stall <n>`.
    pub fn debug_adaptive(&mut self, verb: &str, arg: &str, cx: &mut Context<Self>) -> String {
        match verb {
            "measure" => {
                if self.adaptive_measure("debug", cx) {
                    "measuring".into()
                } else {
                    "error: no session, or a measurement runs".into()
                }
            }
            "fake-speed" => {
                let fake = match arg {
                    "off" | "" => None,
                    kbps => match kbps.parse::<u64>() {
                        Ok(kbps) => Some(kbps * 1000),
                        Err(_) => return "error: usage: quality fake-speed <kbps|off>".into(),
                    },
                };
                STATE.lock().unwrap().fake = fake;
                format!("fake={}", arg)
            }
            "fake-stall" => {
                let Ok(n) = arg.parse::<usize>() else {
                    return "error: usage: quality fake-stall <n>".into();
                };
                let now = Instant::now();
                {
                    let mut state = STATE.lock().unwrap();
                    for _ in 0..n {
                        state.on_stall(now - FAKE_STALL, FAKE_STALL);
                    }
                }
                self.adaptive_tick(cx);
                describe(self.sync.in_group())
            }
            // The switch of Settings > Playback, as the checkbox sets it.
            "adaptive" => {
                let on = match arg {
                    "on" => true,
                    "off" => false,
                    _ => return "error: usage: quality adaptive <on|off>".into(),
                };
                if self.config.adaptive_quality.unwrap_or(DEFAULT_ON) != on {
                    self.toggle_adaptive_quality(cx);
                }
                format!("adaptive={on}")
            }
            // For a screenshot of a test instance that sits under other
            // windows: brings its window over them without taking the
            // focus, and puts it back.
            "window-front" | "window-back" => {
                use crate::macos::{Id, class, send};
                let app = send!(Id, class(c"NSApplication"), c"sharedApplication");
                let windows = send!(Id, app, c"windows");
                let count = send!(usize, windows, c"count");
                for i in 0..count {
                    let window = send!(Id, windows, c"objectAtIndex:", i => usize);
                    if verb == "window-front" {
                        send!((), window, c"orderFront:", std::ptr::null_mut() => Id);
                    } else {
                        send!((), window, c"orderBack:", std::ptr::null_mut() => Id);
                    }
                }
                verb.to_string()
            }
            _ => "error: quality state|set <kbps|auto>|menu|info|quit|measure|fake-speed <kbps|off>|fake-stall <n>|adaptive <on|off>|window-front|window-back".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: u64 = 1_000_000;

    #[test]
    fn the_cap_of_the_stalls_holds_until_a_newer_measurement() {
        let file = Some(9_900_000);
        {
            let mut state = STATE.lock().unwrap();
            *state = Adaptive::new();
            state.enabled = true;
            state.fake = None;
            state.link = Some(Link { bps: 50 * M, at: Instant::now(), bytes: 0, from: Source::Measured });
        }
        assert_eq!(auto_cap(file, Some("h264")), None);
        assert_eq!(auto_label(), "Original");
        // Stalls set a cap: Auto sends it although the link looks fast.
        let stepped_at = Instant::now() + Duration::from_secs(1);
        STATE.lock().unwrap().stepped = Some((8 * M, stepped_at));
        assert_eq!(auto_cap(file, Some("h264")), Some(8 * M));
        assert_eq!(auto_label(), "8 Mbps");
        // A measurement after the step lifts it.
        STATE.lock().unwrap().link = Some(Link {
            bps: 50 * M,
            at: stepped_at + Duration::from_secs(60),
            bytes: 0,
            from: Source::Measured,
        });
        assert_eq!(auto_cap(file, Some("h264")), None);
        assert!(STATE.lock().unwrap().stepped.is_none());
        // The switch off is the file, whatever the link.
        STATE.lock().unwrap().fake = Some(3 * M);
        assert_eq!(auto_cap(file, Some("h264")), Some(1_500_000));
        STATE.lock().unwrap().enabled = false;
        assert_eq!(auto_cap(file, Some("h264")), None);
        *STATE.lock().unwrap() = Adaptive::new();
    }
}
