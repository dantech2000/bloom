// SPDX-License-Identifier: AGPL-3.0-or-later
//! Subtitles in the player: the timing offset of the subtitle and of the
//! audio (mpv `sub-delay` and `audio-delay`), and the search for subtitles
//! on the server's providers with a download into the library.
//!
//! A delay is for one item. The mpv worker resets it when another item
//! loads (`src/player.rs`) and keeps it when the same item loads again, as
//! a change of the quality does.
//!
//! The server calls (`SubtitleController`): `GET /Items/{id}/RemoteSearch/
//! Subtitles/{language}` lists candidates, `POST /Items/{id}/RemoteSearch/
//! Subtitles/{subtitleId}` downloads one next to the media, and `DELETE
//! /Videos/{id}/Subtitles/{index}` removes an external subtitle. The first
//! two need the user policy `EnableSubtitleManagement`, the last one an
//! administrator. A search finds nothing without a subtitle provider
//! plugin on the server.

use std::{collections::HashMap, time::Duration};

use anyhow::{Result, anyhow};
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px, rgba,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    app::Bloom,
    jellyfin::Client,
    ui::{glass::glass, menu::MenuItem, scroll_area::ScrollArea, theme::UiTheme, tip::tip},
    views::cards::icon,
};

/// Width of the panel.
const PANEL_W: f32 = 520.;
/// A step of the keys, in milliseconds.
const KEY_STEP_MS: i64 = 100;
/// The delay stays within this many seconds, as a typing slip would not
/// push a subtitle out of the film.
const MAX_DELAY_SECS: f64 = 600.;

// ----- the timing offset -----------------------------------------------------

/// "0 ms", "+250 ms", "-1.5 s" for a delay in seconds.
pub fn delay_label(secs: f64) -> String {
    let ms = (secs * 1000.).round() as i64;
    if ms == 0 {
        "0 ms".to_string()
    } else if ms.abs() >= 10_000 {
        format!("{:+.1} s", ms as f64 / 1000.)
    } else {
        format!("{ms:+} ms")
    }
}

/// The delay in seconds after a step of `delta_ms` from `current`.
pub fn stepped(current: f64, delta_ms: i64) -> f64 {
    let ms = (current * 1000.).round() as i64 + delta_ms;
    (ms as f64 / 1000.).clamp(-MAX_DELAY_SECS, MAX_DELAY_SECS)
}

/// The text of a step in the menu: "+100 ms".
fn step_label(delta_ms: i64) -> String {
    format!("{delta_ms:+} ms")
}

/// Which delay: of the subtitle or of the audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delay {
    Subtitle,
    Audio,
}

impl Delay {
    fn property(self) -> &'static str {
        match self {
            Delay::Subtitle => "sub-delay",
            Delay::Audio => "audio-delay",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Delay::Subtitle => "Subtitles",
            Delay::Audio => "Audio",
        }
    }
}

/// Why a delay cannot change now; none when it can.
///
/// A subtitle burned into the video is part of the picture, so mpv cannot
/// move it. The audio delay is allowed in a SyncPlay group: mpv's
/// `audio-pts`, which the group measures, moves with the delay, so the
/// worker adds the delay back to the sample (`player.rs`).
pub fn blocked(delay: Delay, burned: bool) -> Option<&'static str> {
    match delay {
        Delay::Subtitle if burned => Some("The subtitle is burned into the video; it cannot move."),
        _ => None,
    }
}

// ----- candidates ----------------------------------------------------------------

/// One subtitle the server found at a provider.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Candidate {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub download_count: Option<i64>,
    #[serde(default)]
    pub community_rating: Option<f32>,
    #[serde(default)]
    pub is_hash_match: Option<bool>,
    #[serde(rename = "ThreeLetterISOLanguageName", default)]
    pub language: String,
    #[serde(default)]
    pub ai_translated: Option<bool>,
    #[serde(default)]
    pub machine_translated: Option<bool>,
    #[serde(default)]
    pub forced: Option<bool>,
    #[serde(default)]
    pub hearing_impaired: Option<bool>,
}

impl Candidate {
    /// The quiet line under the name: provider, format, downloads, rating.
    pub fn detail(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.provider_name.is_empty() {
            parts.push(self.provider_name.clone());
        }
        if !self.format.is_empty() {
            parts.push(self.format.to_uppercase());
        }
        if let Some(count) = self.download_count.filter(|c| *c > 0) {
            parts.push(format!("{} downloads", thousands(count)));
        }
        if let Some(rating) = self.community_rating.filter(|r| *r > 0.) {
            parts.push(format!("rating {rating:.1}"));
        }
        if !self.author.is_empty() {
            parts.push(format!("by {}", self.author));
        }
        parts.join(" · ")
    }

    /// The small labels on a row, in a fixed order.
    pub fn badges(&self) -> Vec<&'static str> {
        let mut badges = Vec::new();
        if self.is_hash_match == Some(true) {
            badges.push("Hash match");
        }
        if self.hearing_impaired == Some(true) {
            badges.push("Hearing impaired");
        }
        if self.forced == Some(true) {
            badges.push("Forced");
        }
        if self.machine_translated == Some(true) || self.ai_translated == Some(true) {
            badges.push("Machine translated");
        }
        badges
    }
}

