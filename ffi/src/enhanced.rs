//! The Jellyfin Enhanced plugin for the iOS app (the Mac's src/enhanced.rs,
//! src/hidden.rs and src/bookmarks.rs, their requests and file rules):
//! the user's switches, the Seerr rows and links of a detail page, hidden
//! content, and bookmarks. A Seerr request (`seerr_request`) starts real
//! downloads: the app asks the user first, and no test sends one.

use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use bloom_core::{
    jellyfin::{Client, Item, ItemKind, Method, SeerrItem},
    quality_tags,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{BloomError, HideScope, MediaKind, SeerrMediaType, Server, media_kind, run};

/// What the plugin has set for this user; none when the server has no
/// such plugin.
#[derive(uniffi::Record, Default)]
pub struct EnhancedSettings {
    pub seerr_similar: bool,
    pub seerr_recommended: bool,
    /// Leave titles of the library out of the Seerr rows.
    pub seerr_skip_library: bool,
    pub seerr_link: bool,
    pub seerr_url: Option<String>,
    pub arr_links: bool,
    pub rating_in_player: bool,
    pub auto_skip_intro: bool,
    pub auto_skip_outro: bool,
    pub bookmarks: bool,
    pub hidden_content: bool,
    pub request_more: bool,
    /// The plugin's quality tags on posters (the user's value, else the
    /// server default, else off). The categories and their order are put
    /// in force in core when the settings are read.
    pub quality_tags: bool,
    /// The plugin's custom subtitle style for this user, in mpv's terms;
    /// none when the plugin's styles are off (spec §7.3: the last source).
    pub subtitle: Option<SubtitleLook>,
}

/// A subtitle look from one source (Bloom's setting, the system's caption
/// style, the plugin's); a field the source leaves to the next is none.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq)]
pub struct SubtitleLook {
    /// Text colour, "#AARRGGBB" as mpv reads it.
    pub color: Option<String>,
    /// A box behind the text (true) or an outline and shadow (false).
    pub boxed: Option<bool>,
    /// The box's colour, "#AARRGGBB"; the dark default when none.
    pub back: Option<String>,
    /// Size against the normal size.
    pub scale: Option<f64>,
    pub font: Option<String>,
    /// Place from the top, in percent; 100 is the lower edge.
    pub position: Option<i64>,
}

/// One mpv property and its value.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct MpvProperty {
    pub name: String,
    pub value: String,
}

/// The box of a look that names none (the Mac's dark box).
const DARK_BOX: &str = "#99000000";

/// The `sub-*` properties of the subtitle look, each from the first source
/// that sets it: Bloom's own setting, else the system's caption style, else
/// the Jellyfin Enhanced plugin's style (spec §7.3). A property none of
/// them sets is left to mpv. ASS styling and image tracks ignore them all
/// (mpv's `sub-ass-override` stays as it is).
#[uniffi::export]
pub fn subtitle_properties(own: SubtitleLook, system: SubtitleLook, plugin: Option<SubtitleLook>) -> Vec<MpvProperty> {
    let sources = [own, system, plugin.unwrap_or_default()];
    let first = |pick: &dyn Fn(&SubtitleLook) -> Option<String>| sources.iter().find_map(pick);
    let mut properties = Vec::new();
    let mut put = |name: &str, value: String| properties.push(MpvProperty { name: name.to_string(), value });
    if let Some(color) = first(&|look| look.color.clone()) {
        put("sub-color", color);
    }
    if let Some(look) = sources.iter().find(|look| look.boxed.is_some()) {
        if look.boxed == Some(true) {
            put("sub-border-style", "background-box".to_string());
            put("sub-back-color", look.back.clone().unwrap_or_else(|| DARK_BOX.to_string()));
        } else {
            put("sub-border-style", "outline-and-shadow".to_string());
        }
    }
    if let Some(scale) = first(&|look| look.scale.filter(|s| *s > 0.).map(|s| format!("{s:.3}"))) {
        put("sub-scale", scale);
    }
    if let Some(position) = first(&|look| look.position.map(|p| p.clamp(0, 150).to_string())) {
        put("sub-pos", position);
    }
    if let Some(font) = first(&|look| look.font.clone().filter(|f| !f.is_empty())) {
        put("sub-font", font);
    }
    properties
}

