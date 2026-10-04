// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hidden content of the Jellyfin Enhanced plugin. A user hides a title; the
//! plugin then leaves it out of the lists the user sees. The list is per user,
//! in `hidden-content.json` on the server
//! (`/JellyfinEnhanced/user-settings/{user}/hidden-content.json`):
//! `{ "Items": { "<key>": { ItemId, Name, Type, TmdbId, HiddenAt, PosterPath,
//! SeriesId, SeriesName, SeasonNumber, EpisodeNumber, HideScope } },
//! "Settings": { Enabled, FilterLibrary, FilterSearch, ... } }`.
//!
//! The plugin filters on the server, in the answers of the item lists, of
//! Continue Watching, of Next Up, of the latest items, of the suggestions and
//! of the search hints. That runs only while the administrator has "hidden
//! content" on in the plugin. The app follows the same switch, and reads the
//! same rules (`IsHiddenById` and `ShouldFilterSurface` of
//! `HiddenContentResponseFilter.cs`), so that a title it hides leaves the
//! lists on the page at once, before the next load.
//!
//! The only write call of the plugin for this file takes the whole file, so
//! the app reads it again, changes one entry, and sends it back. It keeps the
//! keys it does not know.

use std::collections::{HashMap, HashSet};

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, div, px, rgba,
};
use serde_json::{Value, json};

use crate::{
    app::{Bloom, Page},
    jellyfin::{Client, Item},
    ui::{menu::MenuItem, theme::UiTheme, tip::tip},
    views::cards::icon,
};

/// Where a list shows, for the filter of the plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// Libraries, "latest" rows, the hero and the detail pages.
    Library,
    Search,
    NextUp,
    ContinueWatching,
}

impl Surface {
    fn name(self) -> &'static str {
        match self {
            Surface::Library => "library",
            Surface::Search => "search",
            Surface::NextUp => "nextup",
            Surface::ContinueWatching => "continuewatching",
        }
    }
}

/// The switches in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub filter_library: bool,
    pub filter_search: bool,
    pub filter_next_up: bool,
    pub filter_continue_watching: bool,
    pub show_hide_buttons: bool,
    pub show_button_details: bool,
}

impl Default for Settings {
    /// The defaults of the plugin.
    fn default() -> Self {
        Self {
            enabled: true,
            filter_library: true,
            filter_search: false,
            filter_next_up: true,
            filter_continue_watching: true,
            show_hide_buttons: true,
            show_button_details: true,
        }
    }
}

fn field<'a>(entry: &'a Value, name: &str) -> Option<&'a Value> {
    entry.get(name).or_else(|| {
        let mut camel = name.to_string();
        camel[..1].make_ascii_lowercase();
        entry.get(camel.as_str())
    })
}

