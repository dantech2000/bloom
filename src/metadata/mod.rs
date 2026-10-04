// SPDX-License-Identifier: AGPL-3.0-or-later
//! Metadata manager for administrators: one dialog for an item with four
//! tabs. Details edits the fields of the item, Images shows its artwork and
//! finds other artwork, Identify matches the item to a title of a metadata
//! provider, and Refresh asks the server to read the metadata again.
//!
//! The app holds the editing state of the text fields, because a view
//! renders with `&Bloom` and cannot make an entity.

mod tabs;

use std::rc::Rc;

use anyhow::Result;
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Focusable as _, ScrollHandle, Subscription,
    Window,
    base::input::{InputEvent, InputState, TextareaState},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    admin::Confirm,
    app::Bloom,
    jellyfin::Client,
    ui::menu::MenuItem,
};

/// One text field of the Details tab.
pub struct FieldSpec {
    pub label: &'static str,
    pub placeholder: &'static str,
    /// Part of the row the field takes: 1.0 is the full width.
    pub span: f32,
}

const fn field(label: &'static str, placeholder: &'static str, span: f32) -> FieldSpec {
    FieldSpec {
        label,
        placeholder,
        span,
    }
}

// Places of the Details fields in `State::inputs`.
pub const TITLE: usize = 0;
pub const ORIGINAL_TITLE: usize = 1;
pub const SORT_TITLE: usize = 2;
pub const TAGLINE: usize = 3;
pub const YEAR: usize = 4;
pub const PREMIERE: usize = 5;
pub const END: usize = 6;
pub const COMMUNITY_RATING: usize = 7;
pub const CRITIC_RATING: usize = 8;
pub const OFFICIAL_RATING: usize = 9;
pub const GENRES: usize = 10;
pub const TAGS: usize = 11;
pub const STUDIOS: usize = 12;
pub const IMDB: usize = 13;
pub const TMDB: usize = 14;
pub const TVDB: usize = 15;
/// Fields of the Details tab.
pub const DETAIL_FIELDS: usize = 16;
// Fields of the Identify tab come after them.
pub const FIND_NAME: usize = 16;
pub const FIND_YEAR: usize = 17;
pub const FIND_IMDB: usize = 18;
pub const FIND_TMDB: usize = 19;
const FIELD_COUNT: usize = 20;

pub const FIELDS: [FieldSpec; FIELD_COUNT] = [
    field("Title", "Title", 1.),
    field("Original title", "Title in the original language", 0.5),
    field("Sort title", "Title the library sorts by", 0.5),
    field("Tagline", "Tagline", 1.),
    field("Year", "2024", 0.25),
    field("Release date", "YYYY-MM-DD", 0.25),
    field("End date", "YYYY-MM-DD", 0.25),
    field("Community rating", "0 to 10", 0.25),
    field("Critic rating", "0 to 100", 0.25),
    field("Parental rating", "PG-13", 0.25),
    field("Genres", "Drama, Mystery", 1.),
    field("Tags", "Comma separated", 1.),
    field("Studios", "Comma separated", 1.),
    field("IMDb id", "tt0000000", 1. / 3.),
    field("TMDb id", "000000", 1. / 3.),
    field("TheTVDB id", "000000", 1. / 3.),
    field("Name", "Title to look for", 0.5),
    field("Year", "2024", 0.5 / 3.),
    field("IMDb id", "tt0000000", 0.5 / 3.),
    field("TMDb id", "000000", 0.5 / 3.),
];

/// Parts of an item a refresh must not change, as the server names them.
pub const LOCKABLE: [(&str, &str); 8] = [
    ("Name", "Name"),
    ("Overview", "Overview"),
    ("Genres", "Genres"),
    ("OfficialRating", "Parental rating"),
    ("Cast", "People"),
    ("ProductionLocations", "Countries"),
    ("Studios", "Studios"),
    ("Tags", "Tags"),
];

