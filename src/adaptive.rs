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
    stream::{self, LADDER, Rung, bitrate_label},
};

// ----- the numbers ---------------------------------------------------------------

/// Whether Auto lowers the quality on a slow connection when the user has
/// not chosen. Off: the app plays the file as it is whenever that is
/// possible, and does not compress by default.
pub const DEFAULT_ON: bool = false;

/// The share of the measured speed the player may count on; the rest is
/// for the overhead of the network and its ups and downs. The web client
/// takes the same share.
const SAFETY: f64 = 0.7;
/// A step up needs this much more room than a step down, so the choice
/// does not flip at the edge.
const UP_MARGIN: f64 = 1.25;
/// A measurement counts as fresh for this long.
const FRESH: Duration = Duration::from_secs(10 * 60);
/// A value kept from an earlier run counts for this long.
const KEPT_FOR: u64 = 24 * 3600;
/// The test downloads: the bytes asked for, and the speed the next one
/// needs. A slow link stops after the first; the whole run stays under
/// a few MB (the server rounds a body up to the next power of two).
const STEPS: [(u64, u64); 3] = [
    (500_000, 1_000_000),
    (1_000_000, 20_000_000),
    (2_000_000, u64::MAX),
];
/// One test download may take this long; what came in by then counts.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
/// Stalls count inside this window.
const WINDOW: Duration = Duration::from_secs(60);
/// A pause for data shorter than this is a hiccup, not a stall.
const MIN_STALL: Duration = Duration::from_secs(1);
/// Stalls in the window, or time stalled in it, that step the quality down.
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
/// Without a stall for this long, and with a measurement newer than the
/// step down, the quality goes back up while the item plays.
const RECOVERY: Duration = Duration::from_secs(10 * 60);
/// The length counted for a stall the debug channel feeds.
const FAKE_STALL: Duration = Duration::from_secs(3);
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
    let rungs = stream::ladder(Some(file), codec);
    let fits_direct = usable >= file;
    let wanted = if fits_direct { Choice::Direct } else { best_rung(&rungs, usable) };
    match previous {
        Some(Choice::Direct) if fits_direct => (Choice::Direct, "the link carries the file"),
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

impl Client {
    /// Measures the link to the server. Downloads a few MB at most.
    pub fn measure_link(&self) -> Result<Measurement> {
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

    pub fn stall(&mut self, at: Instant, length: Duration) {
        self.stalls.push((at, length));
    }

    fn prune(&mut self, now: Instant) {
        self.stalls.retain(|(at, _)| now.saturating_duration_since(*at) < WINDOW);
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

// ----- the state --------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Link {
    bps: u64,
    at: Instant,
    bytes: u64,
    /// "measured", "kept" (from the config), "fake" (the debug channel).
    from: &'static str,
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
    /// The pause for data that runs now, and whether it counted already.
    buffering_since: Option<(Instant, bool)>,
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
    state.enabled = config.adaptive_quality.unwrap_or(crate::adaptive::DEFAULT_ON);
    if let (Some(bps), Some(wall)) = (config.link_bps, config.link_measured_at) {
        let age = unix_now().saturating_sub(wall);
        if age <= KEPT_FOR {
            let at = Instant::now()
                .checked_sub(Duration::from_secs(age))
                .unwrap_or_else(Instant::now);
            state.link = Some(Link { bps, at, bytes: 0, from: "kept" });
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

/// "Original" or the rung, for the Auto entry of the Quality menu.
pub fn auto_label() -> String {
    let state = STATE.lock().unwrap();
    match state.choice {
        Some(choice) if state.enabled => choice.label(),
        _ => "Original".to_string(),
    }
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
            let from = if link.from == "kept" { " · from the last run" } else { "" };
            format!("{} · {when}{from}", bitrate_label(link.bps))
        }
        None if state.measuring => "measuring".to_string(),
        None => "not measured".to_string(),
    }
}

/// One line for the debug channel.
pub fn describe() -> String {
    let mut state = STATE.lock().unwrap();
    let now = Instant::now();
    let link = match state.link {
        Some(link) => format!(
            "link={} link_age={} link_bytes={} link_from={}",
            link.bps / 1000,
            link.at.elapsed().as_secs(),
            link.bytes,
            link.from
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
    let in_group = false;
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
        let on = !self.config.adaptive_quality.unwrap_or(crate::adaptive::DEFAULT_ON);
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
                            from: "measured",
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
        let now = Instant::now();
        let status = &self.player_status;
        let playing = self.player_open && status.state == PlayState::Playing;
        let in_group = self.sync.in_group();
        let manual = stream::max_bitrate().is_some();
        let mut state = STATE.lock().unwrap();

        // After a sleep the network is often another one.
        match state.clock {
            Some((mono, wall)) => {
                let wall_passed = SystemTime::now()
                    .duration_since(wall)
                    .unwrap_or(Duration::ZERO);
                if wall_passed.saturating_sub(mono.elapsed()) > SLEEP_GAP {
                    log::info!("link: the machine slept; a measurement is due");
                    state.wanted = Some("the machine slept");
                    state.stepper.clear();
                    state.buffering_since = None;
                    state.clock = Some((now, SystemTime::now()));
                }
            }
            None => state.clock = Some((now, SystemTime::now())),
        }

        // A pause for data counts at its end, or at once when it lasts.
        match (playing && status.buffering, state.buffering_since) {
            (true, None) => state.buffering_since = Some((now, false)),
            (true, Some((since, false))) if now.duration_since(since) >= STALLED_DOWN => {
                state.on_stall(since, now.duration_since(since));
                state.buffering_since = Some((since, true));
            }
            (false, Some((since, counted))) => {
                let length = now.duration_since(since);
                if !counted && length >= MIN_STALL {
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
        if manual || !state.enabled {
            state.stepper.clear();
        }

        let mut reload: Option<String> = None;
        if playing && !self.scrubbing && !manual && state.enabled && !status.buffering {
            let resolved = stream::current();
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
        if due && !self.session.is_none() {
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
                describe()
            }
            // The switch of Settings > Playback, as the checkbox sets it.
            "adaptive" => {
                let on = match arg {
                    "on" => true,
                    "off" => false,
                    _ => return "error: usage: quality adaptive <on|off>".into(),
                };
                if self.config.adaptive_quality.unwrap_or(crate::adaptive::DEFAULT_ON) != on {
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

    #[test]
    fn the_cap_of_the_stalls_holds_until_a_newer_measurement() {
        let file = Some(9_900_000);
        {
            let mut state = STATE.lock().unwrap();
            *state = Adaptive::new();
            state.enabled = true;
            state.fake = None;
            state.link = Some(Link { bps: 50 * M, at: Instant::now(), bytes: 0, from: "measured" });
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
            from: "measured",
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
