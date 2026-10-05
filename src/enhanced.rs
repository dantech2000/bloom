// SPDX-License-Identifier: AGPL-3.0-or-later
//! Features of the Jellyfin Enhanced plugin, done in the app: Seerr rows and
//! links on the detail page, the plugin's keyboard shortcuts, automatic pause and skip in the player, and its subtitle style.
//! Each feature follows the setting of the user in the plugin; the profile
//! menu can set it for this app alone.

use std::{collections::HashMap, time::Instant};

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, KeyDownEvent, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, Window, div, px, rgb, rgba,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    app::{Bloom, Page},
    icons::{Filled, filled},
    jellyfin::{Client, Item, SeerrItem},
    macos::{Id, class, send},
    player::PlayState,
    ui::{glass::glass, menu::MenuItem, theme::UiTheme},
    views::{
        cards::{CARD_TEXT_H, Cards, icon, section_padded},
        search::seerr_card,
    },
};

/// The features with a switch, as config key and menu text.
pub const TOGGLES: [(&str, &str); 11] = [
    ("seerr_rows", "Seerr recommendations"),
    ("links", "Seerr, Sonarr and Radarr links"),
    ("rating_in_player", "Ratings in the player"),
    ("auto_pause", "Pause when the window is hidden"),
    ("auto_resume", "Resume when the window returns"),
    ("auto_skip_intro", "Skip intros automatically"),
    ("auto_skip_outro", "Skip credits automatically"),
    ("shortcuts", "Enhanced keyboard shortcuts"),
    ("bookmarks", "Bookmarks"),
    ("hidden_content", "Hidden content"),
    ("request_more", "Request more seasons"),
];

/// Switches that exist only when the server has the feature on: the menu and
/// the settings page leave the others out.
pub const SERVER_GATED: [&str; 3] = ["bookmarks", "hidden_content", "request_more"];

/// Speed change of one key press.
const SPEED_STEP: f32 = 0.25;
/// A position change above this between two looks at the player is a seek.
const JUMP_SECS: f64 = 5.;

#[derive(Clone, Debug, PartialEq)]
pub struct Shortcut {
    /// Name of the action in the plugin, such as "CycleAspectRatio".
    pub name: String,
    /// Key as the plugin writes it: "A", "Shift+H", "/", "0-9".
    pub key: String,
    pub label: String,
    /// The key acts in the player; the others act on the pages.
    pub player: bool,
}

/// Subtitle look of the user, in mpv terms.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubtitleStyle {
    /// Text colour as "#AARRGGBB".
    pub color: String,
    /// Box colour as "#AARRGGBB"; `None` for no box.
    pub back: Option<String>,
    /// Size against the normal size.
    pub scale: f64,
    pub font: Option<String>,
    /// Place from the top, in percent; 100 is the lower edge.
    pub position: i64,
}

/// What the plugin has set for this user.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// Server values of the switches in [`TOGGLES`].
    flags: HashMap<&'static str, bool>,
    pub seerr_similar: bool,
    pub seerr_recommended: bool,
    /// Leaves titles of the library out of the Seerr rows.
    pub seerr_skip_library: bool,
    pub seerr_link: bool,
    pub seerr_url: Option<String>,
    pub arr_links: bool,
    pub shortcuts: Vec<Shortcut>,
    pub subtitle: Option<SubtitleStyle>,
}