/// 12345 as "12,345".
fn thousands(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut out = String::new();
    for (n, c) in digits.chars().enumerate() {
        if n > 0 && (digits.len() - n) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0 { format!("-{out}") } else { out }
}

/// The candidates of a search answer. A row that is not an object is left
/// out; the best match is first: a hash match, then the downloads.
pub fn parse_candidates(body: &Value) -> Vec<Candidate> {
    let mut list: Vec<Candidate> = body
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| serde_json::from_value(row.clone()).ok())
        .filter(|c: &Candidate| !c.id.is_empty())
        .collect();
    list.sort_by_key(|c| {
        (
            std::cmp::Reverse(c.is_hash_match == Some(true)),
            std::cmp::Reverse(c.download_count.unwrap_or(0)),
        )
    });
    list
}

/// What an empty search means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Nothing {
    /// The server lists no subtitle provider.
    NoProvider,
    /// There are providers, or the app could not ask; nothing matched.
    NotFound { providers_unknown: bool },
}

/// The rule for an empty list. `providers` is the number of subtitle
/// fetchers the server lists (`/Libraries/AvailableOptions`), or none when
/// the app could not read it (the call needs more rights than the search).
pub fn nothing_found(providers: Option<usize>) -> Nothing {
    match providers {
        Some(0) => Nothing::NoProvider,
        Some(_) => Nothing::NotFound { providers_unknown: false },
        None => Nothing::NotFound { providers_unknown: true },
    }
}

impl Nothing {
    pub fn message(&self) -> &'static str {
        match self {
            Nothing::NoProvider => {
                "The server has no subtitle provider. Install one (for example Open Subtitles) in the dashboard."
            }
            Nothing::NotFound { .. } => "No subtitles found",
        }
    }

    /// A quiet second line, when the cause is not certain.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Nothing::NotFound { providers_unknown: true } => Some(
                "The search needs a subtitle provider on the server, such as Open Subtitles.",
            ),
            _ => None,
        }
    }
}

// ----- languages -----------------------------------------------------------------

/// The languages of the chips: three-letter code and name.
const COMMON: [(&str, &str); 14] = [
    ("eng", "English"),
    ("spa", "Spanish"),
    ("fra", "French"),
    ("deu", "German"),
    ("ita", "Italian"),
    ("por", "Portuguese"),
    ("nld", "Dutch"),
    ("rus", "Russian"),
    ("jpn", "Japanese"),
    ("kor", "Korean"),
    ("zho", "Chinese"),
    ("ara", "Arabic"),
    ("pol", "Polish"),
    ("tur", "Turkish"),
];

/// The name of a language code; the code itself when it is not in the list.
pub fn language_name(code: &str) -> String {
    COMMON
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(code))
        .map_or_else(|| code.to_uppercase(), |(_, name)| name.to_string())
}

/// The language the search starts with: the preference of the user when it
/// is a three-letter code, else English.
pub fn default_language(preference: Option<&str>) -> String {
    preference
        .map(str::trim)
        .filter(|p| p.len() == 3 && p.chars().all(|c| c.is_ascii_alphabetic()))
        .map_or_else(|| "eng".to_string(), str::to_ascii_lowercase)
}

/// The chips: the language of the user first when it is not a common one,
/// then the common ones.
pub fn language_choices(preference: Option<&str>) -> Vec<String> {
    let mut list: Vec<String> = COMMON.iter().map(|(c, _)| c.to_string()).collect();
    let own = default_language(preference);
    if !list.contains(&own) {
        list.insert(0, own);
    }
    list
}

// ----- the requests ----------------------------------------------------------------

/// A value in the path of a request.
fn segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn search_path(item_id: &str, language: &str) -> String {
    format!("/Items/{}/RemoteSearch/Subtitles/{}", segment(item_id), segment(language))
}

pub fn download_path(item_id: &str, subtitle_id: &str) -> String {
    format!("/Items/{}/RemoteSearch/Subtitles/{}", segment(item_id), segment(subtitle_id))
}

pub fn remove_path(item_id: &str, index: i64) -> String {
    format!("/Videos/{}/Subtitles/{index}", segment(item_id))
}

/// A subtitle stream of an item, as the server lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct SubStream {
    pub index: i64,
    pub external: bool,
    pub path: String,
    pub language: String,
    pub title: String,
}

/// The subtitle streams in the JSON of an item.
pub fn parse_streams(item: &Value) -> Vec<SubStream> {
    let text = |s: &Value, key: &str| s.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    item.get("MediaStreams")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.get("Type").and_then(Value::as_str) == Some("Subtitle"))
        .map(|s| SubStream {
            index: s.get("Index").and_then(Value::as_i64).unwrap_or(-1),
            external: s.get("IsExternal").and_then(Value::as_bool).unwrap_or(false),
            path: text(s, "Path"),
            language: text(s, "Language"),
            title: text(s, "DisplayTitle"),
        })
        .collect()
}

/// The stream that `after` has and `before` has not: an external one with a
/// path that was not there. The indexes of the external streams can move
/// when one is added, so the path names a stream.
pub fn new_stream<'a>(before: &[SubStream], after: &'a [SubStream]) -> Option<&'a SubStream> {
    after
        .iter()
        .filter(|s| s.external)
        .find(|s| !before.iter().any(|b| b.external && b.path == s.path && !s.path.is_empty()))
        .filter(|_| after.iter().filter(|a| a.external).count() > before.iter().filter(|b| b.external).count())
}