fn text(entry: &Value, name: &str) -> String {
    field(entry, name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Settings {
    pub fn of(file: &Value) -> Self {
        let mut settings = Self::default();
        let Some(map) = field(file, "Settings").filter(|s| s.is_object()) else {
            return settings;
        };
        let flag = |name: &str, into: &mut bool| {
            if let Some(value) = field(map, name).and_then(Value::as_bool) {
                *into = value;
            }
        };
        flag("Enabled", &mut settings.enabled);
        flag("FilterLibrary", &mut settings.filter_library);
        flag("FilterSearch", &mut settings.filter_search);
        flag("FilterNextUp", &mut settings.filter_next_up);
        flag("FilterContinueWatching", &mut settings.filter_continue_watching);
        flag("ShowHideButtons", &mut settings.show_hide_buttons);
        flag("ShowButtonDetails", &mut settings.show_button_details);
        settings
    }

    /// `ShouldFilterSurface`.
    fn filters(&self, surface: Surface) -> bool {
        self.enabled
            && match surface {
                Surface::Library => self.filter_library,
                Surface::Search => self.filter_search,
                Surface::NextUp => self.filter_next_up,
                Surface::ContinueWatching => self.filter_continue_watching,
            }
    }
}

/// One hidden title.
#[derive(Clone, Debug, PartialEq)]
pub struct Hidden {
    /// Key in the file: the item id, or "tmdb-<id>" for a title not in the library.
    pub key: String,
    pub item_id: String,
    pub name: String,
    pub kind: String,
    pub tmdb_id: String,
    pub series_id: String,
    pub series_name: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    /// "global", "nextup", "continuewatching" or "homesections".
    pub scope: String,
}

impl Hidden {
    /// Where the title is hidden, in words.
    pub fn scope_label(&self) -> &'static str {
        match self.scope.as_str() {
            "nextup" => "Next Up only",
            "continuewatching" => "Continue Watching only",
            "homesections" => "Home rows only",
            _ => "Everywhere",
        }
    }

    /// Name with the series and place of an episode.
    pub fn title(&self) -> String {
        match (&self.series_name, self.season, self.episode) {
            (name, Some(s), Some(e)) if !name.is_empty() => {
                format!("{name} S{s:02}E{e:02} {}", self.name)
            }
            _ => self.name.clone(),
        }
    }
}

/// The entries of a file, the newest first.
pub fn parse(file: &Value) -> Vec<Hidden> {
    let Some(items) = field(file, "Items").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut all: Vec<(String, Hidden)> = items
        .iter()
        .filter(|(_, entry)| entry.is_object())
        .map(|(key, entry)| {
            let scope = text(entry, "HideScope").to_lowercase();
            (
                text(entry, "HiddenAt"),
                Hidden {
                    key: key.clone(),
                    item_id: text(entry, "ItemId"),
                    name: text(entry, "Name"),
                    kind: text(entry, "Type"),
                    tmdb_id: text(entry, "TmdbId"),
                    series_id: text(entry, "SeriesId"),
                    series_name: text(entry, "SeriesName"),
                    season: field(entry, "SeasonNumber").and_then(Value::as_i64),
                    episode: field(entry, "EpisodeNumber").and_then(Value::as_i64),
                    scope: if scope.is_empty() { "global".into() } else { scope },
                },
            )
        })
        .collect();
    all.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.key.cmp(&b.1.key)));
    all.into_iter().map(|(_, hidden)| hidden).collect()
}

/// An id as the plugin compares ids: no dashes, lower case.
fn norm(id: &str) -> String {
    id.replace('-', "").to_lowercase()
}

/// What the filter looks up: the scopes of each hidden id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HideSet {
    pub settings: Settings,
    by_item: HashMap<String, HashSet<String>>,
    by_series: HashMap<String, HashSet<String>>,
}

impl HideSet {
    pub fn of(file: &Value) -> Self {
        let settings = Settings::of(file);
        let mut set = Self { settings: settings.clone(), ..Default::default() };
        // With the switch off the plugin builds an empty set.
        if !settings.enabled {
            return set;
        }
        for entry in parse(file) {
            if entry.item_id.trim().is_empty() {
                continue;
            }
            let id = norm(&entry.item_id);
            set.by_item.entry(id.clone()).or_default().insert(entry.scope.clone());
            if entry.kind.eq_ignore_ascii_case("Series") {
                set.by_series.entry(id).or_default().insert(entry.scope.clone());
            }
        }
        set
    }

    pub fn is_empty(&self) -> bool {
        self.by_item.is_empty() && self.by_series.is_empty()
    }

    /// `ScopeAppliesToSurface`: the surface must filter, and the scope must be
    /// that surface, "global", or "homesections" on the two home rows.
    fn applies(&self, scope: &str, surface: Surface) -> bool {
        if !self.settings.filters(surface) {
            return false;
        }
        scope == surface.name()
            || (scope == "homesections"
                && matches!(surface, Surface::NextUp | Surface::ContinueWatching))
            || scope == "global"
    }