/// What the detail page shows on top of the library's own data.
#[derive(Clone, Debug, Default)]
pub struct DetailExtras {
    pub item_id: String,
    pub recommended: Vec<SeerrItem>,
    pub similar: Vec<SeerrItem>,
    /// Name and address of the pages of this title in other tools.
    pub links: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Aspect {
    #[default]
    Auto,
    Cover,
    Fill,
}

#[derive(Default)]
pub struct State {
    pub settings: Settings,
    pub detail: Option<DetailExtras>,
    /// The list of keys is open.
    pub help_open: bool,
    pub bookmarks: crate::bookmarks::BookmarkState,
    pub hidden: crate::hidden::HiddenState,
    pub seasons: crate::seasons::SeasonsState,
    aspect: Aspect,
    /// Item in the player at the last look, and its position then.
    item: String,
    position: f64,
    /// Position before the last seek.
    before_jump: Option<f64>,
    /// Range that was skipped: item and start of the range.
    skipped: Option<(String, i64)>,
    /// The app paused playback because the window was out of view.
    auto_paused: bool,
    visibility_checked: Option<Instant>,
    /// A remote command started the playback and the tick has not seen it.
    remote_pending: bool,
    /// The last local input at the time the tick saw the remote playback.
    /// While no new input came, the user did not touch this playback.
    remote_activity: Option<Instant>,
}

impl Client {
    /// Settings of the user in the Jellyfin Enhanced plugin. `None` when the
    /// server has no such plugin.
    pub fn enhanced_settings(&self) -> Option<Settings> {
        let public: Value = self.get("/JellyfinEnhanced/public-config", &[]).ok()?;
        let user = self.user().ok()?.to_string();
        let own: Value = self
            .get(
                &format!("/JellyfinEnhanced/user-settings/{user}/settings.json"),
                &[],
            )
            .unwrap_or(Value::Null);
        let keys: Value = self
            .get(
                &format!("/JellyfinEnhanced/user-settings/{user}/shortcuts.json"),
                &[],
            )
            .unwrap_or(Value::Null);
        // The user's own value wins over the server default.
        let flag = |name: &str| {
            own.get(name)
                .and_then(Value::as_bool)
                .or_else(|| public.get(name).and_then(Value::as_bool))
                .unwrap_or(false)
        };
        let number = |name: &str, default: i64| {
            own.get(name).and_then(Value::as_i64).unwrap_or(default)
        };
        // These features are the administrator's choice: the user has no
        // value of their own in the settings file.
        let public_flag = |name: &str, default: bool| {
            public.get(name).and_then(Value::as_bool).unwrap_or(default)
        };
        let seerr = flag("JellyseerrEnabled");
        let seerr_similar = seerr && flag("JellyseerrShowSimilar");
        let seerr_recommended = seerr && flag("JellyseerrShowRecommended");
        let seerr_link = seerr && flag("JellyseerrShowDetailPageLink");
        let arr_links = flag("ArrLinksEnabled");
        let flags = HashMap::from([
            ("seerr_rows", seerr_similar || seerr_recommended),
            ("links", seerr_link || arr_links),
            ("rating_in_player", flag("ShowRatingInPlayer")),
            ("auto_pause", flag("AutoPauseEnabled")),
            ("auto_resume", flag("AutoResumeEnabled")),
            ("auto_skip_intro", flag("AutoSkipIntro")),
            ("auto_skip_outro", flag("AutoSkipOutro")),
            ("shortcuts", !flag("DisableAllShortcuts")),
            ("bookmarks", public_flag("BookmarksEnabled", false)),
            ("hidden_content", public_flag("HiddenContentEnabled", false)),
            (
                "request_more",
                seerr && public_flag("JellyseerrShowRequestMoreOnSeries", true),
            ),
        ]);

        let text = |entry: &Value, name: &str| {
            entry
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let mut shortcuts: Vec<Shortcut> = public
            .get("Shortcuts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|entry| Shortcut {
                name: text(entry, "Name"),
                key: text(entry, "Key"),
                label: text(entry, "Label"),
                player: text(entry, "Category") == "Player",
            })
            .collect();
        // Keys the user changed in the plugin's panel.
        for entry in keys
            .get("Shortcuts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (name, key) = (text(entry, "Name"), text(entry, "Key"));
            if let Some(shortcut) = shortcuts.iter_mut().find(|s| s.name == name)
                && !key.is_empty()
            {
                shortcut.key = key;
            }
        }

        let subtitle = (!flag("DisableCustomSubtitleStyles")).then(|| {
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
            const FONTS: [Option<&str>; 5] = [
                None,
                Some("Noto Sans"),
                Some("Arial"),
                Some("Courier New"),
                Some("Roboto Mono"),
            ];
            let pick = |name: &str, default: i64, len: usize| {
                (number(name, default).max(0) as usize).min(len - 1)
            };
            let (mut color, mut back) = {
                let (color, back) = STYLES[pick("SelectedStylePresetIndex", 0, STYLES.len())];
                (color.to_string(), back.to_string())
            };
            if flag("UsingCustomColors") {
                let own_color = |name: &str| {
                    own.get(name)
                        .and_then(Value::as_str)
                        .filter(|c| c.len() == 9 && c.starts_with('#'))
                        .map(str::to_string)
                };
                color = own_color("CustomSubtitleTextColor").unwrap_or(color);
                back = own_color("CustomSubtitleBgColor").unwrap_or(back);
            }
            SubtitleStyle {
                color: mpv_color(&color),
                back: (!back.ends_with("00")).then(|| mpv_color(&back)),
                scale: SIZES[pick("SelectedFontSizePresetIndex", 2, SIZES.len())] / 1.2,
                font: FONTS[pick("SelectedFontFamilyPresetIndex", 0, FONTS.len())]
                    .map(str::to_string),
                // The plugin's default place is 85; mpv's is the lower edge.
                position: (100 + number("SubtitleVerticalPosition", 85) - 85).clamp(0, 100),
            }
        });

        Some(Settings {
            flags,
            seerr_similar,
            seerr_recommended,
            seerr_skip_library: flag("JellyseerrExcludeLibraryItems"),
            seerr_link,
            seerr_url: public
                .get("JellyseerrBaseUrl")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
                .map(|url| url.trim_end_matches('/').to_string()),
            arr_links,
            shortcuts,
            subtitle,
        })
    }