impl Client {
    fn subtitle_streams(&self, item_id: &str) -> Result<Vec<SubStream>> {
        let user = self.user()?.to_string();
        let item: Value = self.get(&format!("/Items/{}", segment(item_id)), &[("userId", user)])?;
        Ok(parse_streams(&item))
    }

    /// The rights of the user: may manage subtitles, is an administrator,
    /// and the subtitle language of the user.
    fn subtitle_rights(&self) -> Result<Rights> {
        let me: Value = self.get("/Users/Me", &[])?;
        let flag = |name: &str| {
            me.pointer(&format!("/Policy/{name}")).and_then(Value::as_bool).unwrap_or(false)
        };
        let admin = flag("IsAdministrator");
        Ok(Rights {
            user: me.get("Id").and_then(Value::as_str).unwrap_or_default().to_string(),
            manage: admin || flag("EnableSubtitleManagement"),
            admin,
            language: me
                .pointer("/Configuration/SubtitleLanguagePreference")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    /// The number of subtitle providers of the server; none when the call
    /// is not allowed.
    fn subtitle_providers(&self) -> Option<usize> {
        let options: Value = self.get("/Libraries/AvailableOptions", &[]).ok()?;
        options.get("SubtitleFetchers").and_then(Value::as_array).map(Vec::len)
    }

    /// Searches the providers of the server.
    pub fn search_subtitles(&self, item_id: &str, language: &str) -> Result<Search> {
        let body: Value = self.get(
            &search_path(item_id, language),
            &[("isPerfectMatch", "false".to_string())],
        )?;
        let candidates = parse_candidates(&body);
        let nothing = candidates.is_empty().then(|| nothing_found(self.subtitle_providers()));
        Ok(Search { candidates, nothing })
    }

    /// Downloads a candidate into the library of the server and waits for
    /// the new stream: the server lists it after it refreshes the item.
    /// None when the stream did not appear in time.
    pub fn download_subtitle(&self, item_id: &str, subtitle_id: &str) -> Result<Option<SubStream>> {
        let before = self.subtitle_streams(item_id)?;
        self.call("POST", &download_path(item_id, subtitle_id), &[])?;
        for _ in 0..16 {
            std::thread::sleep(Duration::from_millis(1500));
            let after = self.subtitle_streams(item_id)?;
            if let Some(found) = new_stream(&before, &after) {
                return Ok(Some(found.clone()));
            }
        }
        Ok(None)
    }

    /// Removes an external subtitle of an item. Refuses a stream that is
    /// not external.
    pub fn remove_subtitle(&self, item_id: &str, index: i64) -> Result<()> {
        let streams = self.subtitle_streams(item_id)?;
        match streams.iter().find(|s| s.index == index) {
            None => return Err(anyhow!("the item has no subtitle stream {index}")),
            Some(s) if !s.external => {
                return Err(anyhow!("stream {index} is not an external subtitle"));
            }
            Some(_) => {}
        }
        self.call("DELETE", &remove_path(item_id, index), &[])
    }
}

/// The rights of the user for subtitles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rights {
    pub user: String,
    pub manage: bool,
    pub admin: bool,
    pub language: Option<String>,
}

/// The answer of a search.
#[derive(Clone, Debug, PartialEq)]
pub struct Search {
    pub candidates: Vec<Candidate>,
    pub nothing: Option<Nothing>,
}

// ----- the state of the panel ------------------------------------------------------

/// What a row of the panel shows of its download.
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    Working,
    Done,
    Failed(String),
}

#[derive(Default)]
pub struct State {
    /// Asked for: the rights of the user, for the signed-in user.
    rights: Option<Rights>,
    rights_asked: Option<String>,
    pub panel_open: bool,
    /// The item of the panel.
    pub item_id: String,
    pub item_title: String,
    pub language: String,
    pub results: Vec<Candidate>,
    pub loading: bool,
    pub nothing: Option<Nothing>,
    pub error: Option<String>,
    pub progress: HashMap<String, Progress>,
    /// Counts the searches, so a late answer of an older one is dropped.
    generation: u64,
    /// The delays the menus show.
    seen: (f64, f64),
}

impl State {
    pub fn allowed(&self) -> bool {
        self.rights.as_ref().is_some_and(|r| r.manage)
    }
}

// ----- the app ---------------------------------------------------------------------

impl Bloom {
    /// True while the item that plays has a subtitle burned in.
    fn subs_burned(&self) -> bool {
        crate::stream::current().is_some_and(|r| r.burned_subtitle.is_some())
    }

    /// Reads the rights of the user once per user, for the menu entry.
    pub fn subs_load_rights(&mut self, cx: &mut Context<Self>) {
        let Some(user) = self.session.as_ref().map(|s| s.user_id.clone()) else { return };
        if self.subs.rights_asked.as_deref() == Some(user.as_str()) {
            return;
        }
        self.subs.rights_asked = Some(user.clone());
        self.subs.rights = None;
        self.fetch(
            cx,
            |client| client.subtitle_rights(),
            move |this, result, cx| {
                if this.session.as_ref().map(|s| &s.user_id) != Some(&user) {
                    return;
                }
                match result {
                    Ok(rights) => {
                        this.subs.rights = Some(rights);
                        this.rebuild_track_menus(cx);
                        if matches!(this.page, crate::app::Page::Detail(_)) {
                            this.rebuild_detail_menus(cx);
                        }
                        cx.notify();
                    }
                    Err(err) => log::warn!("subtitle rights: {err:#}"),
                }
            },
        );
    }