    /// `IsHiddenById`: the item, its series, or the item as a series.
    pub fn is_hidden(&self, item_id: &str, series_id: Option<&str>, surface: Surface) -> bool {
        let id = norm(item_id);
        let any = |scopes: Option<&HashSet<String>>| {
            scopes.is_some_and(|set| set.iter().any(|scope| self.applies(scope, surface)))
        };
        any(self.by_item.get(&id))
            || series_id.is_some_and(|series| any(self.by_series.get(&norm(series))))
            || any(self.by_series.get(&id))
    }

    pub fn is_item_hidden(&self, item: &Item, surface: Surface) -> bool {
        self.is_hidden(&item.id, item.series_id.as_deref(), surface)
    }

    /// Takes the hidden items out of a list.
    pub fn filter(&self, items: &mut Vec<Item>, surface: Surface) {
        if !self.is_empty() {
            items.retain(|item| !self.is_item_hidden(item, surface));
        }
    }
}

/// Takes the hidden titles out of the lists of one page.
fn filter_page(page: &mut Page, set: &HideSet) {
    match page {
        Page::Home(data) => {
            set.filter(&mut data.hero, Surface::Library);
            set.filter(&mut data.resume, Surface::ContinueWatching);
            set.filter(&mut data.next_up, Surface::NextUp);
            for (_, items) in &mut data.latest {
                set.filter(items, Surface::Library);
            }
        }
        Page::Library(data) => set.filter(&mut data.items, Surface::Library),
        Page::Search(data) => set.filter(&mut data.results, Surface::Search),
        Page::Detail(data) => {
            set.filter(&mut data.similar, Surface::Library);
            set.filter(&mut data.seasons, Surface::Library);
            if data.next_up.as_ref().is_some_and(|i| set.is_item_hidden(i, Surface::NextUp)) {
                data.next_up = None;
            }
        }
        _ => {}
    }
}

/// How to hide a title. The scope "global" is the one of the hide button on a
/// detail page; the home rows use their own (see `hide_from_row`).
#[derive(Clone, Debug, PartialEq)]
pub struct HideRequest {
    pub item_id: String,
    pub name: String,
    pub kind: String,
    pub tmdb_id: String,
    pub series_id: String,
    pub series_name: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub scope: String,
}

impl HideRequest {
    /// For an item of the library. A series gets its own TMDB id; an episode
    /// carries the series' name and its place, as in the plugin.
    pub fn of(item: &Item) -> Self {
        let episode = item.kind == "Episode";
        Self {
            item_id: item.id.clone(),
            name: item.name.clone(),
            kind: item.kind.clone(),
            tmdb_id: item.provider_ids.get("Tmdb").cloned().flatten().unwrap_or_default(),
            series_id: item.series_id.clone().unwrap_or_default(),
            series_name: item.series_name.clone().unwrap_or_default(),
            season: episode.then_some(item.parent_index_number).flatten().map(i64::from),
            episode: episode.then_some(item.index_number).flatten().map(i64::from),
            scope: "global".into(),
        }
    }

    pub fn entry(&self, now: &str) -> Value {
        json!({
            "ItemId": self.item_id,
            "Name": self.name,
            "Type": self.kind,
            "TmdbId": self.tmdb_id,
            "HiddenAt": now,
            "PosterPath": "",
            "SeriesId": self.series_id,
            "SeriesName": self.series_name,
            "SeasonNumber": self.season,
            "EpisodeNumber": self.episode,
            "HideScope": self.scope,
        })
    }
}