    /// Titles Seerr relates to a title: `list` is "similar" or
    /// "recommendations", `media` is "movie" or "tv".
    fn seerr_related(&self, media: &str, tmdb: &str, list: &str) -> Result<Vec<SeerrItem>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            results: Vec<SeerrItem>,
        }
        let page: Page = self.get(
            &format!("/JellyfinEnhanced/jellyseerr/{media}/{tmdb}/{list}"),
            &[],
        )?;
        Ok(page
            .results
            .into_iter()
            .map(|mut item| {
                // Some lists leave the type out; it is the type asked for.
                if item.media_type.is_empty() {
                    item.media_type = media.to_string();
                }
                item
            })
            .collect())
    }

    /// Pages of a title in Radarr or Sonarr. The server gives them to
    /// administrators only.
    fn arr_links(&self, movie: bool, id: &str) -> Vec<(String, String)> {
        let (param, group) = if movie {
            ("tmdbIds", "movies")
        } else {
            ("tvdbIds", "series")
        };
        let Ok(found) = self.get::<Value>(
            "/JellyfinEnhanced/arr/links",
            &[(param, id.to_string())],
        ) else {
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
                Some((name, url))
            })
            .collect()
    }
}

/// "#RRGGBBAA" as mpv writes a colour: "#AARRGGBB".
fn mpv_color(css: &str) -> String {
    match (css.get(1..7), css.get(7..9)) {
        (Some(rgb), Some(alpha)) => format!("#{alpha}{rgb}"),
        _ => css.to_string(),
    }
}

/// The key of an event as the plugin writes keys: "A", "Shift+H", "/", "+".
fn combo(event: &KeyDownEvent) -> Option<String> {
    let keystroke = &event.keystroke;
    let modifiers = keystroke.modifiers;
    if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
        return None;
    }
    let key = keystroke.key.as_str();
    let mut chars = key.chars();
    let (first, single) = (chars.next()?, chars.next().is_none());
    if single && first.is_ascii_alphabetic() {
        let letter = first.to_ascii_uppercase();
        return Some(if modifiers.shift {
            format!("Shift+{letter}")
        } else {
            letter.to_string()
        });
    }
    // A sign typed with Shift, such as "+" or "?".
    if let Some(typed) = keystroke.key_char.as_deref()
        && typed.chars().count() == 1
        && !typed.chars().all(char::is_alphanumeric)
    {
        return Some(typed.to_string());
    }
    if modifiers.shift {
        return Some(match key {
            "=" => "+".to_string(),
            "/" => "?".to_string(),
            other => format!("Shift+{other}"),
        });
    }
    Some(key.to_string())
}

/// True when a window of the app is in view: not minimized, not hidden, and
/// not fully covered. A browser reports a tab as hidden in the same cases.
fn app_in_view() -> bool {
    /// `NSWindowOcclusionStateVisible`.
    const VISIBLE: usize = 1 << 1;
    let app = send!(Id, class(c"NSApplication"), c"sharedApplication");
    if app.is_null() {
        return true;
    }
    let windows = send!(Id, app, c"windows");
    let count = send!(usize, windows, c"count");
    (0..count).any(|i| {
        let window = send!(Id, windows, c"objectAtIndex:", i => usize);
        send!(usize, window, c"occlusionState") & VISIBLE != 0
    })
}

impl Bloom {
    /// True when a feature of [`TOGGLES`] is on: the choice made in this
    /// app, or else the user's setting in the plugin.
    pub fn je(&self, key: &str) -> bool {
        if SERVER_GATED.contains(&key) {
            return self.enhanced_feature(key);
        }
        self.config.enhanced.get(key).copied().unwrap_or_else(|| {
            self.enhanced
                .settings
                .flags
                .get(key)
                .copied()
                .unwrap_or(false)
        })
    }

    /// True when the server has a feature of [`SERVER_GATED`] on and the user
    /// has not switched it off in this app.
    pub fn enhanced_feature(&self, key: &str) -> bool {
        self.enhanced.settings.flags.get(key).copied().unwrap_or(false)
            && self.config.enhanced.get(key).copied().unwrap_or(true)
    }

    /// True when the switch shows in the menu and in the settings.
    pub fn enhanced_toggle_shown(&self, key: &str) -> bool {
        !SERVER_GATED.contains(&key)
            || self.enhanced.settings.flags.get(key).copied().unwrap_or(false)
    }

    /// Reads the plugin's settings for the open session.
    pub fn load_enhanced(&mut self, cx: &mut Context<Self>) {
        self.enhanced = State::default();
        let Some(opened) = self.session.as_ref().map(|s| s.user_id.clone()) else {
            return;
        };
        self.fetch(
            cx,
            |client| Ok(client.enhanced_settings()),
            move |this, result, cx| {
                if let (Ok(Some(settings)), Some(session)) = (result, this.session.as_ref())
                    && session.user_id == opened
                {
                    this.enhanced.settings = settings;
                    this.load_bookmarks(cx);
                    this.load_hidden(cx);
                    this.rebuild_menu(cx);
                    // A detail page that opened first gets its rows now.
                    this.load_enhanced_detail(cx);
                    cx.notify();
                }
            },
        );
    }

    pub fn toggle_enhanced(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let on = !self.je(key);
        self.config.enhanced.insert(key.to_string(), on);
        self.save_config(cx);
        self.rebuild_menu(cx);
        if matches!(key, "seerr_rows" | "links") {
            self.enhanced.detail = None;
            self.load_enhanced_detail(cx);
        }
        if key == "bookmarks" {
            self.load_bookmarks(cx);
        }
        if key == "request_more" {
            self.enhanced.seasons.probe = None;
            self.load_request_more(cx);
        }
        cx.notify();
    }