/// Image types the Images tab can look for.
pub const IMAGE_TYPES: [&str; 7] = [
    "Primary", "Backdrop", "Logo", "Thumb", "Banner", "Art", "Disc",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Details,
    Images,
    Identify,
    Refresh,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Details, Tab::Images, Tab::Identify, Tab::Refresh];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Details => "Details",
            Tab::Images => "Images",
            Tab::Identify => "Identify",
            Tab::Refresh => "Refresh",
        }
    }

    /// Text fields of the tab, in Tab-key order.
    fn fields(self) -> std::ops::Range<usize> {
        match self {
            Tab::Details => 0..DETAIL_FIELDS,
            Tab::Identify => DETAIL_FIELDS..FIELD_COUNT,
            _ => 0..0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshMode {
    /// Looks for new and changed files only.
    Scan,
    /// Also asks the providers for metadata the item does not have.
    Missing,
    /// Replaces all metadata with that of the providers.
    All,
}

impl RefreshMode {
    pub const ALL: [RefreshMode; 3] = [RefreshMode::Scan, RefreshMode::Missing, RefreshMode::All];

    pub fn label(self) -> &'static str {
        match self {
            RefreshMode::Scan => "Scan for new and updated files",
            RefreshMode::Missing => "Search for missing metadata",
            RefreshMode::All => "Replace all metadata",
        }
    }

    pub fn note(self) -> &'static str {
        match self {
            RefreshMode::Scan => "Reads the files again. Does not ask the metadata providers.",
            RefreshMode::Missing => "Fills empty fields from the providers. Keeps what is there.",
            RefreshMode::All => "Replaces every field that is not locked. Your edits are lost.",
        }
    }
}

/// One image the item has.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageInfo {
    #[serde(default)]
    pub image_type: String,
    #[serde(default)]
    pub image_index: Option<u32>,
    #[serde(default)]
    pub image_tag: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// One image a provider offers.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RemoteImage {
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub community_rating: Option<f64>,
    #[serde(default)]
    pub language: Option<String>,
}

/// Images of one type that the providers offer.
pub struct Browse {
    pub kind: String,
    pub loading: bool,
    pub error: Option<String>,
    pub images: Vec<RemoteImage>,
    pub total: usize,
}

/// The open dialog.
pub struct Manager {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub tab: Tab,
    /// The whole item as the server gave it, so a save loses nothing.
    pub item: Option<Value>,
    pub error: Option<String>,
    /// Text of each Details field when the item loaded, and the overview
    /// last. A save writes only the fields whose text changed.
    pub initial: Vec<String>,
    /// "Continuing", "Ended" or "Unreleased"; series only.
    pub status: String,
    pub lock: bool,
    pub locked: Vec<String>,
    pub images: Option<Vec<ImageInfo>>,
    pub browse: Option<Browse>,
    pub refresh_mode: RefreshMode,
    pub replace_images: bool,
    pub replace_trickplay: bool,
    pub searching: bool,
    pub search_error: Option<String>,
    /// Titles the providers found; `None` before the first search.
    pub results: Option<Vec<Value>>,
    /// An apply also replaces the images of the item.
    pub apply_images: bool,
    /// A change is on its way to the server.
    pub busy: bool,
    /// Question that waits for an answer.
    pub confirm: Option<Confirm>,
}

impl Manager {
    /// True when Identify can look this kind of item up.
    pub fn can_identify(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "Movie" | "Series" | "BoxSet" | "Trailer" | "Person" | "MusicAlbum" | "MusicArtist"
        )
    }
}

/// State of the metadata manager that lives as long as the app.
pub struct State {
    pub open: Option<Manager>,
    /// Holds the focus of the dialog when no field has it.
    pub focus: FocusHandle,
    pub inputs: Vec<Entity<InputState>>,
    pub overview: Entity<TextareaState>,
    pub scroll: ScrollHandle,
    /// The fields wait for the text of the item (set in the next render,
    /// which has the window).
    fill: bool,
    /// The dialog opened since the last render and wants the focus.
    fresh: bool,
    _subscriptions: Vec<Subscription>,
}

impl State {
    pub fn new(window: &mut Window, cx: &mut Context<Bloom>) -> Self {
        let inputs: Vec<Entity<InputState>> = FIELDS
            .iter()
            .map(|spec| cx.new(|cx| InputState::new(window, cx).placeholder(spec.placeholder)))
            .collect();
        // Enter in a field of the Identify tab starts the search.
        let subscriptions = inputs[DETAIL_FIELDS..]
            .iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.metadata_search(cx);
                    }
                })
            })
            .collect();
        let overview = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .soft_wrap(true)
                .placeholder("Overview")
        });
        Self {
            open: None,
            focus: cx.focus_handle(),
            inputs,
            overview,
            scroll: ScrollHandle::new(),
            fill: false,
            fresh: false,
            _subscriptions: subscriptions,
        }
    }
}