/// Adds an entry to a file, under the item id. The other entries, the
/// settings and the keys of the file that are not known stay as they are. An
/// older entry of the same item has its scope widened, never narrowed: the
/// one that already covers everything stays.
pub fn hide(file: &mut Value, request: &HideRequest, now: &str) {
    if !file.is_object() {
        *file = json!({});
    }
    let key = if file.get("Items").is_some() || file.get("items").is_none() {
        "Items"
    } else {
        "items"
    };
    let map = file.as_object_mut().expect("an object");
    let items = map.entry(key).or_insert_with(|| json!({}));
    let Some(items) = items.as_object_mut() else { return };
    let id = norm(&request.item_id);
    let existing = items
        .iter()
        .find(|(k, entry)| norm(k) == id || norm(&text(entry, "ItemId")) == id)
        .map(|(k, entry)| (k.clone(), text(entry, "HideScope").to_lowercase()));
    match existing {
        Some((old_key, old_scope)) => {
            let scope = widest(&old_scope, &request.scope);
            if let Some(entry) = items.get_mut(&old_key).and_then(Value::as_object_mut) {
                entry.insert("HideScope".into(), json!(scope));
            }
        }
        None => {
            items.insert(request.item_id.clone(), request.entry(now));
        }
    }
}

/// The wider of two scopes (`WiderScope` of the plugin): "global" over
/// "homesections" over the two rows; the two rows together make "homesections".
fn widest(a: &str, b: &str) -> String {
    let rank = |scope: &str| match scope {
        "global" | "" => 4,
        "homesections" => 3,
        "continuewatching" | "nextup" => 2,
        _ => 1,
    };
    let (a, b) = (if a.is_empty() { "global" } else { a }, if b.is_empty() { "global" } else { b });
    if rank(a) == 2 && rank(b) == 2 && a != b {
        return "homesections".into();
    }
    if rank(a) >= rank(b) { a.to_string() } else { b.to_string() }
}

/// Takes the entry of a key or of an item id out of the file. True when one
/// was there.
pub fn unhide(file: &mut Value, key_or_id: &str) -> bool {
    let id = norm(key_or_id);
    for name in ["Items", "items"] {
        let Some(items) = file.get_mut(name).and_then(Value::as_object_mut) else {
            continue;
        };
        let found: Vec<String> = items
            .iter()
            .filter(|(k, entry)| {
                k.as_str() == key_or_id
                    || (!id.is_empty()
                        && (norm(k) == id || norm(&text(entry, "ItemId")) == id))
            })
            .map(|(k, _)| k.clone())
            .collect();
        if !found.is_empty() {
            for key in found {
                items.remove(&key);
            }
            return true;
        }
    }
    false
}

impl Client {
    pub fn hidden_file(&self) -> anyhow::Result<Value> {
        let user = self.user()?.to_string();
        self.get(&format!("/JellyfinEnhanced/user-settings/{user}/hidden-content.json"), &[])
    }

    fn save_hidden_file(&self, file: &Value) -> anyhow::Result<()> {
        let user = self.user()?.to_string();
        self.post(&format!("/JellyfinEnhanced/user-settings/{user}/hidden-content.json"), file)
            .map(drop)
    }

    /// Reads the file, changes it, writes it, and answers with the new file.
    /// The read is fresh, so a change the server made a moment ago (such as a
    /// "remove from Continue Watching") stays.
    pub fn change_hidden(
        &self,
        change: impl FnOnce(&mut Value) -> bool,
    ) -> anyhow::Result<Value> {
        let mut file = self.hidden_file()?;
        if change(&mut file) {
            self.save_hidden_file(&file)?;
        }
        Ok(file)
    }
}

/// State of the hidden content in the app.
#[derive(Default)]
pub struct HiddenState {
    /// `hidden-content.json` as the server has it.
    pub file: Option<Value>,
    pub set: HideSet,
    /// For the debug channel: the feature is on in this app only, and nothing
    /// is read from or written to the server. Used to look at the screens
    /// while the server has hidden content off.
    pub demo: bool,
}

impl HiddenState {
    pub fn set_file(&mut self, file: Value) {
        self.set = HideSet::of(&file);
        self.file = Some(file);
    }

    pub fn all(&self) -> Vec<Hidden> {
        self.file.as_ref().map(parse).unwrap_or_default()
    }
}