    /// The "Jellyfin Enhanced" entry of the profile menu.
    pub fn enhanced_menu(&self, cx: &mut Context<Self>) -> Option<MenuItem> {
        if self.enhanced.settings.flags.is_empty() {
            return None;
        }
        let this = cx.weak_entity();
        let mut entries: Vec<MenuItem> = TOGGLES
            .iter()
            .filter(|(key, _)| self.enhanced_toggle_shown(key))
            .map(|(key, label)| {
                let handle = this.clone();
                let key: &'static str = key;
                MenuItem::new(SharedString::from(format!("menu.je.{key}")), *label)
                    .checked(self.je(key))
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |this, cx| this.toggle_enhanced(key, cx)).ok();
                    })
            })
            .collect();
        entries.push(MenuItem::separator());
        entries.push(
            MenuItem::new("menu.je.keys", "Keyboard shortcuts…").on_click(move |_, _, cx| {
                this.update(cx, |this, cx| {
                    this.enhanced.help_open = true;
                    cx.notify();
                })
                .ok();
            }),
        );
        Some(MenuItem::submenu("menu.je", "Jellyfin Enhanced", entries).icon(LucideIcon::Sparkles))
    }

    // ----- detail page --------------------------------------------------------

    /// Loads the Seerr rows and the links of the open detail page.
    pub fn load_enhanced_detail(&mut self, cx: &mut Context<Self>) {
        self.load_request_more(cx);
        // The web client may have added a bookmark since the last look.
        if matches!(&self.page, Page::Detail(data) if data.item.is_playable()) {
            self.load_bookmarks(cx);
        }
        let Page::Detail(data) = &self.page else {
            return;
        };
        let item = &data.item;
        let movie = match item.kind.as_str() {
            "Movie" => true,
            "Series" => false,
            _ => return,
        };
        if self.enhanced.detail.as_ref().is_some_and(|d| d.item_id == item.id) {
            return;
        }
        let id_of = |name: &str| item.provider_ids.get(name).cloned().flatten();
        let Some(tmdb) = id_of("Tmdb") else { return };
        let tvdb = id_of("Tvdb");
        let settings = self.enhanced.settings.clone();
        let rows = self.je("seerr_rows");
        let links = self.je("links");
        let admin = self.is_admin();
        if !rows && !links {
            return;
        }
        let item_id = item.id.clone();
        let media = if movie { "movie" } else { "tv" };
        self.fetch(
            cx,
            move |client| {
                let list = |on: bool, name: &str| {
                    if !(rows && on) {
                        return Vec::new();
                    }
                    let mut found = client.seerr_related(media, &tmdb, name).unwrap_or_default();
                    if settings.seerr_skip_library {
                        found.retain(|item| item.library_id().is_none());
                    }
                    found
                };
                // The three requests run at the same time.
                let (recommended, similar, arr) = std::thread::scope(|scope| {
                    let recommended =
                        scope.spawn(|| list(settings.seerr_recommended, "recommendations"));
                    let similar = scope.spawn(|| list(settings.seerr_similar, "similar"));
                    let arr = if links && settings.arr_links && admin {
                        match (movie, &tvdb) {
                            (true, _) => client.arr_links(true, &tmdb),
                            (false, Some(tvdb)) => client.arr_links(false, tvdb),
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    (
                        recommended.join().unwrap_or_default(),
                        similar.join().unwrap_or_default(),
                        arr,
                    )
                });
                let mut all_links = Vec::new();
                if let (true, true, Some(base)) = (links, settings.seerr_link, &settings.seerr_url)
                {
                    all_links.push(("Seerr".to_string(), format!("{base}/{media}/{tmdb}")));
                }
                all_links.extend(arr);
                Ok(DetailExtras {
                    item_id,
                    recommended,
                    similar,
                    links: all_links,
                })
            },
            |this, result, cx| {
                if let (Ok(extras), Page::Detail(data)) = (result, &this.page)
                    && data.item.id == extras.item_id
                {
                    this.enhanced.detail = Some(extras);
                    cx.notify();
                }
            },
        );
    }

    fn detail_extras(&self, item: &Item) -> Option<&DetailExtras> {
        self.enhanced.detail.as_ref().filter(|d| d.item_id == item.id)
    }

    /// The links to the title in Seerr, Radarr and Sonarr.
    pub fn enhanced_detail_lines(&self, item: &Item, cx: &mut Context<Self>) -> Option<Div> {
        let t = UiTheme::read(cx).clone();
        let links = self
            .detail_extras(item)
            .map(|extras| extras.links.as_slice())
            .unwrap_or_default();
        let tags = if crate::jellyfin::quality_tags() {
            item.quality_labels()
        } else {
            Vec::new()
        };
        let request_more = self.can_request_more(&item.id);
        if links.is_empty() && tags.is_empty() && !request_more {
            return None;
        }
        let mut block = div()
            .mt(px(2.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .text_size(px(14.))
            .text_color(t.colors.foreground);
        if !tags.is_empty() {
            block = block.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(tags.into_iter().map(crate::quality::pill)),
            );
        }
        if !links.is_empty() || request_more {
            let mut row = div().flex().flex_wrap().gap(px(8.));
            if request_more {
                let series_id = item.id.clone();
                row = row.child(
                    div()
                        .id("detail.je.request-more")
                        .h(px(30.))
                        .px(px(12.))
                        .rounded(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .cursor_pointer()
                        .bg(rgba(0xffffff1f))
                        .hover(|s| s.bg(rgba(0xffffff33)))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(icon(LucideIcon::Download, 14., t.colors.foreground))
                        .child("Request more seasons")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.open_seasons_dialog(series_id.clone(), cx)
                        })),
                );
            }
            for (name, url) in links {
                let url = url.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("detail.je.link.{name}")))
                        .h(px(30.))
                        .px(px(12.))
                        .rounded(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .cursor_pointer()
                        .bg(rgba(0xffffff1f))
                        .hover(|s| s.bg(rgba(0xffffff33)))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(name.clone())
                        .child(icon(LucideIcon::ExternalLink, 14., t.colors.foreground))
                        .on_click(move |_: &ClickEvent, _, cx| crate::macos::open_web_url(cx, &url)),
                );
            }
            block = block.child(row);
        }
        Some(block)
    }

    /// The "Recommended" and "Similar" rows from Seerr.
    pub fn enhanced_detail_rows(
        &self,
        item: &Item,
        left: f32,
        right: f32,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let mut rows = Vec::new();
        let Some(extras) = self.detail_extras(item).filter(|_| self.je("seerr_rows")) else {
            return rows;
        };
        let w = self.metrics().portrait_w;
        for (id, title, items) in [
            ("detail.je.recommended", "Recommended on Seerr", &extras.recommended),
            ("detail.je.similar", "Similar on Seerr", &extras.similar),
        ] {
            if items.is_empty() {
                continue;
            }
            let cards = Cards::new(w, items.len(), |i, cx| seerr_card(&items[i], w, cx));
            rows.push(section_padded(
                self,
                id,
                title,
                None,
                w * 1.5 + CARD_TEXT_H,
                cards,
                left,
                right,
                cx,
            ));
        }
        rows
    }

    // ----- player --------------------------------------------------------------

    /// Community and critic rating of the item in the player.
    pub fn enhanced_player_rating(&self) -> Option<Div> {
        let item = self.playing.as_ref().filter(|_| self.je("rating_in_player"))?;
        let community = item.community_rating.filter(|r| *r > 0.);
        let critic = item.critic_rating.filter(|r| *r > 0.);
        if community.is_none() && critic.is_none() {
            return None;
        }
        let white = rgba(0xffffffde);
        let mut line = div()
            .flex_shrink_0()
            .ml(px(10.))
            .flex()
            .items_center()
            .gap(px(5.))
            .text_size(px(14.))
            .text_color(white);
        if let Some(rating) = community {
            line = line
                .child(filled(Filled::Star, 14., rgb(0xf2b01e)))
                .child(format!("{rating:.1}"));
        }
        if let Some(rating) = critic {
            // The tomato is fresh from 60 percent up.
            let color = if rating >= 60. { rgb(0xfa320a) } else { rgb(0x7bb928) };
            line = line
                .child(div().ml(px(6.)).size(px(10.)).rounded_full().bg(color))
                .child(format!("{rating:.0}%"));
        }
        Some(line)
    }

    /// Sends the user's subtitle look to the player.
    pub fn apply_subtitle_style(&self) {
        let look = &self.config.subtitle_look;
        let plugin = self.enhanced.settings.subtitle.clone();
        if plugin.is_none() && *look == crate::config::SubtitleLook::default() {
            return;
        }
        // The look set in this app goes over the style of the plugin.
        let style = plugin.unwrap_or(SubtitleStyle {
            color: "#FFFFFFFF".to_string(),
            back: None,
            scale: 1.,
            font: None,
            position: 100,
        });
        let color = look.color.clone().unwrap_or(style.color);
        let back = match look.background {
            Some(true) => Some("#B3000000".to_string()),
            Some(false) => None,
            None => style.back,
        };
        self.player.set_property("sub-color", &color);
        match &back {
            Some(back) => {
                self.player.set_property("sub-back-color", back);
                self.player.set_property("sub-border-style", "background-box");
            }
            None => self.player.set_property("sub-border-style", "outline-and-shadow"),
        }
        let scale = look.scale.unwrap_or(style.scale);
        self.player.set_property("sub-scale", &format!("{scale:.3}"));
        let position = look.position.unwrap_or(style.position);
        self.player.set_property("sub-pos", &position.to_string());
        if let Some(font) = &style.font {
            self.player.set_property("sub-font", font);
        }
    }

    /// Work that follows the player: the position before a seek, automatic
    /// skip, and pause while the window is out of view.
    pub fn enhanced_tick(&mut self, cx: &mut Context<Self>) {
        // In a group nothing moves the player by itself: a skip or a pause
        // of one member would be one for all.
        if self.sync.following() {
            return;
        }
        if self.player_status.state != PlayState::Playing {
            return;
        }
        let Some(item_id) = self.playing.as_ref().map(|i| i.id.clone()) else {
            return;
        };
        let position = self.player_status.position;
        if self.enhanced.item != item_id {
            self.enhanced.item = item_id.clone();
            self.enhanced.before_jump = None;
            self.enhanced.auto_paused = false;
            // A new item starts with the picture as it is.
            if self.enhanced.aspect != Aspect::Auto {
                self.set_aspect(Aspect::Auto);
            }
        } else if (position - self.enhanced.position).abs() > JUMP_SECS
            && self.enhanced.position > 0.
        {
            self.enhanced.before_jump = Some(self.enhanced.position);
        }
        self.enhanced.position = position;

        // Skips a marked intro or credits range once.
        if let Some(segment) = self.current_segment() {
            let wanted = match segment.kind.as_str() {
                "Intro" => self.je("auto_skip_intro"),
                "Outro" => self.je("auto_skip_outro"),
                _ => false,
            };
            let mark = (item_id, (segment.start_secs() * 1000.) as i64);
            if wanted && self.enhanced.skipped.as_ref() != Some(&mark) {
                let (end, intro) = (segment.end_secs(), segment.kind == "Intro");
                self.enhanced.skipped = Some(mark);
                self.player.seek_absolute(end);
                self.toast(if intro { "Skipped intro" } else { "Skipped credits" }, "", cx);
            }
        }

        // Half a second is soon enough for a window that went out of view.
        let due = self
            .enhanced
            .visibility_checked
            .is_none_or(|at| at.elapsed().as_millis() >= 500);
        let activity = self.last_activity();
        if std::mem::take(&mut self.enhanced.remote_pending) {
            self.enhanced.remote_activity = Some(activity);
        }
        if !due || !(self.je("auto_pause") || self.enhanced.auto_paused) || !auto_pause_allowed() {
            return;
        }
        self.enhanced.visibility_checked = Some(Instant::now());
        let in_view = app_in_view();
        let paused = self.player_status.paused;
        let wanted = auto_pause_wanted(
            self.je("auto_pause"),
            self.enhanced.remote_activity,
            activity,
        );
        if !in_view && !paused && wanted {
            self.player.toggle_pause();
            self.enhanced.auto_paused = true;
        } else if in_view && self.enhanced.auto_paused {
            self.enhanced.auto_paused = false;
            if paused && self.je("auto_resume") {
                self.player.toggle_pause();
            }
        }
    }

    fn set_aspect(&mut self, aspect: Aspect) {
        self.enhanced.aspect = aspect;
        let (panscan, keep) = match aspect {
            Aspect::Auto => ("0", "yes"),
            Aspect::Cover => ("1", "yes"),
            Aspect::Fill => ("0", "no"),
        };
        self.player.set_property("panscan", panscan);
        self.player.set_property("keepaspect", keep);
    }

    /// Name of the plugin action bound to a key, for the player or the pages.
    fn enhanced_action(&self, event: &KeyDownEvent, player: bool) -> Option<String> {
        if !self.je("shortcuts") {
            return None;
        }
        let pressed = combo(event)?;
        let digit = pressed.len() == 1 && pressed.chars().all(|c| c.is_ascii_digit());
        self.enhanced
            .settings
            .shortcuts
            .iter()
            .filter(|s| s.player == player)
            .find(|s| s.key.eq_ignore_ascii_case(&pressed) || (digit && s.key == "0-9"))
            .map(|s| match (s.key == "0-9", digit) {
                (true, true) => format!("{}:{pressed}", s.name),
                _ => s.name.clone(),
            })
    }

    /// Runs a player action of the plugin by name. False for an action the
    /// app does not have.
    pub fn enhanced_player_action(&mut self, action: &str, cx: &mut Context<Self>) -> bool {
        let (name, argument) = action.split_once(':').unwrap_or((action, ""));
        match name {
            "CycleAspectRatio" => {
                let next = match self.enhanced.aspect {
                    Aspect::Auto => Aspect::Cover,
                    Aspect::Cover => Aspect::Fill,
                    Aspect::Fill => Aspect::Auto,
                };
                self.set_aspect(next);
                self.toast(format!("Aspect ratio: {next:?}"), "", cx);
            }
            "ShowPlaybackInfo" => self.toggle_playback_info(cx),
            "CycleSubtitleTracks" => self.player.command("cycle", &["sub"]),
            "CycleAudioTracks" => self.player.command("cycle", &["audio"]),
            "IncreasePlaybackSpeed" | "DecreasePlaybackSpeed" | "ResetPlaybackSpeed" => {
                let speed = match name {
                    "IncreasePlaybackSpeed" => (self.speed + SPEED_STEP).min(4.),
                    "DecreasePlaybackSpeed" => (self.speed - SPEED_STEP).max(0.25),
                    _ => 1.,
                };
                self.set_speed(speed, cx);
                self.toast(format!("Speed: {speed}x"), "", cx);
            }
            "SkipIntroOutro" => match self.current_segment().map(|s| s.end_secs()) {
                Some(end) => self.request_seek_to(end, cx),
                None => return false,
            },
            "BookmarkCurrentTime" => match self.playing.as_ref().map(|i| i.id.clone()) {
                Some(id) if self.bookmarks_on() => {
                    self.add_bookmark(id, self.player_status.position, String::new(), cx)
                }
                _ => return false,
            },
            "FrameStepBack" => self.player.command("frame-back-step", &[]),
            "FrameStepForward" => self.player.command("frame-step", &[]),
            "JumpToLastPosition" => match self.enhanced.before_jump {
                Some(position) => self.request_seek_to(position, cx),
                None => return false,
            },
            "JumpToPercentage" => {
                let Ok(tenth) = argument.parse::<f64>() else {
                    return false;
                };
                self.player
                    .seek_absolute(self.player_status.duration * tenth / 10.);
            }
            _ => return false,
        }
        true
    }

    /// A key in the player that no key of the app took. True when a plugin
    /// shortcut handled it.
    pub fn enhanced_player_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        match self.enhanced_action(event, true) {
            Some(action) => self.enhanced_player_action(&action, cx),
            None => false,
        }
    }

    /// A key on the pages, before the keys of the app. True when the list of
    /// keys or a plugin shortcut handled it.
    pub fn enhanced_shell_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.enhanced.help_open {
            // Any key closes the list.
            self.enhanced.help_open = false;
            return true;
        }
        if combo(event).as_deref() == Some("?") && self.je("shortcuts") {
            self.enhanced.help_open = true;
            return true;
        }
        let Some(action) = self.enhanced_action(event, false) else {
            return false;
        };
        match action.as_str() {
            "GoToDashboard" if self.is_admin() => {
                self.open_admin(crate::admin::Section::Dashboard, cx)
            }
            "QuickConnect" => self.open_authorize(window, cx),
            "GoToHome" => self.open_home(cx),
            "PlayRandomItem" => self.open_random(cx),
            // "OpenSearch" stays with the app's own key.
            _ => return false,
        }
        true
    }

    /// The list of keys, over the page.
    pub fn render_enhanced_help(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !self.enhanced.help_open {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let key_row = |keys: &str, label: &str| {
            div()
                .h(px(30.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(16.))
                .text_size(px(14.))
                .text_color(t.colors.foreground.opacity(0.85))
                .child(label.to_string())
                .child(
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(6.))
                        .bg(rgba(0xffffff1f))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground)
                        .child(keys.to_string()),
                )
        };
        let column = |title: &str, rows: Vec<Div>| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .mb(px(8.))
                        .text_size(px(13.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground.opacity(0.5))
                        .child(title.to_string()),
                )
                .children(rows)
        };
        let shortcuts = &self.enhanced.settings.shortcuts;
        // Actions the app does not have are left out of the list.
        const MISSING: [&str; 2] = ["SubtitleMenu", "OpenEpisodePreview"];
        let rows_of = |player: bool| -> Vec<Div> {
            shortcuts
                .iter()
                .filter(|s| s.player == player && !MISSING.contains(&s.name.as_str()))
                .filter(|s| s.name != "BookmarkCurrentTime" || self.bookmarks_on())
                .filter(|s| player || s.name != "GoToDashboard" || self.is_admin())
                .map(|s| key_row(&s.key, &s.label))
                .collect()
        };
        let mut pages = rows_of(false);
        pages.push(key_row("?", "This list"));
        pages.push(key_row("Esc", "Back"));
        let mut player = vec![
            key_row("Space", "Play or pause"),
            key_row("← →", "Seek 5 seconds"),
            key_row("J L", "Seek 10 seconds"),
            key_row("↑ ↓", "Volume"),
            key_row("M", "Mute"),
            key_row("Z Shift+Z", "Subtitle delay"),
            key_row("Ctrl - Ctrl +", "Audio delay"),
            key_row("F", "Full screen"),
            key_row("Shift+N Shift+P", "Next or previous item"),
        ];
        // The first plugin keys fill the second column; the rest get a third.
        let mut enhanced = rows_of(true);
        let rest = enhanced.split_off(enhanced.len().min(4));
        player.extend(enhanced);
        Some(
            div()
                .id("je.help")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.enhanced.help_open = false;
                    cx.notify();
                }))
                .child(
                    div()
                        .relative()
                        .w(px(860.))
                        .max_w_full()
                        .rounded(px(24.))
                        .border_1()
                        .border_color(rgba(0xf5f5f733))
                        .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
                        .p(px(24.))
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .child(
                            div()
                                .text_size(px(20.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child("Keyboard shortcuts"),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(28.))
                                .child(column("Pages", pages))
                                .child(column("Player", player))
                                .child(column("Player, from Jellyfin Enhanced", rest)),
                        ),
                ),
        )
    }

    /// The verbs of `je` for bookmarks, hidden content, seasons and tags. The
    /// answer is the text for the debug channel; `None` when the verb is not
    /// one of these. No verb sends a Seerr request.
    pub fn debug_je(
        &mut self,
        verb: &str,
        argument: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let argument = argument.trim();
        let client = || self.session.as_ref().map(|s| s.client.clone());
        Some(match verb {
            // Puts the window away, so the auto-pause can be tested.
            "minimize" => {
                window.minimize_window();
                "minimized".to_string()
            }
            "bookmarks" => self.bookmarks_debug(argument),
            "bookmark-add" => self.bookmark_add_debug(argument, cx),
            "bookmark-remove" => self.bookmark_remove_debug(argument, cx),
            "bookmark-jump" => self.bookmark_jump_debug(argument, cx),
            "bookmarks-panel" => {
                self.toggle_bookmarks_panel(window, cx);
                format!("panel_open={}", self.enhanced.bookmarks.panel_open)
            }
            "hidden" => self.hidden_debug(),
            // Hidden content on in this app only, with no call to the server.
            "hidden-demo" => {
                self.enhanced.hidden.demo = argument != "off";
                if !self.enhanced.hidden.demo {
                    self.enhanced.hidden = Default::default();
                }
                self.rebuild_detail_menus(cx);
                self.hidden_debug()
            }
            "hide" | "unhide" => {
                let Some(client) = client() else {
                    return Some("error: not signed in".into());
                };
                let item = match client.item(argument) {
                    Ok(item) => item,
                    Err(err) => return Some(format!("error: {err:#}")),
                };
                if verb == "hide" {
                    self.hide_items(vec![crate::hidden::HideRequest::of(&item)], cx);
                } else {
                    self.unhide_item(item.id.clone(), cx);
                }
                format!("sent: {verb} {} ({})", item.id, item.name)
            }
            "seasons" => self.seasons_debug(argument),
            "request-dialog" => {
                if argument.is_empty() {
                    self.request_dialog_debug()
                } else {
                    self.open_seasons_dialog(argument.to_string(), cx);
                    "opened".to_string()
                }
            }
            "request-close" => {
                self.close_seasons_dialog(cx);
                self.request_dialog_debug()
            }
            "tags" => {
                let Some(client) = client() else {
                    return Some("error: not signed in".into());
                };
                match client.item(argument) {
                    Ok(item) => {
                        let input = crate::quality::Input::of(&item);
                        let raw = crate::quality::detect(&input);
                        let shown = crate::quality::arrange(&raw, &crate::quality::prefs());
                        format!(
                            "{}: derived={raw:?} shown={shown:?} master={} prefs={:?}",
                            item.name,
                            crate::jellyfin::quality_tags(),
                            crate::quality::prefs()
                        )
                    }
                    Err(err) => format!("error: {err:#}"),
                }
            }
            // A category the plugin has off, shown anyway, to look at it.
            "tags-force" => match crate::quality::force_categories(argument) {
                Ok(()) => {
                    cx.notify();
                    format!("forced: {:?}", crate::quality::prefs())
                }
                Err(err) => format!("error: {err}"),
            },
            _ => return None,
        })
    }

    /// State of the plugin features, for the debug channel.
    pub fn enhanced_debug(&self) -> String {
        let flags: Vec<String> = TOGGLES
            .iter()
            .map(|(key, _)| format!("{key}={}", self.je(key)))
            .collect();
        let detail = self.enhanced.detail.as_ref().map(|d| {
            format!(
                "recommended={} similar={} links={:?}",
                d.recommended.len(),
                d.similar.len(),
                d.links.iter().map(|(name, _)| name).collect::<Vec<_>>()
            )
        });
        format!(
            "{} | shortcuts={} subtitle={:?} | detail: {} | aspect={:?} before_jump={:?} \
             in_view={} auto_paused={} remote_hold={} speed={} bookmarks_panel={}",
            flags.join(" "),
            self.enhanced.settings.shortcuts.len(),
            self.enhanced.settings.subtitle,
            detail.unwrap_or_else(|| "none".to_string()),
            self.enhanced.aspect,
            self.enhanced.before_jump,
            app_in_view(),
            self.enhanced.auto_paused,
            self.enhanced.remote_activity == Some(self.last_activity()),
            self.speed,
            self.enhanced.bookmarks.panel_open,
        )
    }
}