/// A colour of the plugin ("#RRGGBBAA", as CSS writes it) as mpv reads it
/// ("#AARRGGBB"), the Mac's `mpv_color`.
fn mpv_color(css: &str) -> String {
    match (css.get(1..7), css.get(7..9)) {
        (Some(rgb), Some(alpha)) => format!("#{alpha}{rgb}"),
        _ => css.to_string(),
    }
}

/// A switch of the plugin: the user's own value wins over the server
/// default; off when neither has it.
fn user_flag(own: &Value, public: &Value, name: &str) -> bool {
    own.get(name).and_then(Value::as_bool).or_else(|| public.get(name).and_then(Value::as_bool)).unwrap_or(false)
}

/// The plugin's subtitle style for the user (the Mac's src/enhanced.rs):
/// its presets by their place in each list, or the user's own colours.
/// None when the plugin's custom styles are off.
fn subtitle_style(own: &Value, public: &Value) -> Option<SubtitleLook> {
    let flag = |name: &str| user_flag(own, public, name);
    if flag("DisableCustomSubtitleStyles") {
        return None;
    }
    // The plugin's presets; the place in each list is what it saves.
    const STYLES: [(&str, &str); 6] = [
        ("#FFFFFFFF", "#00000000"),
        ("#FFFFFFFF", "#000000FF"),
        ("#FFFFFFFF", "#000000B2"),
        ("#FFFF00FF", "#000000B2"),
        ("#FFFFFFFF", "#444444B2"),
        ("#000000FF", "#FFFFFFFF"),
    ];
    const SIZES: [f64; 6] = [0.8, 1., 1.2, 1.8, 2., 3.];
    const FONTS: [Option<&str>; 5] = [None, Some("Noto Sans"), Some("Arial"), Some("Courier New"), Some("Roboto Mono")];
    let number = |name: &str, default: i64| own.get(name).and_then(Value::as_i64).unwrap_or(default);
    let pick = |name: &str, default: i64, len: usize| (number(name, default).max(0) as usize).min(len - 1);
    let (color, back) = STYLES[pick("SelectedStylePresetIndex", 0, STYLES.len())];
    let (mut color, mut back) = (color.to_string(), back.to_string());
    if flag("UsingCustomColors") {
        let own_color = |name: &str| {
            own.get(name).and_then(Value::as_str).filter(|c| c.len() == 9 && c.starts_with('#')).map(str::to_string)
        };
        color = own_color("CustomSubtitleTextColor").unwrap_or(color);
        back = own_color("CustomSubtitleBgColor").unwrap_or(back);
    }
    let boxed = !back.ends_with("00");
    Some(SubtitleLook {
        color: Some(mpv_color(&color)),
        boxed: Some(boxed),
        back: boxed.then(|| mpv_color(&back)),
        // The plugin's middle size is the normal one.
        scale: Some(SIZES[pick("SelectedFontSizePresetIndex", 2, SIZES.len())] / 1.2),
        font: FONTS[pick("SelectedFontFamilyPresetIndex", 0, FONTS.len())].map(str::to_string),
        // The plugin's default place is 85; mpv's is the lower edge.
        position: Some(number("SubtitleVerticalPosition", 85).saturating_add(15).clamp(0, 100)),
    })
}

/// A title of Seerr: a card that opens its page on Seerr, or the library's
/// item when the library has it.
#[derive(uniffi::Record)]
pub struct SeerrCard {
    pub id: i64,
    pub media_type: SeerrMediaType,
    pub title: String,
    pub year: Option<String>,
    pub poster: Option<String>,
    /// 2 pending, 3 processing, 4 partly available, 5 available.
    pub status: Option<i32>,
    pub jellyfin_id: Option<String>,
    pub page: Option<String>,
}

#[derive(uniffi::Record)]
pub struct NamedLink {
    pub name: String,
    pub url: String,
}

/// What a detail page shows from Seerr and the *arr tools.
#[derive(uniffi::Record, Default)]
pub struct SeerrExtras {
    /// The title's TMDB id, for "Request more seasons".
    pub tmdb_id: Option<i64>,
    pub recommended: Vec<SeerrCard>,
    pub similar: Vec<SeerrCard>,
    pub links: Vec<NamedLink>,
}