impl Bloom {
    /// True when the plugin has hidden content on for this server, the user
    /// has it on in the file, and has not switched it off in this app.
    pub fn hidden_on(&self) -> bool {
        self.enhanced.hidden.demo
            || (self.enhanced_feature("hidden_content") && self.enhanced.hidden.set.settings.enabled)
    }

    pub fn load_hidden(&mut self, cx: &mut Context<Self>) {
        if !self.enhanced_feature("hidden_content") || self.enhanced.hidden.demo {
            return;
        }
        let opened = self.session.as_ref().map(|s| s.user_id.clone());
        self.fetch(
            cx,
            |client| client.hidden_file(),
            move |this, result, cx| {
                if this.session.as_ref().map(|s| s.user_id.clone()) != opened {
                    return;
                }
                match result {
                    Ok(file) => {
                        this.enhanced.hidden.set_file(file);
                        this.drop_hidden_from_page();
                        cx.notify();
                    }
                    Err(err) => log::warn!("hidden content: {err:#}"),
                }
            },
        );
    }

    /// Hides titles. The page loses them at once; the file on the server
    /// follows.
    pub fn hide_items(&mut self, requests: Vec<HideRequest>, cx: &mut Context<Self>) {
        if !self.hidden_on() || requests.is_empty() {
            return;
        }
        let names = requests.iter().map(|r| r.name.clone()).collect::<Vec<_>>().join(", ");
        // The page shows the change before the server answers.
        let mut local = self.enhanced.hidden.file.clone().unwrap_or_else(|| json!({}));
        let now = jiff::Timestamp::now().to_string();
        for request in &requests {
            hide(&mut local, request, &now);
        }
        self.enhanced.hidden.set_file(local);
        self.drop_hidden_from_page();
        cx.notify();
        if self.enhanced.hidden.demo {
            return;
        }
        self.fetch(
            cx,
            move |client| {
                client.change_hidden(|file| {
                    for request in &requests {
                        hide(file, request, &now);
                    }
                    true
                })
            },
            move |this, result, cx| match result {
                Ok(file) => {
                    this.enhanced.hidden.set_file(file);
                    this.toast("Hidden", names.clone(), cx);
                    cx.notify();
                }
                Err(err) => {
                    this.toast("Could not hide it", format!("{err:#}"), cx);
                    this.load_hidden(cx);
                }
            },
        );
    }

    /// Shows a title again: takes its entry out of the file.
    pub fn unhide_item(&mut self, key_or_id: String, cx: &mut Context<Self>) {
        if !self.hidden_on() {
            return;
        }
        if let Some(file) = self.enhanced.hidden.file.clone() {
            let mut local = file;
            unhide(&mut local, &key_or_id);
            self.enhanced.hidden.set_file(local);
            cx.notify();
        }
        if self.enhanced.hidden.demo {
            self.load_page(cx);
            return;
        }
        let key = key_or_id.clone();
        self.fetch(
            cx,
            move |client| client.change_hidden(|file| unhide(file, &key)),
            move |this, result, cx| match result {
                Ok(file) => {
                    this.enhanced.hidden.set_file(file);
                    // The page lost the title when it hid it; load it again.
                    this.load_page(cx);
                    cx.notify();
                }
                Err(err) => {
                    this.toast("Could not unhide it", format!("{err:#}"), cx);
                    this.load_hidden(cx);
                }
            },
        );
    }

    /// Takes the hidden titles out of the lists of the page, where the plugin
    /// hides them: rows of the home page, a library, the search results, and
    /// the lists of a detail page.
    pub fn drop_hidden_from_page(&mut self) {
        if !self.hidden_on() {
            return;
        }
        let set = self.enhanced.hidden.set.clone();
        if set.is_empty() {
            return;
        }
        // The pages Back returns to lose them as well.
        filter_page(&mut self.page, &set);
        for page in &mut self.history {
            filter_page(page, &set);
        }
    }