    /// The "Timing" entry of the subtitle menu or the "Audio delay" entry
    /// of the audio menu: the delay now, and the steps.
    pub fn timing_menu_item(&self, delay: Delay, cx: &mut Context<Self>) -> MenuItem {
        let (id, label) = match delay {
            Delay::Subtitle => ("subs.timing", "Timing"),
            Delay::Audio => ("audio.delay", "Audio delay"),
        };
        if let Some(reason) = blocked(delay, self.subs_burned()) {
            return MenuItem::new(id, label).detail(reason).disabled(true);
        }
        let now = match delay {
            Delay::Subtitle => self.player_status.sub_delay,
            Delay::Audio => self.player_status.audio_delay,
        };
        let this = cx.weak_entity();
        let mut steps = Vec::new();
        for step in [-500, -100, 100, 500] {
            let handle = this.clone();
            steps.push(
                MenuItem::new(SharedString::from(format!("{id}.{step}")), step_label(step))
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |t, cx| t.subs_step(delay, step, cx)).ok();
                    }),
            );
        }
        let handle = this.clone();
        steps.push(MenuItem::separator());
        steps.push(
            MenuItem::new(SharedString::from(format!("{id}.reset")), "Reset")
                .disabled(now == 0.)
                .on_click(move |_, _, cx| {
                    handle.update(cx, |t, cx| t.subs_set_delay(delay, 0., cx)).ok();
                }),
        );
        MenuItem::submenu(id, label, steps).detail(delay_label(now))
    }

    /// A step of the delay, from a key or the menu.
    pub fn subs_step(&mut self, delay: Delay, delta_ms: i64, cx: &mut Context<Self>) {
        let now = match delay {
            Delay::Subtitle => self.player_status.sub_delay,
            Delay::Audio => self.player_status.audio_delay,
        };
        self.subs_set_delay(delay, stepped(now, delta_ms), cx);
    }

    /// Sets a delay in seconds and shows it. Returns false when it cannot
    /// change (the debug command says why).
    pub fn subs_set_delay(&mut self, delay: Delay, secs: f64, cx: &mut Context<Self>) -> bool {
        if !self.player_open {
            return false;
        }
        if let Some(reason) = blocked(delay, self.subs_burned()) {
            self.subs_toast("Subtitle timing", reason, cx);
            return false;
        }
        let secs = secs.clamp(-MAX_DELAY_SECS, MAX_DELAY_SECS);
        self.player.set_property(delay.property(), &format!("{secs:.3}"));
        self.subs_toast(&format!("{} {}", delay.name(), delay_label(secs)), "", cx);
        true
    }

    /// The toast of the player for a timing change: one at a time, so a key
    /// held down replaces it.
    fn subs_toast(&self, title: &str, description: &str, cx: &mut Context<Self>) {
        let (title, description) = (title.to_string(), description.to_string());
        self.toasts.update(cx, |toasts, cx| {
            toasts.push("subs-delay", title, description, Some(Duration::from_secs(2)), cx);
        });
    }

    /// The keys of the player for the delays: Z and Shift+Z for the
    /// subtitle, Ctrl+minus and Ctrl+plus for the audio (as mpv has them).
    /// The keys of the plugin come first. True when the key was handled.
    pub fn subs_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let m = event.keystroke.modifiers;
        if m.platform || m.alt || m.function {
            return false;
        }
        let key = event.keystroke.key.as_str();
        let (delay, delta) = match (key, m.control, m.shift) {
            ("z", false, false) => (Delay::Subtitle, -KEY_STEP_MS),
            ("z", false, true) => (Delay::Subtitle, KEY_STEP_MS),
            ("-", true, _) => (Delay::Audio, -KEY_STEP_MS),
            ("=" | "+" | "plus", true, _) => (Delay::Audio, KEY_STEP_MS),
            _ => return false,
        };
        self.subs_step(delay, delta, cx);
        true
    }

    /// Called after each status poll: the menus show the delay mpv reports.
    pub fn subs_status_changed(&mut self, cx: &mut Context<Self>) {
        let seen = (self.player_status.sub_delay, self.player_status.audio_delay);
        if seen != self.subs.seen {
            self.subs.seen = seen;
            self.rebuild_track_menus(cx);
        }
    }

    /// The entry "Search for subtitles…" for the menu of an item; none
    /// when the user may not manage subtitles.
    pub fn subs_search_item(&self, item_id: &str, title: &str, cx: &mut Context<Self>) -> Option<MenuItem> {
        if !self.subs.allowed() {
            return None;
        }
        let (this, id, title) = (cx.weak_entity(), item_id.to_string(), title.to_string());
        Some(
            MenuItem::new("subs.search", "Search for subtitles…")
                .icon(LucideIcon::Search)
                .on_click(move |_, window, cx| {
                    let (this, id, title) = (this.clone(), id.clone(), title.clone());
                    // After the menu has closed.
                    window.defer(cx, move |window, cx| {
                        this.update(cx, |t, cx| t.open_subs_panel(&id, &title, window, cx)).ok();
                    });
                }),
        )
    }

    /// The title and id of the item that plays.
    pub fn subs_playing(&self) -> Option<(String, String)> {
        self.playing.as_ref().map(|i| (i.id.clone(), i.display_title()))
    }

    pub fn open_subs_panel(&mut self, item_id: &str, title: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popups(None, window, cx);
        let same = self.subs.item_id == item_id;
        self.subs.panel_open = true;
        self.subs.item_id = item_id.to_string();
        self.subs.item_title = title.to_string();
        if !same || self.subs.language.is_empty() {
            let preference = self.subs.rights.as_ref().and_then(|r| r.language.clone());
            self.subs.language = default_language(preference.as_deref());
            self.subs.results.clear();
            self.subs.progress.clear();
        }
        self.subs_search(cx);
        cx.notify();
    }

    /// Closes the panel; called with the other popups.
    pub fn close_subs_panel(&mut self) -> bool {
        std::mem::take(&mut self.subs.panel_open)
    }

    fn subs_search(&mut self, cx: &mut Context<Self>) {
        self.subs.generation += 1;
        let generation = self.subs.generation;
        self.subs.loading = true;
        self.subs.error = None;
        self.subs.nothing = None;
        let (id, language) = (self.subs.item_id.clone(), self.subs.language.clone());
        self.fetch(
            cx,
            move |client| client.search_subtitles(&id, &language),
            move |this, result, cx| {
                if this.subs.generation != generation {
                    return;
                }
                this.subs.loading = false;
                match result {
                    Ok(search) => {
                        this.subs.results = search.candidates;
                        this.subs.nothing = search.nothing;
                    }
                    Err(err) => {
                        this.subs.results.clear();
                        this.subs.error = Some(format!("{err:#}"));
                    }
                }
                cx.notify();
            },
        );
    }

    /// Downloads a candidate of the panel. When it is listed, the item
    /// refreshes: the detail page reloads, and a player that plays the
    /// item loads it again (same position) with the new subtitle shown.
    pub fn subs_download(&mut self, subtitle_id: &str, cx: &mut Context<Self>) {
        if matches!(self.subs.progress.get(subtitle_id), Some(Progress::Working)) {
            return;
        }
        self.subs.progress.insert(subtitle_id.to_string(), Progress::Working);
        let (item_id, sid) = (self.subs.item_id.clone(), subtitle_id.to_string());
        let (work_item, work_sid) = (item_id.clone(), sid.clone());
        self.fetch(
            cx,
            move |client| client.download_subtitle(&work_item, &work_sid),
            move |this, result, cx| {
                match result {
                    Ok(found) => {
                        this.subs.progress.insert(sid.clone(), Progress::Done);
                        this.subs_downloaded(&item_id, found, cx);
                    }
                    Err(err) => {
                        this.subs.progress.insert(sid.clone(), Progress::Failed(format!("{err:#}")));
                        this.toast("Subtitle download failed", format!("{err:#}"), cx);
                    }
                }
                cx.notify();
            },
        );
    }

    fn subs_downloaded(&mut self, item_id: &str, found: Option<SubStream>, cx: &mut Context<Self>) {
        let Some(found) = found else {
            self.toast(
                "Subtitle downloaded",
                "The server has not listed it yet. Refresh the page in a moment.",
                cx,
            );
            return;
        };
        let name = if found.title.is_empty() { language_name(&found.language) } else { found.title.clone() };
        self.toast("Subtitle added", name, cx);
        let on_page = matches!(&self.page, crate::app::Page::Detail(d) if d.item.id == item_id);
        if on_page {
            self.load_page(cx);
        }
        let playing = self.playing.as_ref().is_some_and(|i| i.id == item_id);
        if self.player_open && playing {
            self.stream_reload(None, Some(found.index));
        }
    }

    /// The panel: language chips and the results.
    pub fn render_subs_panel(&self, top: Option<f32>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.subs.panel_open {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let soft = rgba(0xf5f5f7b3);
        fn swallow(_: &mut Bloom, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Bloom>) {
            cx.stop_propagation();
        }
        let state = &self.subs;
        let pill = |text: String| {
            div()
                .px(px(8.))
                .py(px(1.))
                .rounded(px(8.))
                .bg(rgba(0xffffff1f))
                .text_size(px(11.))
                .text_color(soft)
                .child(text)
        };

        let preference = state.rights.as_ref().and_then(|r| r.language.clone());
        let mut chips = div().flex().flex_wrap().gap(px(6.)).px(px(12.));
        for code in language_choices(preference.as_deref()) {
            let selected = code == state.language;
            let pick = code.clone();
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("subs.lang.{code}")))
                    .px(px(10.))
                    .h(px(28.))
                    .rounded(px(14.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .when(selected, |el| {
                        el.bg(rgba(0xf5f5f7e6)).text_color(rgba(0x1c1c1eff))
                    })
                    .when(!selected, |el| {
                        el.bg(rgba(0xffffff1f)).text_color(t.colors.foreground).hover(|s| s.bg(rgba(0xffffff33)))
                    })
                    .child(language_name(&code))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if this.subs.language != pick {
                            this.subs.language = pick.clone();
                            this.subs_search(cx);
                            cx.notify();
                        }
                    })),
            );
        }

        let quiet = |text: &str| {
            div().px(px(12.)).py(px(10.)).text_size(px(14.)).text_color(soft).child(text.to_string())
        };
        let mut list = div().flex().flex_col().gap(px(2.));
        if state.loading {
            list = list.child(quiet("Searching…"));
        } else if let Some(error) = &state.error {
            list = list.child(quiet(&format!("The search failed: {error}")));
        } else if state.results.is_empty() {
            let nothing = state.nothing.clone().unwrap_or(Nothing::NotFound { providers_unknown: true });
            list = list.child(quiet(nothing.message()));
            if let Some(hint) = nothing.hint() {
                list = list.child(
                    div().px(px(12.)).text_size(px(12.)).text_color(soft.opacity(0.8)).child(hint),
                );
            }
        }
        for (n, candidate) in state.results.iter().enumerate() {
            let progress = state.progress.get(&candidate.id).cloned();
            let id = candidate.id.clone();
            let working = progress == Some(Progress::Working);
            let (glyph, tooltip) = match &progress {
                Some(Progress::Working) => (LucideIcon::LoaderCircle, "Downloading…".to_string()),
                Some(Progress::Done) => (LucideIcon::Check, "Added to the library".to_string()),
                Some(Progress::Failed(why)) => (LucideIcon::RotateCw, format!("Failed: {why}. Try again")),
                None => (LucideIcon::Download, "Download to the library".to_string()),
            };
            let badges = candidate.badges();
            list = list.child(
                div()
                    .id(SharedString::from(format!("subs.row.{n}")))
                    .min_h(px(44.))
                    .px(px(12.))
                    .py(px(6.))
                    .rounded(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .hover(|s| s.bg(rgba(0xffffff1f)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(15.))
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(t.colors.foreground)
                                    .child(candidate.name.clone()),
                            )
                            .child(
                                div().truncate().text_size(px(12.)).text_color(soft).child(candidate.detail()),
                            )
                            .when(!badges.is_empty(), |el| {
                                el.child(
                                    div()
                                        .mt(px(4.))
                                        .flex()
                                        .flex_wrap()
                                        .gap(px(4.))
                                        .children(badges.into_iter().map(|b| pill(b.to_string()))),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("subs.download.{n}")))
                            .size(px(34.))
                            .rounded(px(10.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(!working, |el| el.cursor_pointer().hover(|s| s.bg(rgba(0xffffff29))))
                            .tooltip(tip(tooltip))
                            .child(icon(glyph, 18., t.colors.foreground))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.subs_download(&id, cx)
                            })),
                    ),
            );
        }

        let panel = div()
            .absolute()
            .right(px(24.))
            .w(px(PANEL_W.min(self.viewport_w - 48.)))
            .rounded(px(24.))
            .border_1()
            .border_color(rgba(0xf5f5f733))
            .on_mouse_down(MouseButton::Left, cx.listener(swallow))
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                if this.close_subs_panel() {
                    cx.notify();
                }
            }))
            .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .px(px(12.))
                    .pt(px(4.))
                    .truncate()
                    .text_size(px(17.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(t.colors.foreground)
                    .child("Search for subtitles"),
            )
            .child(
                div()
                    .px(px(12.))
                    .truncate()
                    .text_size(px(12.))
                    .text_color(soft)
                    .child(state.item_title.clone()),
            )
            .child(chips)
            .child(
                div().h(px(340.)).child(ScrollArea::new("subs.scroll").size_full().child(list)),
            );
        Some(match top {
            Some(top) => panel.top(px(top)),
            None => panel.bottom(px(134.)),
        })
    }

    // ----- the debug channel -----------------------------------------------------

    /// `subs delay <ms>`, `audio-delay <ms>`, `state`, `search <item> <lang>`,
    /// `panel <item>`, `download <place>`, `remove <item> <index>`.
    pub fn debug_subs(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        let arg = arg.trim();
        let write_allowed = std::env::var("BLOOM_ALLOW_SUBTITLE_WRITE").as_deref() == Ok("1");
        match verb {
            "" | "state" => self.subs_describe(),
            "delay" | "audio-delay" => {
                let Ok(ms) = arg.parse::<f64>() else {
                    return format!("error: subs {verb} <milliseconds>");
                };
                let delay = if verb == "delay" { Delay::Subtitle } else { Delay::Audio };
                if self.subs_set_delay(delay, ms / 1000., cx) {
                    "ok".to_string()
                } else {
                    format!(
                        "refused: {}",
                        blocked(delay, self.subs_burned()).unwrap_or("the player is not open")
                    )
                }
            }
            "search" => {
                let (id, language) = arg.split_once(' ').unwrap_or((arg, "eng"));
                let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
                    return "error: not signed in".into();
                };
                match client.search_subtitles(id, language.trim()) {
                    Err(err) => format!("error: {err:#}"),
                    Ok(search) => {
                        let mut out = format!("{} candidates", search.candidates.len());
                        if let Some(nothing) = &search.nothing {
                            out.push_str(&format!(" | {} {:?}", nothing.message(), nothing));
                        }
                        for (n, c) in search.candidates.iter().enumerate() {
                            out.push_str(&format!(
                                "\n{} {} | {} | {:?}",
                                n + 1,
                                c.name,
                                c.detail(),
                                c.badges()
                            ));
                        }
                        out
                    }
                }
            }
            "panel" => {
                if arg.is_empty() {
                    return if self.close_subs_panel() { "closed".into() } else { "error: subs panel <item id>".into() };
                }
                self.subs_load_rights(cx);
                let title = self
                    .session
                    .as_ref()
                    .and_then(|s| s.client.item(arg).ok())
                    .map_or_else(|| arg.to_string(), |item| item.display_title());
                self.open_subs_panel(arg, &title, window, cx);
                self.subs_describe()
            }
            "lang" => {
                self.subs.language = arg.to_string();
                self.subs_search(cx);
                self.subs_describe()
            }
            "download" => {
                if !write_allowed {
                    return "refused: set BLOOM_ALLOW_SUBTITLE_WRITE=1 (a download writes a file into the library)".into();
                }
                let Some((id, name)) = arg
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| self.subs.results.get(n.wrapping_sub(1)))
                    .map(|c| (c.id.clone(), c.name.clone()))
                else {
                    return "error: no such result; open the panel first".into();
                };
                self.subs_download(&id, cx);
                format!("downloading {name:?}")
            }
            "remove" => {
                if !write_allowed {
                    return "refused: set BLOOM_ALLOW_SUBTITLE_WRITE=1 (this deletes a file in the library)".into();
                }
                let (id, index) = arg.split_once(' ').unwrap_or((arg, ""));
                let Ok(index) = index.trim().parse::<i64>() else {
                    return "error: subs remove <item id> <stream index>".into();
                };
                let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
                    return "error: not signed in".into();
                };
                match client.remove_subtitle(id, index) {
                    Ok(()) => "removed".into(),
                    Err(err) => format!("error: {err:#}"),
                }
            }
            "streams" => {
                let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
                    return "error: not signed in".into();
                };
                match client.subtitle_streams(arg) {
                    Ok(list) => list
                        .iter()
                        .map(|s| format!("{}:{}{}", s.index, s.language, if s.external { ":ext" } else { "" }))
                        .collect::<Vec<_>>()
                        .join(" "),
                    Err(err) => format!("error: {err:#}"),
                }
            }
            _ => "error: subs state|delay <ms>|audio-delay <ms>|search <item> <lang>|panel [item]|lang <code>|\
                  download <place>|remove <item> <index>|streams <item>"
                .into(),
        }
    }

    fn subs_describe(&self) -> String {
        let status = &self.player_status;
        let selected = status
            .tracks
            .iter()
            .find(|t| t.kind == "sub" && t.selected)
            .map_or("none".to_string(), |t| format!("{} ({})", t.id, t.label()));
        let rights = self.subs.rights.as_ref().map_or("unknown".to_string(), |r| {
            format!("manage={} admin={}", r.manage, r.admin)
        });
        format!(
            "sub-delay={:.3} audio-delay={:.3} | subtitle={selected} | burned={} | rights: {rights} | panel={} lang={} loading={} results={} nothing={:?} error={:?} progress={:?}",
            status.sub_delay,
            status.audio_delay,
            if self.subs_burned() { "yes" } else { "no" },
            self.subs.panel_open,
            self.subs.language,
            self.subs.loading,
            self.subs.results.len(),
            self.subs.nothing,
            self.subs.error,
            self.subs.progress,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
        thread,
    };

    use serde_json::json;

    use super::*;

    #[test]
    fn delay_labels() {
        assert_eq!(delay_label(0.), "0 ms");
        assert_eq!(delay_label(0.25), "+250 ms");
        assert_eq!(delay_label(-0.1), "-100 ms");
        assert_eq!(delay_label(0.0004), "0 ms");
        assert_eq!(delay_label(12.5), "+12.5 s");
    }

    #[test]
    fn steps_round_to_milliseconds_and_stay_in_range() {
        assert_eq!(stepped(0., 100), 0.1);
        assert_eq!(stepped(0.1, -500), -0.4);
        assert_eq!(stepped(0.30000000000000004, 100), 0.4);
        assert_eq!(stepped(599.9, 500), 600.);
    }

    #[test]
    fn a_burned_subtitle_blocks_only_its_delay() {
        assert!(blocked(Delay::Subtitle, true).is_some());
        assert!(blocked(Delay::Subtitle, false).is_none());
        assert!(blocked(Delay::Audio, true).is_none());
    }

    fn sample() -> Value {
        json!([
            {"Id": "a", "Name": "Film.2024.en", "ProviderName": "Open Subtitles", "Format": "srt",
             "Author": "x", "DownloadCount": 1200, "CommunityRating": 8.1, "IsHashMatch": false,
             "ThreeLetterISOLanguageName": "eng", "HearingImpaired": true},
            {"Id": "b", "Name": "Film.2024.hash", "ProviderName": "Open Subtitles", "Format": "ass",
             "DownloadCount": 5, "IsHashMatch": true, "ThreeLetterISOLanguageName": "eng",
             "MachineTranslated": true, "Forced": true},
            {"Name": "no id"},
            "junk"
        ])
    }

    #[test]
    fn candidates_parse_and_sort_hash_matches_first() {
        let list = parse_candidates(&sample());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "b");
        assert_eq!(list[0].badges(), vec!["Hash match", "Forced", "Machine translated"]);
        assert_eq!(list[1].badges(), vec!["Hearing impaired"]);
        assert_eq!(list[1].detail(), "Open Subtitles · SRT · 1,200 downloads · rating 8.1 · by x");
        assert_eq!(list[1].language, "eng");
        assert!(parse_candidates(&json!({"x": 1})).is_empty());
    }

    #[test]
    fn language_choice() {
        assert_eq!(default_language(Some("spa")), "spa");
        assert_eq!(default_language(Some("")), "eng");
        assert_eq!(default_language(Some("english")), "eng");
        assert_eq!(default_language(None), "eng");
        assert_eq!(language_choices(Some("swe"))[0], "swe");
        assert_eq!(language_choices(Some("spa"))[0], "eng");
        assert_eq!(language_name("fra"), "French");
        assert_eq!(language_name("swe"), "SWE");
    }

    #[test]
    fn the_message_for_an_empty_search() {
        assert_eq!(nothing_found(Some(0)), Nothing::NoProvider);
        assert_eq!(
            nothing_found(Some(0)).message(),
            "The server has no subtitle provider. Install one (for example Open Subtitles) in the dashboard."
        );
        assert_eq!(nothing_found(Some(2)).message(), "No subtitles found");
        assert!(nothing_found(Some(2)).hint().is_none());
        assert_eq!(nothing_found(None).message(), "No subtitles found");
        assert!(nothing_found(None).hint().is_some());
    }

    #[test]
    fn request_paths() {
        assert_eq!(search_path("abc", "eng"), "/Items/abc/RemoteSearch/Subtitles/eng");
        assert_eq!(
            download_path("abc", "os/1 2"),
            "/Items/abc/RemoteSearch/Subtitles/os%2F1%202"
        );
        assert_eq!(remove_path("abc", 7), "/Videos/abc/Subtitles/7");
    }

    #[test]
    fn a_new_external_stream_is_found_by_its_path() {
        let stream = |index, external, path: &str| SubStream {
            index,
            external,
            path: path.to_string(),
            language: "eng".into(),
            title: String::new(),
        };
        let before = vec![stream(2, false, ""), stream(3, true, "/m/a.srt")];
        assert!(new_stream(&before, &before).is_none());
        // The new file sorts first: the old one moves to index 4.
        let after = vec![stream(2, false, ""), stream(3, true, "/m/0.srt"), stream(4, true, "/m/a.srt")];
        assert_eq!(new_stream(&before, &after).map(|s| s.index), Some(3));
    }

    #[test]
    fn streams_parse() {
        let item = json!({"MediaStreams": [
            {"Type": "Video", "Index": 0},
            {"Type": "Subtitle", "Index": 2, "IsExternal": true, "Path": "/m/a.srt", "Language": "eng"},
            {"Type": "Subtitle", "Index": 3, "Language": "spa"}
        ]});
        let list = parse_streams(&item);
        assert_eq!(list.len(), 2);
        assert!(list[0].external && !list[1].external);
    }

    /// A server that lists the item with one subtitle, then with two after
    /// the download, and writes down what it was asked.
    struct Mock {
        port: u16,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Mock {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let log = log.clone();
                    thread::spawn(move || serve(stream, log));
                }
            });
            Self { port, seen }
        }
    }

    fn serve(mut stream: TcpStream, log: Arc<Mutex<Vec<String>>>) {
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        while !data.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(n) => data.extend_from_slice(&buffer[..n]),
            }
        }
        let head = String::from_utf8_lossy(&data).to_string();
        let mut words = head.split_whitespace();
        let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
        let line = format!("{method} {target}");
        let downloaded = log.lock().unwrap().iter().any(|l| l.starts_with("POST "));
        log.lock().unwrap().push(line);
        let path = target.split('?').next().unwrap_or("");
        let (status, body) = if method == "POST" {
            ("204 No Content", String::new())
        } else if path.ends_with("/RemoteSearch/Subtitles/eng") {
            ("200 OK", sample().to_string())
        } else if path.starts_with("/Items/it1") {
            let mut streams = vec![json!({"Type": "Subtitle", "Index": 2, "IsExternal": true,
                "Path": "/m/old.srt", "Language": "eng"})];
            if downloaded {
                streams.push(json!({"Type": "Subtitle", "Index": 3, "IsExternal": true,
                    "Path": "/m/new.srt", "Language": "eng", "DisplayTitle": "English - SRT"}));
            }
            ("200 OK", json!({"MediaStreams": streams}).to_string())
        } else {
            ("404 Not Found", String::new())
        };
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    }

    #[test]
    fn search_and_download_against_a_mock_server() {
        let mock = Mock::start();
        let client = Client::new(&format!("http://127.0.0.1:{}", mock.port), "dev").with_session("tok", "u1");
        let search = client.search_subtitles("it1", "eng").unwrap();
        assert_eq!(search.candidates.len(), 2);
        assert!(search.nothing.is_none());

        let found = client.download_subtitle("it1", "os/1").unwrap().expect("a new stream");
        assert_eq!(found.index, 3);
        let seen = mock.seen.lock().unwrap().clone();
        assert!(seen.contains(&"GET /Items/it1/RemoteSearch/Subtitles/eng?isPerfectMatch=false".to_string()), "{seen:?}");
        assert!(seen.contains(&"POST /Items/it1/RemoteSearch/Subtitles/os%2F1".to_string()), "{seen:?}");
    }

    #[test]
    fn remove_refuses_an_embedded_stream() {
        let mock = Mock::start();
        let client = Client::new(&format!("http://127.0.0.1:{}", mock.port), "dev").with_session("tok", "u1");
        assert!(client.remove_subtitle("it1", 9).is_err());
        assert!(!mock.seen.lock().unwrap().iter().any(|l| l.starts_with("DELETE")));
        client.remove_subtitle("it1", 2).unwrap_err(); // the mock has no DELETE route: 404
        assert!(mock.seen.lock().unwrap().iter().any(|l| l == "DELETE /Videos/it1/Subtitles/2"));
    }
}
