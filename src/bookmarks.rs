// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bookmarks of the Jellyfin Enhanced plugin: a time in a title, with an
//! optional label. The plugin keeps them per user in `bookmark.json` on the
//! server (`/JellyfinEnhanced/user-settings/{user}/bookmark.json`), so the web
//! client and this app show the same ones.
//!
//! The file is `{ "Bookmarks": { "<id>": { ItemId, TmdbId, TvdbId, MediaType,
//! Name, Timestamp, Label, CreatedAt, UpdatedAt, SyncedFrom, SeasonNumber,
//! EpisodeNumber } } }`. The app never writes the whole file. It uses the
//! two calls of the plugin that change one entry on the server (`/add` and
//! `DELETE .../{id}`), so a bookmark that the web client adds at the same time
//! is not lost. The app keeps the file as it got it, so keys it does not know
//! stay.

use std::time::{Duration, Instant};

use gpui_icons::LucideIcon;
use gpui_kit::{
    AppContext as _, ClickEvent, Context, Div, Entity, Focusable as _, InteractiveElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled,
    Subscription, Window, div, prelude::FluentBuilder as _, px, relative, rgba,
};
use serde_json::{Value, json};

use crate::{
    app::Bloom,
    jellyfin::{Client, Item, format_duration},
    ui::{
        glass::glass,
        input::{Input, InputEvent, InputState},
        scroll_area::ScrollArea,
        theme::UiTheme,
        tip::tip,
    },
    views::cards::icon,
};

/// Width of the panel in the player.
const PANEL_W: f32 = 420.;

/// One bookmark of the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    /// Key of the entry in the file, such as "Bm_1790000000000_ab12cd34e".
    pub id: String,
    pub item_id: String,
    pub tmdb_id: String,
    pub tvdb_id: String,
    pub media_type: String,
    pub name: String,
    /// Seconds from the start.
    pub timestamp: f64,
    pub label: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
}

/// What the plugin knows of the title when it adds a bookmark.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    pub item_id: String,
    /// For an episode this is the id of the series, as in the plugin.
    pub tmdb_id: String,
    pub tvdb_id: String,
    /// "movie" or "tv".
    pub media_type: String,
    pub name: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
}

/// A field of an entry; the server writes PascalCase, the web client camelCase.
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

/// The entries of a `bookmark.json`, by time.
pub fn parse(file: &Value) -> Vec<Bookmark> {
    let Some(entries) = field(file, "Bookmarks").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut all: Vec<Bookmark> = entries
        .iter()
        .filter(|(_, entry)| entry.is_object())
        .map(|(id, entry)| Bookmark {
            id: id.clone(),
            item_id: text(entry, "ItemId"),
            tmdb_id: text(entry, "TmdbId"),
            tvdb_id: text(entry, "TvdbId"),
            media_type: text(entry, "MediaType"),
            name: text(entry, "Name"),
            timestamp: field(entry, "Timestamp")
                .and_then(Value::as_f64)
                .unwrap_or(0.),
            label: text(entry, "Label"),
            season: field(entry, "SeasonNumber").and_then(Value::as_i64),
            episode: field(entry, "EpisodeNumber").and_then(Value::as_i64),
        })
        .collect();
    all.sort_by(|a, b| a.timestamp.total_cmp(&b.timestamp).then(a.id.cmp(&b.id)));
    all
}

/// The bookmarks of one title, by the rule of the plugin
/// (`findBookmarksForItem`): those with its item id; when there are none,
/// those with the same TMDB or TVDB id. For an episode the season and the
/// episode number must agree, because the TMDB id is the series' id.
pub fn for_item(all: &[Bookmark], target: &Target) -> Vec<Bookmark> {
    let exact: Vec<Bookmark> = all
        .iter()
        .filter(|b| !target.item_id.is_empty() && b.item_id == target.item_id)
        .cloned()
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    all.iter()
        .filter(|b| {
            if target.episode.is_some()
                && b.episode.is_some()
                && !(b.episode == target.episode && b.season == target.season)
            {
                return false;
            }
            (!target.tmdb_id.is_empty() && b.tmdb_id == target.tmdb_id)
                || (!target.tvdb_id.is_empty() && b.tvdb_id == target.tvdb_id)
        })
        .cloned()
        .collect()
}