/// Whether the app pauses a playback that goes out of view. A playback that
/// a remote command started (`remote_activity` is the last local input when
/// it began) does not pause until the user gives a new local input: the
/// window of a remote-controlled app is often hidden on purpose.
fn auto_pause_wanted(setting: bool, remote_activity: Option<Instant>, activity: Instant) -> bool {
    setting && remote_activity != Some(activity)
}

impl Bloom {
    /// A remote command starts a playback here.
    pub fn enhanced_remote_started(&mut self) {
        self.enhanced.remote_pending = true;
        self.enhanced.remote_activity = None;
    }
}

/// A test instance often sits behind other windows; it does not pause for
/// that, unless the test asks for it.
fn auto_pause_allowed() -> bool {
    std::env::var_os("BLOOM_CONFIG_READONLY").is_none()
        || std::env::var_os("BLOOM_AUTO_PAUSE").is_some()
}

#[cfg(test)]
mod auto_pause_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_remote_playback_is_not_paused_until_the_user_acts() {
        let t0 = Instant::now();
        assert!(auto_pause_wanted(true, None, t0));
        assert!(!auto_pause_wanted(true, Some(t0), t0));
        // A new input of the user ends the protection.
        assert!(auto_pause_wanted(true, Some(t0), t0 + Duration::from_secs(1)));
        // The setting still decides.
        assert!(!auto_pause_wanted(false, None, t0));
    }
}