/// Text of a field of the item.
fn text(item: &Value, key: &str) -> String {
    match &item[key] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Names of a list of the item, joined with commas.
fn names(item: &Value, key: &str) -> String {
    item[key]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().or_else(|| v["Name"].as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// "Drama, Mystery" as its parts.
fn split(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Text of every Details field of an item, and the overview last.
fn field_texts(item: &Value) -> Vec<String> {
    let date = |key: &str| text(item, key).chars().take(10).collect::<String>();
    let id = |key: &str| text(&item["ProviderIds"], key);
    vec![
        text(item, "Name"),
        text(item, "OriginalTitle"),
        text(item, "ForcedSortName"),
        item["Taglines"][0].as_str().unwrap_or_default().to_string(),
        text(item, "ProductionYear"),
        date("PremiereDate"),
        date("EndDate"),
        text(item, "CommunityRating"),
        text(item, "CriticRating"),
        text(item, "OfficialRating"),
        names(item, "Genres"),
        names(item, "Tags"),
        names(item, "Studios"),
        id("Imdb"),
        id("Tmdb"),
        id("Tvdb"),
        text(item, "Overview"),
    ]
}

/// Writes the text of one field into the item. An empty text clears it.
fn apply_field(item: &mut Value, index: usize, value: &str) -> Result<()> {
    let string = |v: &str| {
        if v.is_empty() {
            Value::Null
        } else {
            Value::String(v.to_string())
        }
    };
    let number = |v: &str, label: &str| -> Result<Value> {
        if v.is_empty() {
            return Ok(Value::Null);
        }
        let n: f64 = v
            .replace(',', ".")
            .parse()
            .map_err(|_| anyhow::anyhow!("{label} must be a number"))?;
        Ok(json!(n))
    };
    let date = |v: &str, label: &str| -> Result<Value> {
        if v.is_empty() {
            return Ok(Value::Null);
        }
        let valid = v.len() == 10
            && v.bytes().enumerate().all(|(i, b)| match i {
                4 | 7 => b == b'-',
                _ => b.is_ascii_digit(),
            });
        anyhow::ensure!(valid, "{label} must look like 2024-05-31");
        Ok(json!(format!("{v}T00:00:00.0000000Z")))
    };
    match index {
        TITLE => {
            anyhow::ensure!(!value.is_empty(), "The title cannot be empty");
            item["Name"] = json!(value);
        }
        ORIGINAL_TITLE => item["OriginalTitle"] = string(value),
        SORT_TITLE => item["ForcedSortName"] = string(value),
        TAGLINE => {
            item["Taglines"] = if value.is_empty() {
                json!([])
            } else {
                json!([value])
            }
        }
        YEAR => {
            item["ProductionYear"] = if value.is_empty() {
                Value::Null
            } else {
                json!(
                    value
                        .parse::<u32>()
                        .map_err(|_| anyhow::anyhow!("The year must be a number"))?
                )
            }
        }
        PREMIERE => item["PremiereDate"] = date(value, "The release date")?,
        END => item["EndDate"] = date(value, "The end date")?,
        COMMUNITY_RATING => item["CommunityRating"] = number(value, "The community rating")?,
        CRITIC_RATING => item["CriticRating"] = number(value, "The critic rating")?,
        OFFICIAL_RATING => item["OfficialRating"] = string(value),
        GENRES => item["Genres"] = json!(split(value)),
        TAGS => item["Tags"] = json!(split(value)),
        STUDIOS => {
            item["Studios"] = split(value)
                .into_iter()
                .map(|name| json!({ "Name": name }))
                .collect()
        }
        IMDB | TMDB | TVDB => {
            let key = ["Imdb", "Tmdb", "Tvdb"][index - IMDB];
            if !item["ProviderIds"].is_object() {
                item["ProviderIds"] = json!({});
            }
            let ids = item["ProviderIds"].as_object_mut().expect("object");
            if value.is_empty() {
                ids.remove(key);
            } else {
                ids.insert(key.to_string(), json!(value));
            }
        }
        _ => item["Overview"] = string(value),
    }
    Ok(())
}

impl Bloom {
    /// Menu entries of the metadata manager for an item. Empty for a user
    /// who is not an administrator.
    pub fn metadata_menu_items(&self, id: &str, name: &str, kind: &str, cx: &mut Context<Self>) -> Vec<MenuItem> {
        if !self.is_admin() {
            return Vec::new();
        }
        let this = cx.weak_entity();
        let entry = |key: &'static str, label: &'static str, tab: Tab| {
            let (handle, id, name, kind) =
                (this.clone(), id.to_string(), name.to_string(), kind.to_string());
            MenuItem::new(key, label).on_click(move |_, _, cx| {
                handle
                    .update(cx, |this, cx| this.open_metadata(&id, &name, &kind, tab, cx))
                    .ok();
            })
        };
        vec![
            MenuItem::separator(),
            entry("meta.menu.edit", "Edit metadata", Tab::Details),
            entry("meta.menu.images", "Edit images", Tab::Images),
            entry("meta.menu.identify", "Identify", Tab::Identify),
            entry("meta.menu.refresh", "Refresh metadata", Tab::Refresh),
        ]
    }

    /// Opens the metadata manager for an item on one of its tabs.
    pub fn open_metadata(&mut self, id: &str, name: &str, kind: &str, tab: Tab, cx: &mut Context<Self>) {
        if !self.is_admin() {
            return;
        }
        self.metadata.open = Some(Manager {
            id: id.to_string(),
            name: name.to_string(),
            kind: kind.to_string(),
            tab,
            item: None,
            error: None,
            initial: Vec::new(),
            status: String::new(),
            lock: false,
            locked: Vec::new(),
            images: None,
            browse: None,
            refresh_mode: RefreshMode::Scan,
            replace_images: false,
            replace_trickplay: false,
            searching: false,
            search_error: None,
            results: None,
            apply_images: true,
            busy: false,
            confirm: None,
        });
        self.metadata.fresh = true;
        self.metadata
            .scroll
            .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        self.load_metadata(cx);
        cx.notify();
    }

    pub fn close_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.metadata.open = None;
        window.focus(&self.app_focus, cx);
        cx.notify();
    }

    /// Shows another tab of the open dialog.
    pub fn set_metadata_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let Some(manager) = &mut self.metadata.open else {
            return;
        };
        manager.tab = tab;
        manager.confirm = None;
        self.metadata.fresh = true;
        self.metadata
            .scroll
            .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        cx.notify();
    }

    /// Reads the item and its images from the server.
    pub fn load_metadata(&mut self, cx: &mut Context<Self>) {
        let Some(manager) = &self.metadata.open else {
            return;
        };
        let id = manager.id.clone();
        let wanted = id.clone();
        self.fetch(
            cx,
            move |client| -> Result<(Value, Vec<ImageInfo>)> {
                let user = client.user()?.to_string();
                // The two requests run at the same time.
                std::thread::scope(|scope| {
                    let images = scope.spawn(|| {
                        client.get::<Vec<ImageInfo>>(&format!("/Items/{id}/Images"), &[])
                    });
                    let item: Value = client.get(&format!("/Items/{id}"), &[("userId", user)])?;
                    Ok((item, images.join().expect("images thread")?))
                })
            },
            move |this, result, cx| {
                let Some(manager) = &mut this.metadata.open else {
                    return;
                };
                if manager.id != wanted {
                    return;
                }
                match result {
                    Ok((item, images)) => {
                        manager.error = None;
                        manager.name = text(&item, "Name");
                        manager.kind = text(&item, "Type");
                        manager.status = text(&item, "Status");
                        manager.lock = item["LockData"].as_bool().unwrap_or(false);
                        manager.locked = item["LockedFields"]
                            .as_array()
                            .map(|list| {
                                list.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        manager.initial = field_texts(&item);
                        manager.item = Some(item);
                        manager.images = Some(images);
                        this.metadata.fill = true;
                    }
                    Err(err) => manager.error = Some(format!("{err:#}")),
                }
                cx.notify();
            },
        );
    }

    /// Puts the text of the item into the fields and gives the dialog the
    /// focus. The root render calls this, because it has the window.
    pub fn prepare_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(manager) = &self.metadata.open else {
            return;
        };
        if std::mem::take(&mut self.metadata.fill) {
            let texts = manager.initial.clone();
            for (input, value) in self.metadata.inputs.iter().zip(&texts) {
                input.update(cx, |input, cx| input.set_value(value.clone(), window, cx));
            }
            let overview = texts.last().cloned().unwrap_or_default();
            self.metadata
                .overview
                .update(cx, |input, cx| input.set_value(overview, window, cx));
            // The Identify form starts with what the item has.
            let find = [
                (FIND_NAME, TITLE),
                (FIND_YEAR, YEAR),
                (FIND_IMDB, IMDB),
                (FIND_TMDB, TMDB),
            ];
            for (target, source) in find {
                let value = texts.get(source).cloned().unwrap_or_default();
                self.metadata.inputs[target]
                    .update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        if std::mem::take(&mut self.metadata.fresh) {
            window.focus(&self.metadata.focus, cx);
        }
    }

    /// Moves the focus to the next field of the tab (Tab key).
    pub fn next_metadata_field(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(manager) = &self.metadata.open else {
            return;
        };
        let fields = manager.tab.fields();
        if fields.is_empty() {
            return;
        }
        let mut handles: Vec<FocusHandle> = self.metadata.inputs[fields]
            .iter()
            .map(|input| input.read(cx).focus_handle(cx))
            .collect();
        if manager.tab == Tab::Details {
            // The overview comes after the tagline on the page.
            handles.insert(TAGLINE + 1, self.metadata.overview.read(cx).focus_handle(cx));
        }
        let count = handles.len();
        let current = handles.iter().position(|h| h.contains_focused(window, cx));
        let next = match (current, back) {
            (Some(i), false) => (i + 1) % count,
            (Some(i), true) => (i + count - 1) % count,
            (None, _) => 0,
        };
        window.focus(&handles[next], cx);
    }

    /// Runs a change on the server and tells the result in a toast. On
    /// success the dialog and the page behind it load again.
    fn metadata_action(
        &mut self,
        done: &'static str,
        cx: &mut Context<Self>,
        work: impl FnOnce(Client) -> Result<()> + Send + 'static,
    ) {
        let Some(manager) = &mut self.metadata.open else {
            return;
        };
        manager.busy = true;
        manager.confirm = None;
        self.fetch(cx, work, move |this, result, cx| {
            if let Some(manager) = &mut this.metadata.open {
                manager.busy = false;
            }
            match result {
                Ok(()) => {
                    this.toast(done, "", cx);
                    this.load_metadata(cx);
                    this.load_page(cx);
                }
                Err(err) => this.toast("The server refused the change", format!("{err:#}"), cx),
            }
            cx.notify();
        });
        cx.notify();
    }

    /// Saves the Details tab: the item of the server with the fields the
    /// user changed.
    pub fn save_metadata(&mut self, cx: &mut Context<Self>) {
        let Some(manager) = &self.metadata.open else {
            return;
        };
        let Some(mut item) = manager.item.clone() else {
            return;
        };
        let mut texts: Vec<String> = self.metadata.inputs[..DETAIL_FIELDS]
            .iter()
            .map(|input| input.read(cx).value().trim().to_string())
            .collect();
        texts.push(self.metadata.overview.read(cx).value().trim().to_string());
        for (index, value) in texts.iter().enumerate() {
            if manager.initial.get(index).map(|s| s.trim()) == Some(value.as_str()) {
                continue;
            }
            if let Err(err) = apply_field(&mut item, index, value) {
                self.toast("Check the fields", format!("{err:#}"), cx);
                return;
            }
        }
        if manager.kind == "Series" && !manager.status.is_empty() {
            item["Status"] = json!(manager.status);
        }
        item["LockData"] = json!(manager.lock);
        item["LockedFields"] = json!(manager.locked);
        // Parts of the item the server writes but cannot read back. With
        // `Trickplay` in the body it answers 500 (it cannot build its own
        // `TrickplayInfoDto` from JSON); the others are large and are not
        // part of an edit.
        if let Some(fields) = item.as_object_mut() {
            for key in ["Trickplay", "MediaSources", "MediaStreams", "Chapters", "UserData"] {
                fields.remove(key);
            }
        }
        let id = manager.id.clone();
        self.metadata_action("Metadata saved", cx, move |client| {
            client.post(&format!("/Items/{id}"), &item).map(drop)
        });
    }

    /// Asks the server to read the metadata of the item again.
    pub fn refresh_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(manager) = &self.metadata.open else {
            return;
        };
        let id = manager.id.clone();
        let scan = manager.refresh_mode == RefreshMode::Scan;
        let mode = if scan { "Default" } else { "FullRefresh" }.to_string();
        let query = vec![
            ("Recursive", "true".to_string()),
            ("ImageRefreshMode", mode.clone()),
            ("MetadataRefreshMode", mode),
            ("ReplaceAllImages", (!scan && manager.replace_images).to_string()),
            ("RegenerateTrickplay", (!scan && manager.replace_trickplay).to_string()),
            (
                "ReplaceAllMetadata",
                (manager.refresh_mode == RefreshMode::All).to_string(),
            ),
        ];
        self.fetch(
            cx,
            move |client| client.call("POST", &format!("/Items/{id}/Refresh"), &query),
            |this, result, cx| match result {
                Ok(()) => this.toast("Refresh queued", "The server works on it now.", cx),
                Err(err) => this.toast("The server refused the refresh", format!("{err:#}"), cx),
            },
        );
        self.close_metadata(window, cx);
    }

    /// Loads the images of one type that the providers offer.
    pub fn browse_images(&mut self, kind: &str, cx: &mut Context<Self>) {
        let Some(manager) = &mut self.metadata.open else {
            return;
        };
        manager.browse = Some(Browse {
            kind: kind.to_string(),
            loading: true,
            error: None,
            images: Vec::new(),
            total: 0,
        });
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Page {
            #[serde(default)]
            images: Vec<RemoteImage>,
            #[serde(default)]
            total_record_count: usize,
        }
        let (id, kind) = (manager.id.clone(), kind.to_string());
        let wanted = (id.clone(), kind.clone());
        self.fetch(
            cx,
            move |client| {
                client.get::<Page>(
                    &format!("/Items/{id}/RemoteImages"),
                    &[
                        ("type", kind),
                        ("startIndex", "0".to_string()),
                        ("limit", "30".to_string()),
                        ("IncludeAllLanguages", "false".to_string()),
                    ],
                )
            },
            move |this, result, cx| {
                let Some(manager) = &mut this.metadata.open else {
                    return;
                };
                let Some(browse) = &mut manager.browse else {
                    return;
                };
                if (manager.id.as_str(), browse.kind.as_str()) != (wanted.0.as_str(), wanted.1.as_str()) {
                    return;
                }
                browse.loading = false;
                match result {
                    Ok(page) => {
                        browse.total = page.total_record_count;
                        browse.images = page.images;
                    }
                    Err(err) => browse.error = Some(format!("{err:#}")),
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    /// Asks before an image of a provider replaces the image of the item.
    pub fn ask_use_image(&mut self, image: &RemoteImage, cx: &mut Context<Self>) {
        let Some(manager) = &mut self.metadata.open else {
            return;
        };
        let Some(kind) = manager.browse.as_ref().map(|b| b.kind.clone()) else {
            return;
        };
        let (id, url) = (manager.id.clone(), image.url.clone());
        let provider = image.provider_name.clone().unwrap_or_default();
        let adds = kind == "Backdrop";
        manager.confirm = Some(Confirm {
            title: format!("Use this {} image?", kind.to_lowercase()),
            message: if adds {
                format!("The server downloads the image and adds it to the backdrops of {}.", manager.name)
            } else {
                format!(
                    "The server downloads the image. It replaces the {} image of {}.",
                    kind.to_lowercase(),
                    manager.name
                )
            },
            action: "Use image".to_string(),
            danger: false,
            run: Rc::new(move |this, cx| {
                let (id, kind, url, provider) =
                    (id.clone(), kind.clone(), url.clone(), provider.clone());
                this.metadata_action("Image saved", cx, move |client| {
                    client.call(
                        "POST",
                        &format!("/Items/{id}/RemoteImages/Download"),
                        &[("Type", kind), ("ImageUrl", url), ("ProviderName", provider)],
                    )
                });
            }),
        });
        cx.notify();
    }

    /// Asks before an image of the item is deleted.
    pub fn ask_delete_image(&mut self, image: &ImageInfo, cx: &mut Context<Self>) {
        let Some(manager) = &mut self.metadata.open else {
            return;
        };
        let (id, kind) = (manager.id.clone(), image.image_type.clone());
        let index = image.image_index.unwrap_or(0);
        manager.confirm = Some(Confirm {
            title: format!("Delete the {} image?", kind.to_lowercase()),
            message: format!(
                "The image is removed from {}. A later refresh can download one again.",
                manager.name
            ),
            action: "Delete".to_string(),
            danger: true,
            run: Rc::new(move |this, cx| {
                let (id, kind) = (id.clone(), kind.clone());
                this.metadata_action("Image deleted", cx, move |client| {
                    client.call("DELETE", &format!("/Items/{id}/Images/{kind}/{index}"), &[])
                });
            }),
        });
        cx.notify();
    }

    /// Asks the providers for titles that match the Identify form.
    pub fn metadata_search(&mut self, cx: &mut Context<Self>) {
        let value = |index: usize| self.metadata.inputs[index].read(cx).value().trim().to_string();
        let (name, year, imdb, tmdb) = (
            value(FIND_NAME),
            value(FIND_YEAR),
            value(FIND_IMDB),
            value(FIND_TMDB),
        );
        let Some(manager) = &mut self.metadata.open else {
            return;
        };
        if manager.tab != Tab::Identify || manager.searching || !manager.can_identify() {
            return;
        }
        if name.is_empty() && imdb.is_empty() && tmdb.is_empty() {
            manager.search_error = Some("Enter a name or an id.".to_string());
            cx.notify();
            return;
        }
        let mut info = json!({ "ProviderIds": {} });
        if !name.is_empty() {
            info["Name"] = json!(name);
        }
        if let Ok(year) = year.parse::<u32>() {
            info["Year"] = json!(year);
        }
        if !imdb.is_empty() {
            info["ProviderIds"]["Imdb"] = json!(imdb);
        }
        if !tmdb.is_empty() {
            info["ProviderIds"]["Tmdb"] = json!(tmdb);
        }
        let body = json!({
            "SearchInfo": info,
            "ItemId": manager.id,
            "IncludeDisabledProviders": true,
        });
        manager.searching = true;
        manager.search_error = None;
        let (kind, wanted) = (manager.kind.clone(), manager.id.clone());
        self.fetch(
            cx,
            move |client| -> Result<Vec<Value>> {
                let mut answer = client.post(&format!("/Items/RemoteSearch/{kind}"), &body)?;
                Ok(answer.read_json()?)
            },
            move |this, result, cx| {
                let Some(manager) = &mut this.metadata.open else {
                    return;
                };
                if manager.id != wanted {
                    return;
                }
                manager.searching = false;
                match result {
                    Ok(results) => manager.results = Some(results),
                    Err(err) => manager.search_error = Some(format!("{err:#}")),
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    /// Asks before the item takes the metadata of a search result.
    pub fn ask_apply_match(&mut self, result: &Value, cx: &mut Context<Self>) {
        let Some(manager) = &mut self.metadata.open else {
            return;
        };
        let (id, result) = (manager.id.clone(), result.clone());
        let images = manager.apply_images;
        let title = text(&result, "Name");
        manager.confirm = Some(Confirm {
            title: "Use this match?".to_string(),
            message: format!(
                "{} takes the metadata of \"{title}\"{}. The metadata it has now is replaced.",
                manager.name,
                if images { " and its images" } else { "" }
            ),
            action: "Apply".to_string(),
            danger: true,
            run: Rc::new(move |this, cx| {
                let (id, result) = (id.clone(), result.clone());
                this.metadata_action("Match applied", cx, move |client| {
                    client
                        .post(
                            &format!("/Items/RemoteSearch/Apply/{id}?ReplaceAllImages={images}"),
                            &result,
                        )
                        .map(drop)
                });
            }),
        });
        cx.notify();
    }
}

impl Bloom {
    /// What the open editor holds, as one line, for the debug command
    /// `meta state`.
    pub fn metadata_describe(&self) -> String {
        let Some(m) = &self.metadata.open else {
            return "meta closed".into();
        };
        let images = m.images.as_ref().map(|list| {
            list.iter()
                .map(|i| format!("{}#{}", i.image_type, i.image_index.unwrap_or(0)))
                .collect::<Vec<_>>()
                .join(",")
        });
        let browse = m.browse.as_ref().map(|b| {
            format!(
                "{} loading={} shown={} total={} error={:?}",
                b.kind,
                b.loading,
                b.images.len(),
                b.total,
                b.error
            )
        });
        let results = m.results.as_ref().map(|list| {
            list.iter().take(5).map(|r| text(r, "Name")).collect::<Vec<_>>().join(" | ")
        });
        format!(
            "meta tab={:?} item={:?} kind={} error={:?} images=[{}] browse=[{}] searching={} search_error={:?} results={} [{}]",
            m.tab,
            m.name,
            m.kind,
            m.error,
            images.unwrap_or_else(|| "not loaded".into()),
            browse.unwrap_or_else(|| "none".into()),
            m.searching,
            m.search_error,
            m.results.as_ref().map_or("none".to_string(), |r| r.len().to_string()),
            results.unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> Value {
        json!({
            "Name": "Example Show",
            "OriginalTitle": "Example Show",
            "ProductionYear": 2026,
            "PremiereDate": "2026-02-18T00:00:00.0000000Z",
            "CommunityRating": 5.5,
            "Genres": ["Crime", "Drama"],
            "Studios": [{ "Name": "Prime Video", "Id": "abc" }],
            "ProviderIds": { "Imdb": "tt30412869", "Tmdb": "241372" },
            "Overview": "A body is found.",
            "Path": "/media/shows/Example Show",
        })
    }

    #[test]
    fn reads_the_fields_of_an_item() {
        let texts = field_texts(&item());
        assert_eq!(texts.len(), DETAIL_FIELDS + 1);
        assert_eq!(texts[TITLE], "Example Show");
        assert_eq!(texts[YEAR], "2026");
        assert_eq!(texts[PREMIERE], "2026-02-18");
        assert_eq!(texts[COMMUNITY_RATING], "5.5");
        assert_eq!(texts[GENRES], "Crime, Drama");
        assert_eq!(texts[STUDIOS], "Prime Video");
        assert_eq!(texts[IMDB], "tt30412869");
        assert_eq!(texts[TVDB], "");
        assert_eq!(texts[DETAIL_FIELDS], "A body is found.");
    }

    #[test]
    fn writes_changed_fields_and_keeps_the_rest() {
        let mut changed = item();
        apply_field(&mut changed, TITLE, "Fifty-Six Days").unwrap();
        apply_field(&mut changed, YEAR, "").unwrap();
        apply_field(&mut changed, END, "2026-03-01").unwrap();
        apply_field(&mut changed, CRITIC_RATING, "81").unwrap();
        apply_field(&mut changed, GENRES, "Crime,  Mystery ,").unwrap();
        apply_field(&mut changed, STUDIOS, "Amazon").unwrap();
        apply_field(&mut changed, TVDB, "443312").unwrap();
        apply_field(&mut changed, IMDB, "").unwrap();
        apply_field(&mut changed, TAGLINE, "Every day counts").unwrap();
        apply_field(&mut changed, DETAIL_FIELDS, "").unwrap();
        assert_eq!(changed["Name"], "Fifty-Six Days");
        assert_eq!(changed["ProductionYear"], Value::Null);
        assert_eq!(changed["EndDate"], "2026-03-01T00:00:00.0000000Z");
        assert_eq!(changed["CriticRating"], 81.0);
        assert_eq!(changed["Genres"], json!(["Crime", "Mystery"]));
        assert_eq!(changed["Studios"], json!([{ "Name": "Amazon" }]));
        assert_eq!(changed["ProviderIds"], json!({ "Tmdb": "241372", "Tvdb": "443312" }));
        assert_eq!(changed["Taglines"], json!(["Every day counts"]));
        assert_eq!(changed["Overview"], Value::Null);
        // Fields the editor does not know stay as they were.
        assert_eq!(changed["Path"], "/media/shows/Example Show");
        assert_eq!(changed["PremiereDate"], "2026-02-18T00:00:00.0000000Z");
    }

    #[test]
    fn refuses_text_that_is_not_a_value() {
        let mut changed = item();
        assert!(apply_field(&mut changed, TITLE, "").is_err());
        assert!(apply_field(&mut changed, YEAR, "next year").is_err());
        assert!(apply_field(&mut changed, PREMIERE, "18.02.2026").is_err());
        assert!(apply_field(&mut changed, COMMUNITY_RATING, "good").is_err());
        assert_eq!(changed, item());
    }
}