/// The body of `POST .../bookmark.json/add`.
pub fn add_body(target: &Target, secs: f64, label: &str) -> Value {
    json!({
        "ItemId": target.item_id,
        "TmdbId": target.tmdb_id,
        "TvdbId": target.tvdb_id,
        "MediaType": target.media_type,
        "Name": target.name,
        "Timestamp": secs,
        "Label": label,
        "SyncedFrom": "",
        "SeasonNumber": target.season,
        "EpisodeNumber": target.episode,
    })
}

/// Adds an entry to the file as the server does, after the server accepted it.
pub fn insert(file: &mut Value, id: &str, body: &Value, now: &str) {
    let mut entry = body.clone();
    if let Some(map) = entry.as_object_mut() {
        map.insert("CreatedAt".into(), json!(now));
        map.insert("UpdatedAt".into(), json!(now));
    }
    if !file.is_object() {
        *file = json!({});
    }
    let key = if file.get("Bookmarks").is_some() { "Bookmarks" } else { "bookmarks" };
    let map = file.as_object_mut().expect("an object");
    let entries = map.entry(key).or_insert_with(|| json!({}));
    if let Some(entries) = entries.as_object_mut() {
        entries.insert(id.to_string(), entry);
    }
}

/// Takes an entry out of the file. False when it was not there.
pub fn remove(file: &mut Value, id: &str) -> bool {
    let mut removed = false;
    for key in ["Bookmarks", "bookmarks"] {
        if let Some(entries) = file.get_mut(key).and_then(Value::as_object_mut) {
            removed |= entries.remove(id).is_some();
        }
    }
    removed
}

impl Target {
    /// The target of an item that the app has in full.
    fn of(item: &Item, series: Option<&Item>) -> Self {
        let id_of = |item: &Item, name: &str| {
            item.provider_ids.get(name).cloned().flatten().unwrap_or_default()
        };
        let episode = item.kind == "Episode";
        let (mut tmdb, mut tvdb) = (id_of(item, "Tmdb"), id_of(item, "Tvdb"));
        // A season or an episode has no TMDB id of its own; the series' id
        // stands for it. TVDB has one per episode, so the episode's wins.
        if let Some(series) = series {
            let from_series = id_of(series, "Tmdb");
            if !from_series.is_empty() {
                tmdb = from_series;
            }
            if tvdb.is_empty() {
                tvdb = id_of(series, "Tvdb");
            }
        }
        Self {
            item_id: item.id.clone(),
            tmdb_id: tmdb,
            tvdb_id: tvdb,
            media_type: match item.kind.as_str() {
                "Movie" => "movie".into(),
                "Series" | "Season" | "Episode" => "tv".into(),
                other => other.to_lowercase(),
            },
            name: if item.name.is_empty() { "Unknown".into() } else { item.name.clone() },
            season: episode.then_some(item.parent_index_number).flatten().map(i64::from),
            episode: episode.then_some(item.index_number).flatten().map(i64::from),
        }
    }
}

impl Client {
    pub fn bookmarks_file(&self) -> anyhow::Result<Value> {
        let user = self.user()?.to_string();
        self.get(&format!("/JellyfinEnhanced/user-settings/{user}/bookmark.json"), &[])
    }

    /// Who a bookmark of this item is for: its ids at other sites.
    pub fn bookmark_target(&self, item_id: &str) -> anyhow::Result<Target> {
        let item = self.item(item_id)?;
        let series = match (&item.series_id, item.kind.as_str()) {
            (Some(series), "Episode" | "Season") => self.item(series).ok(),
            _ => None,
        };
        Ok(Target::of(&item, series.as_ref()))
    }

    /// Asks the plugin to add a bookmark. It answers with the new key.
    pub fn bookmark_add(&self, body: &Value) -> anyhow::Result<String> {
        let user = self.user()?.to_string();
        let mut answer = self.post(
            &format!("/JellyfinEnhanced/user-settings/{user}/bookmark.json/add"),
            body,
        )?;
        let answer: Value = answer.read_json()?;
        answer
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("the server gave no bookmark id"))
    }

    pub fn bookmark_remove(&self, id: &str) -> anyhow::Result<()> {
        let user = self.user()?.to_string();
        self.call(
            "DELETE",
            &format!("/JellyfinEnhanced/user-settings/{user}/bookmark.json/{id}"),
            &[],
        )
    }
}