/// What the app filters at once after a Hide, as the plugin filters the
/// server's answers (the Mac's `HideSet`, `HiddenContentResponseFilter.cs`):
/// the switches of the file and the scopes of each hidden id. An id is as
/// the plugin compares ids: no dashes, lower case.
#[derive(uniffi::Record, Default)]
pub struct HiddenFilter {
    /// The user's "Enabled" in the file; off, nothing is filtered.
    pub enabled: bool,
    pub filter_library: bool,
    pub filter_search: bool,
    pub filter_next_up: bool,
    pub filter_continue_watching: bool,
    /// The scopes of each hidden item.
    pub by_item: HashMap<String, Vec<HideScope>>,
    /// The scopes of each hidden series, which hide its episodes too.
    pub by_series: HashMap<String, Vec<HideScope>>,
}

/// A hidden title.
#[derive(uniffi::Record)]
pub struct HiddenEntry {
    pub key: String,
    pub item_id: String,
    pub title: String,
    pub kind: MediaKind,
    pub scope: HideScope,
}

#[derive(uniffi::Record)]
pub struct Bookmark {
    pub id: String,
    pub secs: f64,
    pub label: String,
}

fn card(item: SeerrItem, base: Option<&str>) -> SeerrCard {
    SeerrCard {
        page: base.map(|base| format!("{base}/{}/{}", item.media_type, item.id)),
        title: item.display_name(),
        year: item.year(),
        poster: item.poster_path.as_ref().map(|p| format!("https://image.tmdb.org/t/p/w342{p}")),
        status: item.media_info.as_ref().and_then(|m| m.status),
        jellyfin_id: item.media_info.as_ref().and_then(|m| m.jellyfin_media_id.clone()),
        id: item.id,
        media_type: SeerrMediaType::parse(&item.media_type),
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
    field(entry, name).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// An id as the plugin compares ids (also for the app's filter).
#[uniffi::export]
pub fn hidden_id(id: String) -> String {
    norm(&id)
}

fn norm(id: &str) -> String {
    id.replace('-', "").to_lowercase()
}

/// Now as the plugin writes a time: "2026-11-14T10:00:00.000Z".
fn now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

fn read_settings(client: &Client) -> Option<EnhancedSettings> {
    let user = client.user().ok()?.to_string();
    let public = client.get::<Value>("/JellyfinEnhanced/public-config", &[]).ok()?;
    let own = client
        .get::<Value>(&format!("/JellyfinEnhanced/user-settings/{user}/settings.json"), &[])
        .unwrap_or(Value::Null);
    let flag = |name: &str| user_flag(&own, &public, name);
    // The administrator's choices: the user has no value of their own.
    let public_flag = |name: &str, default: bool| public.get(name).and_then(Value::as_bool).unwrap_or(default);
    let seerr = flag("JellyseerrEnabled");
    // The categories and their order, for the cards (the Mac's
    // `quality_prefs`); a null of the user is no choice.
    quality_tags::set_prefs(quality_tags::Prefs::from_settings(&own, &public));
    Some(EnhancedSettings {
        seerr_similar: seerr && flag("JellyseerrShowSimilar"),
        seerr_recommended: seerr && flag("JellyseerrShowRecommended"),
        seerr_skip_library: flag("JellyseerrExcludeLibraryItems"),
        seerr_link: seerr && flag("JellyseerrShowDetailPageLink"),
        seerr_url: public
            .get("JellyseerrBaseUrl")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(|url| url.trim_end_matches('/').to_string()),
        arr_links: flag("ArrLinksEnabled"),
        rating_in_player: flag("ShowRatingInPlayer"),
        auto_skip_intro: flag("AutoSkipIntro"),
        auto_skip_outro: flag("AutoSkipOutro"),
        bookmarks: public_flag("BookmarksEnabled", false),
        hidden_content: public_flag("HiddenContentEnabled", false),
        request_more: seerr && public_flag("JellyseerrShowRequestMoreOnSeries", true),
        quality_tags: quality_tags::master_switch(&own, &public),
        subtitle: subtitle_style(&own, &public),
    })
}

/// Puts the quality tags on the cards on or off. On, the list queries ask
/// for the streams the tags are derived from (they are large, so only
/// then); the pages must load again to show them. Answers whether the
/// switch changed.
#[uniffi::export]
pub fn set_quality_tags(on: bool) -> bool {
    let changed = on != bloom_core::jellyfin::quality_tags();
    bloom_core::jellyfin::set_quality_tags(on, true, true);
    changed
}

/// The quality tags of an item as a card shows them, with the choices in
/// force; none while the tags are off.
pub(crate) fn card_tags(item: &Item) -> Vec<String> {
    if bloom_core::jellyfin::quality_tags() {
        quality_tags::labels(item)
    } else {
        Vec::new()
    }
}

/// The filter of a hidden-content file (`HideSet::of_entries` of the Mac):
/// the plugin's defaults for a switch the file lacks, and an empty set
/// while the user has the feature off.
fn hidden_filter(file: &Value) -> HiddenFilter {
    let settings = field(file, "Settings").filter(|s| s.is_object());
    let flag = |name: &str, default: bool| settings.and_then(|s| field(s, name)).and_then(Value::as_bool).unwrap_or(default);
    let mut filter = HiddenFilter {
        enabled: flag("Enabled", true),
        filter_library: flag("FilterLibrary", true),
        filter_search: flag("FilterSearch", false),
        filter_next_up: flag("FilterNextUp", true),
        filter_continue_watching: flag("FilterContinueWatching", true),
        ..HiddenFilter::default()
    };
    if !filter.enabled {
        return filter;
    }
    let Some(items) = field(file, "Items").and_then(Value::as_object) else { return filter };
    for entry in items.values().filter(|entry| entry.is_object()) {
        let id = text(entry, "ItemId");
        if id.trim().is_empty() {
            continue;
        }
        let id = norm(&id);
        let scope = HideScope::of_file(&text(entry, "HideScope"));
        let add = |map: &mut HashMap<String, Vec<HideScope>>| {
            let scopes = map.entry(id.clone()).or_default();
            if !scopes.contains(&scope) {
                scopes.push(scope.clone());
            }
        };
        add(&mut filter.by_item);
        if text(entry, "Type").eq_ignore_ascii_case("Series") {
            add(&mut filter.by_series);
        }
    }
    filter
}

fn provider(item: &Item, name: &str) -> String {
    item.provider_ids.get(name).cloned().flatten().unwrap_or_default()
}

/// Titles Seerr relates to a title: `list` is "similar" or
/// "recommendations", `media` "movie" or "tv".
fn seerr_related(client: &Client, media: &str, tmdb: &str, list: &str) -> Vec<SeerrItem> {
    #[derive(Deserialize)]
    struct Page {
        #[serde(default)]
        results: Vec<SeerrItem>,
    }
    client
        .get::<Page>(&format!("/JellyfinEnhanced/jellyseerr/{media}/{tmdb}/{list}"), &[])
        .map(|page| {
            page.results
                .into_iter()
                .map(|mut item| {
                    if item.media_type.is_empty() {
                        item.media_type = media.to_string();
                    }
                    item
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pages of a title in Radarr or Sonarr (administrators only).
fn arr_links(client: &Client, movie: bool, id: &str) -> Vec<NamedLink> {
    let (param, group) = if movie { ("tmdbIds", "movies") } else { ("tvdbIds", "series") };
    let Ok(found) = client.get::<Value>("/JellyfinEnhanced/arr/links", &[(param, id.to_string())]) else {
        return Vec::new();
    };
    found
        .get(group)
        .and_then(|g| g.get(id))
        .and_then(|g| g.get("matches"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let base = m.get("instanceUrl")?.as_str()?.trim_end_matches('/');
            let name = m.get("instanceName")?.as_str()?.to_string();
            let url = if movie {
                format!("{base}/movie/{id}")
            } else {
                format!("{base}/series/{}", m.get("titleSlug")?.as_str()?)
            };
            Some(NamedLink { name, url })
        })
        .collect()
}

/// The wider of two scopes (the plugin's `WiderScope`).
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

/// Adds or widens the entry of an item; the rest of the file stays.
fn hide(file: &mut Value, item: &Item, scope: &str) {
    if !file.is_object() {
        *file = json!({});
    }
    let key = if file.get("Items").is_some() || file.get("items").is_none() { "Items" } else { "items" };
    let Some(map) = file.as_object_mut() else { return };
    let items = map.entry(key).or_insert_with(|| json!({}));
    let Some(items) = items.as_object_mut() else { return };
    let id = norm(&item.id);
    let existing = items
        .iter()
        .find(|(k, entry)| norm(k) == id || norm(&text(entry, "ItemId")) == id)
        .map(|(k, entry)| (k.clone(), text(entry, "HideScope").to_lowercase()));
    match existing {
        Some((old_key, old_scope)) => {
            let scope = widest(&old_scope, scope);
            if let Some(entry) = items.get_mut(&old_key).and_then(Value::as_object_mut) {
                entry.insert("HideScope".into(), json!(scope));
            }
        }
        None => {
            let episode = item.kind == ItemKind::Episode;
            items.insert(
                item.id.clone(),
                json!({
                    "ItemId": item.id,
                    "Name": item.name,
                    "Type": item.kind.to_string(),
                    "TmdbId": provider(item, "Tmdb"),
                    "HiddenAt": now(),
                    "PosterPath": "",
                    "SeriesId": item.series_id.clone().unwrap_or_default(),
                    "SeriesName": item.series_name.clone().unwrap_or_default(),
                    "SeasonNumber": episode.then_some(item.parent_index_number).flatten(),
                    "EpisodeNumber": episode.then_some(item.index_number).flatten(),
                    "HideScope": scope,
                }),
            );
        }
    }
}

fn unhide(file: &mut Value, key_or_id: &str) -> bool {
    let id = norm(key_or_id);
    for name in ["Items", "items"] {
        let Some(items) = file.get_mut(name).and_then(Value::as_object_mut) else { continue };
        let found: Vec<String> = items
            .iter()
            .filter(|(k, entry)| k.as_str() == key_or_id || (!id.is_empty() && (norm(k) == id || norm(&text(entry, "ItemId")) == id)))
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

fn hidden_path(client: &Client) -> anyhow::Result<String> {
    Ok(format!("/JellyfinEnhanced/user-settings/{}/hidden-content.json", client.user()?))
}

fn bookmarks_path(client: &Client) -> anyhow::Result<String> {
    Ok(format!("/JellyfinEnhanced/user-settings/{}/bookmark.json", client.user()?))
}

#[uniffi::export]
impl Server {
    /// The plugin's switches for this user; none without the plugin.
    pub async fn enhanced(&self) -> Result<Option<EnhancedSettings>, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(read_settings(&client))).await
    }

    /// The Seerr rows and the links of a title, as the settings allow.
    pub async fn seerr_extras(&self, item_id: String) -> Result<SeerrExtras, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let Some(settings) = read_settings(&client) else { return Ok(SeerrExtras::default()) };
            let item = client.item(&item_id)?;
            let movie = item.kind == ItemKind::Movie;
            if !movie && item.kind != ItemKind::Series {
                return Ok(SeerrExtras::default());
            }
            let media = if movie { "movie" } else { "tv" };
            let tmdb = provider(&item, "Tmdb");
            let base = settings.seerr_url.as_deref();
            let tmdb_id = tmdb.parse().ok();
            let keep = |list: Vec<SeerrItem>| -> Vec<SeerrCard> {
                list.into_iter()
                    .filter(|i| !settings.seerr_skip_library || i.media_info.as_ref().and_then(|m| m.jellyfin_media_id.as_ref()).is_none())
                    .map(|i| card(i, base))
                    .collect()
            };
            let mut extras = SeerrExtras { tmdb_id, ..SeerrExtras::default() };
            if !tmdb.is_empty() {
                if settings.seerr_recommended {
                    extras.recommended = keep(seerr_related(&client, media, &tmdb, "recommendations"));
                }
                if settings.seerr_similar {
                    extras.similar = keep(seerr_related(&client, media, &tmdb, "similar"));
                }
                if settings.seerr_link
                    && let Some(base) = base
                {
                    extras.links.push(NamedLink { name: "Seerr".into(), url: format!("{base}/{media}/{tmdb}") });
                }
            }
            if settings.arr_links {
                let id = if movie { tmdb } else { provider(&item, "Tvdb") };
                if !id.is_empty() {
                    extras.links.extend(arr_links(&client, movie, &id));
                }
            }
            Ok(extras)
        })
        .await
    }

    /// Titles Seerr knows for a search text (the Discover row of Search).
    pub async fn seerr_search(&self, query: String) -> Result<Vec<SeerrCard>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let base = read_settings(&client).and_then(|s| s.seerr_url);
            Ok(client.seerr_search(&query)?.into_iter().map(|i| card(i, base.as_deref())).collect())
        })
        .await
    }

    /// Asks Seerr for a movie, or for every season of a series. This
    /// starts real downloads: the app asks the user first, and no test
    /// sends it (CLAUDE.md).
    pub async fn seerr_request(&self, tmdb_id: i64, media_type: SeerrMediaType) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let item: SeerrItem = serde_json::from_value(json!({ "id": tmdb_id, "mediaType": media_type.text() }))
                .map_err(|err| BloomError::Internal { message: format!("seerr item: {err}") })?;
            Ok(client.seerr_request(&item)?)
        })
        .await
    }

    /// Hides a title: the file is read fresh, one entry changes, the whole
    /// file goes back (the plugin's only write call).
    pub async fn hide(&self, item_id: String, scope: HideScope) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let item = client.item(&item_id)?;
            let path = hidden_path(&client)?;
            // Only a file that is not there yet starts empty: any other
            // failure stops, or the whole file would be written over.
            let mut file = match client.get::<Value>(&path, &[]) {
                Ok(file) => file,
                Err(err) if bloom_core::jellyfin::status_of(&err) == Some(404) => json!({}),
                Err(err) => return Err(err.into()),
            };
            hide(&mut file, &item, scope.text());
            client.post(&path, &file)?;
            Ok(())
        })
        .await
    }

    pub async fn unhide(&self, key_or_id: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let path = hidden_path(&client)?;
            let mut file = client.get::<Value>(&path, &[])?;
            if unhide(&mut file, &key_or_id) {
                client.post(&path, &file)?;
            }
            Ok(())
        })
        .await
    }

    /// The hidden titles, the newest first.
    pub async fn hidden(&self) -> Result<Vec<HiddenEntry>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let file = client.get::<Value>(&hidden_path(&client)?, &[]).unwrap_or_else(|_| json!({}));
            let Some(items) = field(&file, "Items").and_then(Value::as_object) else { return Ok(Vec::new()) };
            let mut all: Vec<(String, HiddenEntry)> = items
                .iter()
                .map(|(key, entry)| {
                    let series = text(entry, "SeriesName");
                    let (season, episode) = (field(entry, "SeasonNumber").and_then(Value::as_i64), field(entry, "EpisodeNumber").and_then(Value::as_i64));
                    let name = text(entry, "Name");
                    let title = match (season, episode) {
                        (Some(s), Some(e)) if !series.is_empty() => format!("{series} S{s:02}E{e:02} {name}"),
                        _ => name,
                    };
                    (
                        text(entry, "HiddenAt"),
                        HiddenEntry {
                            key: key.clone(),
                            item_id: text(entry, "ItemId"),
                            title,
                            kind: media_kind(text(entry, "Type")),
                            scope: HideScope::of_file(&text(entry, "HideScope")),
                        },
                    )
                })
                .collect();
            all.sort_by(|a, b| b.0.cmp(&a.0));
            Ok(all.into_iter().map(|(_, entry)| entry).collect())
        })
        .await
    }

    /// What the app filters after a Hide: the switches of the file and the
    /// scopes of each hidden id. Read once, and again after a change.
    pub async fn hidden_filter(&self) -> Result<HiddenFilter, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(hidden_filter(&client.get::<Value>(&hidden_path(&client)?, &[]).unwrap_or_else(|_| json!({}))))).await
    }

    /// The bookmarks of an item, by time.
    pub async fn bookmarks(&self, item_id: String) -> Result<Vec<Bookmark>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let file = client.get::<Value>(&bookmarks_path(&client)?, &[]).unwrap_or_else(|_| json!({}));
            let id = norm(&item_id);
            let mut list: Vec<Bookmark> = ["Bookmarks", "bookmarks"]
                .iter()
                .filter_map(|key| file.get(*key).and_then(Value::as_object))
                .flatten()
                .filter(|(_, entry)| norm(&text(entry, "ItemId")) == id)
                .map(|(key, entry)| Bookmark {
                    id: key.clone(),
                    secs: field(entry, "Timestamp").and_then(Value::as_f64).unwrap_or_default(),
                    label: text(entry, "Label"),
                })
                .collect();
            list.sort_by(|a, b| a.secs.total_cmp(&b.secs));
            Ok(list)
        })
        .await
    }

    /// Adds a bookmark through the plugin's call for one entry, so one the
    /// web client adds at the same time is not lost. Answers its id.
    pub async fn add_bookmark(&self, item_id: String, secs: f64, label: String) -> Result<String, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let item = client.item(&item_id)?;
            let series = match (&item.series_id, &item.kind) {
                (Some(series), ItemKind::Episode | ItemKind::Season) => client.item(series).ok(),
                _ => None,
            };
            let episode = item.kind == ItemKind::Episode;
            let (mut tmdb, mut tvdb) = (provider(&item, "Tmdb"), provider(&item, "Tvdb"));
            if let Some(series) = &series {
                let from_series = provider(series, "Tmdb");
                if !from_series.is_empty() {
                    tmdb = from_series;
                }
                if tvdb.is_empty() {
                    tvdb = provider(series, "Tvdb");
                }
            }
            let body = json!({
                "ItemId": item.id,
                "TmdbId": tmdb,
                "TvdbId": tvdb,
                "MediaType": match &item.kind {
                    ItemKind::Movie => "movie".to_string(),
                    ItemKind::Series | ItemKind::Season | ItemKind::Episode => "tv".to_string(),
                    other => other.name().to_lowercase(),
                },
                "Name": if item.name.is_empty() { "Unknown".to_string() } else { item.name.clone() },
                "Timestamp": secs,
                "Label": label,
                "SyncedFrom": "",
                "SeasonNumber": episode.then_some(item.parent_index_number).flatten(),
                "EpisodeNumber": episode.then_some(item.index_number).flatten(),
            });
            let answer: Value = client.post(&format!("{}/add", bookmarks_path(&client)?), &body)?.read_json().map_err(anyhow::Error::from)?;
            answer
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| BloomError::Answered { detail: "the server gave no bookmark id".into() })
        })
        .await
    }

    pub async fn delete_bookmark(&self, id: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.call(Method::Delete, &format!("{}/{id}", bookmarks_path(&client)?), &[])?)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widens_scopes_as_the_plugin_does() {
        assert_eq!(widest("nextup", "continuewatching"), "homesections");
        assert_eq!(widest("global", "nextup"), "global");
        assert_eq!(widest("", "homesections"), "global");
        assert_eq!(widest("nextup", "nextup"), "nextup");
    }

    #[test]
    fn unhides_by_key_or_id_and_keeps_the_rest() {
        let mut file = json!({ "Items": {
            "a": { "ItemId": "aa-bb", "HideScope": "global" },
            "b": { "ItemId": "cc", "HideScope": "nextup" } }, "Settings": { "Enabled": true } });
        assert!(unhide(&mut file, "AABB"));
        assert!(file["Items"].get("a").is_none());
        assert!(file["Items"].get("b").is_some());
        assert_eq!(file["Settings"]["Enabled"], true);
        assert!(!unhide(&mut file, "zz"));
    }

    #[test]
    fn the_filter_follows_the_file() {
        let file = json!({ "Items": {
            "a": { "ItemId": "AA-BB", "Type": "Series", "HideScope": "nextup" },
            "b": { "itemId": "cc", "Type": "Movie" },
            "c": { "ItemId": "", "Type": "Movie" } },
            "Settings": { "FilterSearch": true } });
        let filter = hidden_filter(&file);
        assert!(filter.enabled && filter.filter_library && filter.filter_search);
        assert_eq!(filter.by_item.get("aabb"), Some(&vec![HideScope::NextUp]));
        assert_eq!(filter.by_series.get("aabb"), Some(&vec![HideScope::NextUp]));
        assert_eq!(filter.by_item.get("cc"), Some(&vec![HideScope::Global]));
        assert_eq!(filter.by_item.len(), 2);
        let off = hidden_filter(&json!({ "Items": { "b": { "ItemId": "cc" } }, "Settings": { "Enabled": false } }));
        assert!(!off.enabled && off.by_item.is_empty());
        assert!(!hidden_filter(&json!({})).filter_search, "search is not filtered by default");
    }

    #[test]
    fn the_plugin_style_follows_its_presets_and_own_colours() {
        let plain = subtitle_style(&json!({}), &json!({})).unwrap();
        assert_eq!(plain.color.as_deref(), Some("#FFFFFFFF"));
        assert_eq!((plain.boxed, plain.back.as_deref()), (Some(false), None));
        assert_eq!((plain.scale, plain.position, plain.font.as_deref()), (Some(1.), Some(100), None));
        let own = json!({ "SelectedStylePresetIndex": 3, "SelectedFontSizePresetIndex": 3,
            "SelectedFontFamilyPresetIndex": 2, "SubtitleVerticalPosition": 75 });
        let yellow = subtitle_style(&own, &json!({})).unwrap();
        assert_eq!(yellow.color.as_deref(), Some("#FFFFFF00"));
        assert_eq!((yellow.boxed, yellow.back.as_deref()), (Some(true), Some("#B2000000")));
        assert_eq!((yellow.scale, yellow.position, yellow.font.as_deref()), (Some(1.5), Some(90), Some("Arial")));
        let custom = json!({ "UsingCustomColors": true, "CustomSubtitleTextColor": "#00FF00FF",
            "CustomSubtitleBgColor": "#11223380", "SelectedStylePresetIndex": 99 });
        let custom = subtitle_style(&custom, &json!({})).unwrap();
        assert_eq!((custom.color.as_deref(), custom.back.as_deref()), (Some("#FF00FF00"), Some("#80112233")));
        assert!(subtitle_style(&json!({}), &json!({ "DisableCustomSubtitleStyles": true })).is_none());
        assert!(subtitle_style(&json!({ "DisableCustomSubtitleStyles": true }), &json!({})).is_none());
    }

    #[test]
    fn the_look_takes_each_value_from_the_first_source_that_sets_it() {
        let find = |props: &[MpvProperty], name: &str| props.iter().find(|p| p.name == name).map(|p| p.value.clone());
        let plugin = SubtitleLook {
            color: Some("#FFFFFF00".into()),
            boxed: Some(true),
            back: Some("#B2000000".into()),
            scale: Some(1.5),
            font: Some("Arial".into()),
            position: Some(90),
        };
        let system = SubtitleLook { color: Some("#FF00FF00".into()), scale: Some(2.), ..Default::default() };
        let own = SubtitleLook { scale: Some(0.8), boxed: Some(false), ..Default::default() };
        let props = subtitle_properties(own, system.clone(), Some(plugin.clone()));
        assert_eq!(find(&props, "sub-scale").as_deref(), Some("0.800"), "own first");
        assert_eq!(find(&props, "sub-color").as_deref(), Some("#FF00FF00"), "then the system");
        assert_eq!(find(&props, "sub-pos").as_deref(), Some("90"), "then the plugin");
        assert_eq!(find(&props, "sub-font").as_deref(), Some("Arial"));
        assert_eq!(find(&props, "sub-border-style").as_deref(), Some("outline-and-shadow"));
        assert_eq!(find(&props, "sub-back-color"), None);
        // The plugin's box with its own colour; a box without one is dark.
        let props = subtitle_properties(SubtitleLook::default(), SubtitleLook::default(), Some(plugin));
        assert_eq!(find(&props, "sub-back-color").as_deref(), Some("#B2000000"));
        let own = SubtitleLook { boxed: Some(true), ..Default::default() };
        let props = subtitle_properties(own, system, None);
        assert_eq!(find(&props, "sub-back-color").as_deref(), Some(DARK_BOX));
        // Nothing set: mpv keeps its own look.
        assert!(subtitle_properties(SubtitleLook::default(), SubtitleLook::default(), None).is_empty());
    }

    #[test]
    fn writes_the_time_as_the_plugin_does() {
        let time = now();
        assert_eq!(time.len(), 24);
        assert!(time.ends_with(".000Z") && time.starts_with("20"));
    }
}