    /// The "Hide" entries of the card menu and the "More" menu of the detail
    /// page. An episode can be hidden alone or with its show.
    pub fn hide_menu_items(&self, item: &Item, detail: bool, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let settings = &self.enhanced.hidden.set.settings;
        let allowed = self.hidden_on()
            && settings.show_hide_buttons
            && (!detail || settings.show_button_details)
            && matches!(item.kind.as_str(), "Movie" | "Series" | "Season" | "Episode");
        if !allowed {
            return Vec::new();
        }
        let this = cx.weak_entity();
        let mut entries = Vec::new();
        let hidden = self.enhanced.hidden.set.is_hidden(&item.id, None, Surface::Library);
        if hidden {
            let (handle, id) = (this.clone(), item.id.clone());
            entries.push(MenuItem::new("hidden.show", "Unhide").on_click(move |_, _, cx| {
                handle.update(cx, |this, cx| this.unhide_item(id.clone(), cx)).ok();
            }));
            return entries;
        }
        let (handle, request) = (this.clone(), HideRequest::of(item));
        let label = match item.kind.as_str() {
            "Episode" => "Hide this episode",
            "Season" => "Hide this season",
            _ => "Hide",
        };
        entries.push(MenuItem::new("hidden.hide", label).on_click(move |_, _, cx| {
            handle.update(cx, |this, cx| this.hide_items(vec![request.clone()], cx)).ok();
        }));
        if let (true, Some(series)) = (matches!(item.kind.as_str(), "Episode" | "Season"), &item.series_id) {
            let handle = this.clone();
            let request = HideRequest {
                item_id: series.clone(),
                name: item.series_name.clone().unwrap_or_default(),
                kind: "Series".into(),
                tmdb_id: String::new(),
                series_id: String::new(),
                series_name: String::new(),
                season: None,
                episode: None,
                scope: "global".into(),
            };
            entries.push(MenuItem::new("hidden.hide-show", "Hide the whole show").on_click(
                move |_, _, cx| {
                    handle.update(cx, |this, cx| this.hide_items(vec![request.clone()], cx)).ok();
                },
            ));
        }
        entries
    }