/// State of the bookmarks in the app.
#[derive(Default)]
pub struct BookmarkState {
    /// `bookmark.json` as the server has it.
    pub file: Option<Value>,
    pub panel_open: bool,
    pub panel_closed: Option<Instant>,
    /// The label field of the panel.
    pub input: Option<Entity<InputState>>,
    _subscription: Option<Subscription>,
}

impl BookmarkState {
    pub fn all(&self) -> Vec<Bookmark> {
        self.file.as_ref().map(parse).unwrap_or_default()
    }
}

impl Bloom {
    /// True when the plugin has bookmarks on for this server and the user
    /// has not switched them off in this app.
    pub fn bookmarks_on(&self) -> bool {
        self.enhanced_feature("bookmarks")
    }

    /// Reads `bookmark.json` of the user.
    pub fn load_bookmarks(&mut self, cx: &mut Context<Self>) {
        if !self.bookmarks_on() {
            return;
        }
        let opened = self.session.as_ref().map(|s| s.user_id.clone());
        self.fetch(
            cx,
            |client| client.bookmarks_file(),
            move |this, result, cx| {
                if this.session.as_ref().map(|s| s.user_id.clone()) != opened {
                    return;
                }
                match result {
                    Ok(file) => {
                        this.enhanced.bookmarks.file = Some(file);
                        cx.notify();
                    }
                    Err(err) => log::warn!("bookmarks: {err:#}"),
                }
            },
        );
    }

    /// Who the bookmarks list is for: the item in the player, else the item of
    /// the detail page. The ids at other sites come from the item itself, as
    /// far as the app has them; a bookmark of an item that came from a list
    /// is matched by its item id.
    fn bookmark_item(&self) -> Option<Item> {
        if let Some(item) = &self.playing {
            return Some(item.clone());
        }
        match &self.page {
            crate::app::Page::Detail(data) => Some(data.item.clone()),
            _ => None,
        }
    }

    /// The bookmarks of an item, by time.
    pub fn bookmarks_of(&self, item: &Item) -> Vec<Bookmark> {
        let series = None;
        for_item(&self.enhanced.bookmarks.all(), &Target::of(item, series))
    }

    /// Adds a bookmark at `secs` of the item. The plugin builds the entry; the
    /// app learns its key from the answer.
    pub fn add_bookmark(&mut self, item_id: String, secs: f64, label: String, cx: &mut Context<Self>) {
        if !self.bookmarks_on() {
            return;
        }
        let secs = (secs.max(0.) * 10.).round() / 10.;
        let id = item_id.clone();
        self.fetch(
            cx,
            move |client| {
                let target = client.bookmark_target(&id)?;
                let body = add_body(&target, secs, &label);
                let key = client.bookmark_add(&body)?;
                Ok((key, body))
            },
            move |this, result, cx| match result {
                Ok((key, body)) => {
                    let now = jiff::Timestamp::now().to_string();
                    let file = this.enhanced.bookmarks.file.get_or_insert_with(|| json!({}));
                    insert(file, &key, &body, &now);
                    this.toast("Bookmark added", format_duration(secs as i64), cx);
                    cx.notify();
                }
                Err(err) => this.toast("Could not add the bookmark", format!("{err:#}"), cx),
            },
        );
    }

    pub fn remove_bookmark(&mut self, id: String, cx: &mut Context<Self>) {
        let key = id.clone();
        self.fetch(
            cx,
            move |client| client.bookmark_remove(&key),
            move |this, result, cx| match result {
                Ok(()) => {
                    if let Some(file) = this.enhanced.bookmarks.file.as_mut() {
                        remove(file, &id);
                    }
                    cx.notify();
                }
                Err(err) => this.toast("Could not remove the bookmark", format!("{err:#}"), cx),
            },
        );
    }

    /// Jumps to a bookmark in the player.
    pub fn jump_to_bookmark(&mut self, secs: f64, cx: &mut Context<Self>) {
        self.request_seek_to(secs, cx);
    }

    /// The label field, made on the first use.
    fn bookmark_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = &self.enhanced.bookmarks.input {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Label (optional)"));
        self.enhanced.bookmarks._subscription = Some(cx.subscribe_in(
            &input,
            window,
            |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.add_bookmark_here(window, cx);
                }
            },
        ));
        self.enhanced.bookmarks.input = Some(input.clone());
        input
    }

    /// Adds a bookmark at the position of the player, with the text of the
    /// label field.
    pub fn add_bookmark_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.playing.clone() else {
            return;
        };
        let label = match &self.enhanced.bookmarks.input {
            Some(input) => {
                let text = input.read(cx).value().trim().to_string();
                input.update(cx, |input, cx| input.set_value("", window, cx));
                text
            }
            None => String::new(),
        };
        self.add_bookmark(item.id, self.player_status.position, label, cx);
    }

    /// Opens or closes the panel of the player.
    pub fn toggle_bookmarks_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.bookmarks_on() {
            return;
        }
        // One popup at a time.
        let open = !self.enhanced.bookmarks.panel_open;
        self.close_popups(None, window, cx);
        // A click on the button while the panel is open closed it as a click
        // outside of it a moment ago; that click must not open it again.
        if open
            && self
                .enhanced
                .bookmarks
                .panel_closed
                .is_some_and(|at| at.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.enhanced.bookmarks.panel_open = open;
        if open {
            self.bookmark_input(window, cx);
            self.load_bookmarks(cx);
        }
        cx.notify();
    }

    /// Closes the panel; called with the other popups.
    pub fn close_bookmarks_panel(&mut self) -> bool {
        std::mem::take(&mut self.enhanced.bookmarks.panel_open)
    }

    /// Escape in the player closes the panel first.
    pub fn bookmarks_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.enhanced.bookmarks.panel_open {
            return false;
        }
        self.enhanced.bookmarks.panel_open = false;
        window.focus(&self.player_focus, cx);
        true
    }

    /// True while the label field has the focus: the keys of the player must
    /// leave the typing alone.
    pub fn bookmark_typing(&self, window: &Window, cx: &Context<Self>) -> bool {
        self.enhanced.bookmarks.panel_open
            && self
                .enhanced
                .bookmarks
                .input
                .as_ref()
                .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
    }

    // ----- player ----------------------------------------------------------------

    /// The button of the player bar.
    pub fn bookmark_button(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !self.bookmarks_on() {
            return None;
        }
        let open = self.enhanced.bookmarks.panel_open;
        Some(
            div()
                .id("player.bookmarks")
                .size(px(40.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xffffff1f)))
                .when(open, |el| el.bg(rgba(0xffffff1f)))
                .tooltip(tip("Bookmarks (B adds one)"))
                .child(icon(LucideIcon::Bookmark, 22., gpui_kit::rgb(0xffffff)))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.toggle_bookmarks_panel(window, cx)
                })),
        )
    }

    /// Marks of the bookmarks on the seek bar, over the slider.
    pub fn bookmark_marks(&self) -> Vec<Div> {
        let duration = self.player_status.duration;
        let Some(item) = self.playing.as_ref().filter(|_| self.bookmarks_on()) else {
            return Vec::new();
        };
        if duration <= 0. {
            return Vec::new();
        }
        self.bookmarks_of(item)
            .into_iter()
            .filter(|b| b.timestamp >= 0. && b.timestamp <= duration)
            .map(|b| {
                // A white diamond above the track; the chapter marks are
                // dark strokes in it.
                div()
                    .absolute()
                    .left(relative((b.timestamp / duration) as f32))
                    .ml(px(-3.5))
                    .top(px(3.))
                    .size(px(7.))
                    .rounded(px(2.))
                    .bg(rgba(0xf5f5f7ff))
                    .border_1()
                    .border_color(rgba(0x000000a0))
            })
            .collect()
    }

    /// The panel in the player: add one, jump to one, delete one.
    pub fn render_bookmarks_panel(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.enhanced.bookmarks.panel_open || !self.bookmarks_on() {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let soft = rgba(0xf5f5f7b3);
        let item = self.playing.clone()?;
        let list = self.bookmarks_of(&item);
        let input = self.enhanced.bookmarks.input.clone();
        let position = self.player_status.position;

        fn swallow(_: &mut Bloom, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Bloom>) {
            cx.stop_propagation();
        }
        let mut rows = div().flex().flex_col().gap(px(2.));
        for b in &list {
            let (jump, remove, at) = (b.timestamp, b.id.clone(), b.timestamp);
            rows = rows.child(
                div()
                    .id(SharedString::from(format!("bookmark.row.{}", b.id)))
                    .min_h(px(44.))
                    .px(px(12.))
                    .py(px(6.))
                    .rounded(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff1f)))
                    .child(
                        div()
                            .w(px(58.))
                            .flex_shrink_0()
                            .text_size(px(15.))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(t.colors.foreground)
                            .child(format_duration(at as i64)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .text_color(if b.label.is_empty() { soft } else { t.colors.foreground })
                            .child(if b.label.is_empty() { "No label".to_string() } else { b.label.clone() }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("bookmark.delete.{}", b.id)))
                            .size(px(28.))
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(rgba(0xffffff29)))
                            .tooltip(tip("Delete the bookmark"))
                            .child(icon(LucideIcon::Trash2, 15., soft))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.remove_bookmark(remove.clone(), cx)
                            })),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.jump_to_bookmark(jump, cx)
                    })),
            );
        }
        let height = (list.len() as f32 * 46.).min(230.);
        Some(
            div()
                .absolute()
                .right(px(24.))
                .bottom(px(134.))
                .w(px(PANEL_W.min(self.viewport_w - 48.)))
                .rounded(px(24.))
                .border_1()
                .border_color(rgba(0xf5f5f733))
                .on_mouse_down(MouseButton::Left, cx.listener(swallow))
                // A click anywhere else closes the panel, as it closes a menu.
                .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    if this.close_bookmarks_panel() {
                        this.enhanced.bookmarks.panel_closed = Some(Instant::now());
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
                        .text_size(px(17.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground)
                        .child("Bookmarks"),
                )
                .child(
                    div()
                        .px(px(4.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .children(input.map(|input| {
                            Input::new(&input).aria_label("Bookmark label").flex_1().h(px(38.))
                        }))
                        .child(
                            div()
                                .id("bookmark.add")
                                .h(px(38.))
                                .px(px(14.))
                                .flex_shrink_0()
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
                                .tooltip(tip("Add a bookmark at this time"))
                                .child(icon(LucideIcon::Plus, 16., t.colors.foreground))
                                .child(format!("Add {}", format_duration(position as i64)))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.add_bookmark_here(window, cx)
                                })),
                        ),
                )
                .child(if list.is_empty() {
                    div()
                        .px(px(12.))
                        .py(px(10.))
                        .text_size(px(14.))
                        .text_color(soft)
                        .child("No bookmark for this title.")
                } else {
                    div().h(px(height)).child(
                        ScrollArea::new("bookmarks.scroll").size_full().child(rows),
                    )
                }),
        )
    }

    // ----- detail page -----------------------------------------------------------

    /// The "Bookmarks" list of a detail page, when the item has some.
    pub fn bookmarks_detail_section(&self, item: &Item, left: f32, right: f32, cx: &mut Context<Self>) -> Option<Div> {
        if !self.bookmarks_on() || !item.is_playable() {
            return None;
        }
        let list = self.bookmarks_of(item);
        if list.is_empty() {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let soft = rgba(0xf5f5f7b3);
        let mut rows = div().flex().flex_col().gap(px(2.)).w(px(520.).min(px(self.viewport_w - left - right)));
        for b in list {
            let (item, at, remove) = (item.clone(), b.timestamp, b.id.clone());
            rows = rows.child(
                div()
                    .id(SharedString::from(format!("detail.bookmark.{}", b.id)))
                    .min_h(px(44.))
                    .px(px(12.))
                    .py(px(6.))
                    .rounded(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff1f)))
                    .tooltip(tip("Play from this time"))
                    .child(icon(LucideIcon::Bookmark, 16., soft))
                    .child(
                        div()
                            .w(px(58.))
                            .flex_shrink_0()
                            .text_size(px(15.))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(t.colors.foreground)
                            .child(format_duration(at as i64)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .text_color(if b.label.is_empty() { soft } else { t.colors.foreground })
                            .child(if b.label.is_empty() { "No label".to_string() } else { b.label.clone() }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("detail.bookmark.delete.{}", b.id)))
                            .size(px(28.))
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(rgba(0xffffff29)))
                            .tooltip(tip("Delete the bookmark"))
                            .child(icon(LucideIcon::Trash2, 15., soft))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.remove_bookmark(remove.clone(), cx)
                            })),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        // Starts the item at the time of the bookmark.
                        let mut start = item.clone();
                        start.user_data.playback_position_ticks =
                            (at * crate::jellyfin::TICKS_PER_SECOND as f64) as i64;
                        this.play(&start, true, window, cx);
                    })),
            );
        }
        Some(
            div()
                .pl(px(left))
                .pr(px(right))
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .text_size(px(22.))
                        .line_height(px(30.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground)
                        .child("Bookmarks"),
                )
                .child(rows),
        )
    }

    /// For the debug channel: the bookmarks of an item as the app has them
    /// and as the server has them.
    pub fn bookmarks_debug(&self, item_id: &str) -> String {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return "error: not signed in".into();
        };
        let line = |list: &[Bookmark]| {
            list.iter()
                .enumerate()
                .map(|(n, b)| {
                    format!("{}:{}s {:?} [{}]", n + 1, b.timestamp, b.label, b.id)
                })
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let target = match client.bookmark_target(item_id) {
            Ok(target) => target,
            Err(err) => return format!("error: {err:#}"),
        };
        let local = for_item(&self.enhanced.bookmarks.all(), &target);
        let server = client
            .bookmarks_file()
            .map(|file| for_item(&parse(&file), &target))
            .map_err(|err| format!("{err:#}"));
        format!(
            "enabled={} total_in_app={} | app: {} | server: {}",
            self.bookmarks_on(),
            self.enhanced.bookmarks.all().len(),
            line(&local),
            match server {
                Ok(list) => line(&list),
                Err(err) => format!("error {err}"),
            }
        )
    }

    /// `je bookmark-add <seconds> <label>`: at the item of the player, else of the page.
    pub fn bookmark_add_debug(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        let (secs, label) = rest.split_once(' ').unwrap_or((rest, ""));
        let Ok(secs) = secs.parse::<f64>() else {
            return "error: je bookmark-add <seconds> <label>".into();
        };
        let Some(item) = self.bookmark_item() else {
            return "error: no item in the player or on the page".into();
        };
        self.add_bookmark(item.id.clone(), secs, label.trim().to_string(), cx);
        format!("sent for {} ({})", item.id, item.name)
    }

    /// `je bookmark-jump <place>`: the player goes to a bookmark of its item.
    pub fn bookmark_jump_debug(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        let Some(item) = self.playing.clone() else {
            return "error: nothing plays".into();
        };
        match rest.trim().parse::<usize>().ok().and_then(|n| self.bookmarks_of(&item).get(n.wrapping_sub(1)).cloned()) {
            Some(b) => {
                self.jump_to_bookmark(b.timestamp, cx);
                format!("jumped to {}s", b.timestamp)
            }
            None => "error: je bookmark-jump <place>".into(),
        }
    }

    /// `je bookmark-remove <place>`: the place in the list of the item, from 1.
    pub fn bookmark_remove_debug(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        let Ok(place) = rest.trim().parse::<usize>() else {
            return "error: je bookmark-remove <place>".into();
        };
        let Some(item) = self.bookmark_item() else {
            return "error: no item in the player or on the page".into();
        };
        match self.bookmarks_of(&item).get(place.wrapping_sub(1)) {
            Some(b) => {
                self.remove_bookmark(b.id.clone(), cx);
                format!("sent: remove {} at {}s", b.id, b.timestamp)
            }
            None => format!("error: no bookmark at place {place}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> Value {
        json!({
            "Bookmarks": {
                "Bm_1": { "ItemId": "a1", "TmdbId": "10", "TvdbId": "", "MediaType": "movie",
                          "Name": "Film", "Timestamp": 90.5, "Label": "Fight",
                          "CreatedAt": "2026-01-01T00:00:00Z", "UpdatedAt": "2026-01-01T00:00:00Z",
                          "SyncedFrom": "", "SeasonNumber": null, "EpisodeNumber": null,
                          "FutureKey": [1, 2] },
                "Bm_2": { "ItemId": "a1", "Timestamp": 12.0, "Label": "" },
                "Bm_3": { "ItemId": "b2", "TmdbId": "77", "MediaType": "tv", "Name": "Pilot",
                          "Timestamp": 5.0, "SeasonNumber": 1, "EpisodeNumber": 1 },
            },
            "NewTopLevelKey": { "x": true }
        })
    }

    #[test]
    fn parse_sorts_by_time_and_reads_both_casings() {
        let all = parse(&file());
        let times: Vec<f64> = all.iter().map(|b| b.timestamp).collect();
        assert_eq!(times, [5.0, 12.0, 90.5]);
        assert_eq!(all[2].label, "Fight");
        assert_eq!(all[0].season, Some(1));
        let camel = json!({ "bookmarks": { "x": { "itemId": "z", "timestamp": 3 } } });
        let one = &parse(&camel)[0];
        assert_eq!((one.item_id.as_str(), one.timestamp), ("z", 3.0));
    }

    #[test]
    fn round_trip_keeps_unknown_keys() {
        let mut f = file();
        let target = Target {
            item_id: "a1".into(),
            tmdb_id: "10".into(),
            media_type: "movie".into(),
            name: "Film".into(),
            ..Default::default()
        };
        let body = add_body(&target, 33.3, "New");
        insert(&mut f, "Bm_new", &body, "2026-10-03T00:00:00Z");
        assert_eq!(f["Bookmarks"]["Bm_new"]["Label"], "New");
        assert_eq!(f["Bookmarks"]["Bm_new"]["CreatedAt"], "2026-10-03T00:00:00Z");
        // Nothing else changed: the unknown keys of the file and of an entry stay.
        assert_eq!(f["NewTopLevelKey"], json!({ "x": true }));
        assert_eq!(f["Bookmarks"]["Bm_1"]["FutureKey"], json!([1, 2]));
        assert!(remove(&mut f, "Bm_new"));
        assert!(!remove(&mut f, "Bm_new"));
        assert_eq!(f, file());
    }

    #[test]
    fn add_body_has_the_fields_of_the_plugin() {
        let target = Target {
            item_id: "e1".into(),
            tmdb_id: "99".into(),
            tvdb_id: "5".into(),
            media_type: "tv".into(),
            name: "Pilot".into(),
            season: Some(1),
            episode: Some(2),
        };
        let body = add_body(&target, 61.5, "Scene");
        assert_eq!(
            body,
            json!({
                "ItemId": "e1", "TmdbId": "99", "TvdbId": "5", "MediaType": "tv",
                "Name": "Pilot", "Timestamp": 61.5, "Label": "Scene", "SyncedFrom": "",
                "SeasonNumber": 1, "EpisodeNumber": 2
            })
        );
    }

    #[test]
    fn matching_by_item_then_by_provider() {
        let all = parse(&file());
        let by_id = Target { item_id: "a1".into(), tmdb_id: "999".into(), ..Default::default() };
        assert_eq!(for_item(&all, &by_id).len(), 2);
        // Another item id with the same TMDB id gets the provider matches.
        let other = Target { item_id: "zz".into(), tmdb_id: "10".into(), ..Default::default() };
        assert_eq!(for_item(&all, &other).len(), 1);
        // For an episode, another episode of the same series does not match.
        let episode = |n| Target {
            item_id: "zz".into(),
            tmdb_id: "77".into(),
            season: Some(1),
            episode: Some(n),
            ..Default::default()
        };
        assert_eq!(for_item(&all, &episode(1)).len(), 1);
        assert_eq!(for_item(&all, &episode(2)).len(), 0);
        assert!(for_item(&all, &Target::default()).is_empty());
    }
}