    /// The page "Hidden content" of the settings.
    pub(crate) fn render_settings_hidden(&self, cx: &mut Context<Self>) -> Div {
        use crate::settings::group;
        let t = UiTheme::read(cx).clone();
        let soft = t.colors.foreground.opacity(0.6);
        let all = self.enhanced.hidden.all();
        let mut list = group("Hidden titles", cx);
        if all.is_empty() {
            list = list.child(
                div()
                    .py(px(12.))
                    .text_size(px(15.))
                    .text_color(soft)
                    .child("Nothing is hidden. Use Hide in the menu of a card or of a title."),
            );
        }
        for entry in all {
            let key = entry.key.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("hidden.row.{}", entry.key)))
                    .min_h(px(52.))
                    .py(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(
                        div()
                            .size(px(38.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(rgba(0xcccccf69))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(
                                match entry.kind.as_str() {
                                    "Movie" => LucideIcon::Clapperboard,
                                    _ => LucideIcon::Tv,
                                },
                                18.,
                                t.colors.foreground,
                            )),
                    )
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
                                    .child(entry.title()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .text_color(soft)
                                    .child(format!(
                                        "{} · {}",
                                        if entry.kind.is_empty() { "Title" } else { entry.kind.as_str() },
                                        entry.scope_label()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("hidden.unhide.{}", entry.key)))
                            .h(px(36.))
                            .px(px(14.))
                            .rounded(px(12.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .cursor_pointer()
                            .bg(rgba(0xffffff1f))
                            .hover(|s| s.bg(rgba(0xffffff33)))
                            .text_size(px(14.))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(t.colors.foreground)
                            .tooltip(tip("Show this title again"))
                            .child(icon(LucideIcon::Eye, 16., t.colors.foreground))
                            .child("Unhide")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.unhide_item(key.clone(), cx)
                            })),
                    ),
            );
        }
        div().flex().flex_col().gap(px(18.)).child(list)
    }

    /// For the debug channel: what the app knows and what the server has.
    pub fn hidden_debug(&self) -> String {
        let settings = &self.enhanced.hidden.set.settings;
        let line = |list: &[Hidden]| {
            list.iter()
                .map(|h| format!("{} {:?} [{}] key={}", h.item_id, h.title(), h.scope, h.key))
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let server = self
            .session
            .as_ref()
            .map(|s| s.client.clone())
            .map(|client| match client.hidden_file() {
                Ok(file) => format!("{} entries: {}", parse(&file).len(), line(&parse(&file))),
                Err(err) => format!("error {err:#}"),
            })
            .unwrap_or_else(|| "not signed in".into());
        format!(
            "feature={} on={} settings(enabled={} library={} search={} nextup={} continue={}) | \
             app: {} entries: {} | server: {}",
            self.enhanced_feature("hidden_content"),
            self.hidden_on(),
            settings.enabled,
            settings.filter_library,
            settings.filter_search,
            settings.filter_next_up,
            settings.filter_continue_watching,
            self.enhanced.hidden.all().len(),
            line(&self.enhanced.hidden.all()),
            server
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "11111111-1111-1111-1111-111111111111";
    const B: &str = "22222222-2222-2222-2222-222222222222";
    const SHOW: &str = "33333333-3333-3333-3333-333333333333";

    fn file() -> Value {
        json!({
            "Items": {
                A: { "ItemId": A, "Name": "Film", "Type": "Movie", "HiddenAt": "2026-01-02T00:00:00Z",
                     "HideScope": "global", "FutureKey": 7 },
                B: { "ItemId": B, "Name": "Ep", "Type": "Episode", "SeriesId": SHOW,
                     "HiddenAt": "2026-01-03T00:00:00Z", "HideScope": "nextup" },
                SHOW: { "ItemId": SHOW, "Name": "Show", "Type": "Series",
                        "HiddenAt": "2026-01-01T00:00:00Z", "HideScope": "continuewatching" },
            },
            "Settings": { "Enabled": true, "FilterSearch": false, "Unknown": "x" },
            "TopLevel": [1]
        })
    }

    fn item(id: &str, series: Option<&str>) -> Item {
        Item { id: id.into(), series_id: series.map(String::from), ..Default::default() }
    }

    #[test]
    fn the_filter_follows_scope_and_surface() {
        let set = HideSet::of(&file());
        // A global hide hides on every surface that filters; search does not by default.
        assert!(set.is_hidden(A, None, Surface::Library));
        assert!(set.is_hidden(A, None, Surface::NextUp));
        assert!(set.is_hidden(A, None, Surface::ContinueWatching));
        assert!(!set.is_hidden(A, None, Surface::Search));
        // Ids with or without dashes, in any case, are the same id.
        assert!(set.is_hidden(&A.replace('-', "").to_uppercase(), None, Surface::Library));
        // A scoped hide hides on its row only.
        assert!(set.is_hidden(B, None, Surface::NextUp));
        assert!(!set.is_hidden(B, None, Surface::Library));
        assert!(!set.is_hidden(B, None, Surface::ContinueWatching));
        // A series hidden in Continue Watching hides its episodes there only.
        assert!(set.is_hidden("other", Some(SHOW), Surface::ContinueWatching));
        assert!(!set.is_hidden("other", Some(SHOW), Surface::NextUp));
        // The series row itself, by its own id.
        assert!(set.is_hidden(SHOW, None, Surface::ContinueWatching));
        assert!(!set.is_hidden("unknown", None, Surface::Library));
    }

    #[test]
    fn surface_switches_and_master_switch() {
        let mut f = file();
        f["Settings"]["FilterLibrary"] = json!(false);
        f["Settings"]["FilterSearch"] = json!(true);
        let set = HideSet::of(&f);
        assert!(!set.is_hidden(A, None, Surface::Library));
        assert!(set.is_hidden(A, None, Surface::Search));
        f["Settings"]["Enabled"] = json!(false);
        let off = HideSet::of(&f);
        assert!(off.is_empty());
        assert!(!off.is_hidden(A, None, Surface::Search));
        // homesections covers both home rows and nothing else.
        let both = HideSet::of(&json!({ "Items": { A: {
            "ItemId": A, "Type": "Movie", "HideScope": "homesections" } } }));
        assert!(both.is_hidden(A, None, Surface::NextUp));
        assert!(both.is_hidden(A, None, Surface::ContinueWatching));
        assert!(!both.is_hidden(A, None, Surface::Library));
    }

    #[test]
    fn filter_takes_items_out() {
        let set = HideSet::of(&file());
        let mut list = vec![item(A, None), item("keep", None), item("ep", Some(SHOW))];
        set.filter(&mut list, Surface::Library);
        let ids: Vec<&str> = list.iter().map(|i| i.id.as_str()).collect();
        // The series is hidden in Continue Watching only, so its episode stays here.
        assert_eq!(ids, ["keep", "ep"]);
        set.filter(&mut list, Surface::ContinueWatching);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn round_trip_keeps_unknown_keys() {
        let mut f = file();
        let request = HideRequest {
            item_id: "44444444-4444-4444-4444-444444444444".into(),
            name: "New".into(),
            kind: "Movie".into(),
            tmdb_id: "5".into(),
            series_id: String::new(),
            series_name: String::new(),
            season: None,
            episode: None,
            scope: "global".into(),
        };
        hide(&mut f, &request, "2026-10-03T00:00:00Z");
        assert_eq!(f["Items"][&request.item_id]["Name"], "New");
        assert_eq!(f["Items"][&request.item_id]["HideScope"], "global");
        assert_eq!(f["TopLevel"], json!([1]));
        assert_eq!(f["Settings"]["Unknown"], "x");
        assert_eq!(f["Items"][A]["FutureKey"], 7);
        assert!(unhide(&mut f, &request.item_id));
        assert!(!unhide(&mut f, &request.item_id));
        assert_eq!(f, file());
    }

    #[test]
    fn hiding_again_widens_the_scope() {
        let mut f = file();
        let mut request = HideRequest::of(&item(B, None));
        request.scope = "continuewatching".into();
        // The two rows together make homesections.
        hide(&mut f, &request, "now");
        assert_eq!(f["Items"][B]["HideScope"], "homesections");
        request.scope = "global".into();
        hide(&mut f, &request, "now");
        assert_eq!(f["Items"][B]["HideScope"], "global");
        // A narrower request never narrows it.
        request.scope = "nextup".into();
        hide(&mut f, &request, "now");
        assert_eq!(f["Items"][B]["HideScope"], "global");
        assert_eq!(f["Items"].as_object().unwrap().len(), 3);
    }

    #[test]
    fn unhide_by_item_id_with_other_dashes() {
        let mut f = file();
        assert!(unhide(&mut f, &A.replace('-', "")));
        assert!(f["Items"].get(A).is_none());
    }

    #[test]
    fn parse_lists_newest_first() {
        let all = parse(&file());
        let ids: Vec<&str> = all.iter().map(|h| h.item_id.as_str()).collect();
        assert_eq!(ids, [B, A, SHOW]);
        assert_eq!(all[0].scope_label(), "Next Up only");
        assert_eq!(all[1].scope_label(), "Everywhere");
    }

    #[test]
    fn settings_defaults() {
        let s = Settings::of(&json!({}));
        assert!(s.enabled && s.filter_library && !s.filter_search);
        let s = Settings::of(&json!({ "settings": { "filterSearch": true } }));
        assert!(s.filter_search);
    }
}
