// SPDX-License-Identifier: AGPL-3.0-or-later
//! Settings of the signed-in user: profile, playback, subtitles, home and
//! display. The server keeps most of them, in the user's configuration and
//! in the display preferences the web client also uses; a few belong to this
//! app and are in its config file. A change is saved at once. The look
//! follows the Abyss theme's rules for the web client's settings pages.

mod sections;

use std::rc::Rc;

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Point, SharedString, Stateful, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px, rgb, rgba,
};
use serde_json::Value;

use crate::{
    app::{Bloom, Page},
    jellyfin::Client,
    ui::{menu::MenuItem, scroll_area::ScrollArea, theme::UiTheme},
    views::cards::icon,
};

/// Width of the section list at the left.
const NAV_W: f32 = 264.;
/// The theme's dark text on an accent fill.
const INK: u32 = 0x121212;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Profile,
    Playback,
    Subtitles,
    Home,
    Display,
    Downloads,
    /// Titles the user hid (Jellyfin Enhanced); shown only when the server
    /// has hidden content on.
    Hidden,
    /// What Bloom is, its license, and the projects it is made with.
    About,
}

impl Section {
    pub const ALL: [Section; 8] = [
        Section::Profile,
        Section::Playback,
        Section::Subtitles,
        Section::Home,
        Section::Display,
        Section::Downloads,
        Section::Hidden,
        Section::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Profile => "Profile",
            Section::Playback => "Playback",
            Section::Subtitles => "Subtitles",
            Section::Home => "Home",
            Section::Display => "Display",
            Section::Downloads => "Downloads",
            Section::Hidden => "Hidden content",
            Section::About => "About",
        }
    }

    fn glyph(self) -> LucideIcon {
        match self {
            Section::Profile => LucideIcon::User,
            Section::Playback => LucideIcon::CirclePlay,
            Section::Subtitles => LucideIcon::Captions,
            Section::Home => LucideIcon::House,
            Section::Display => LucideIcon::Monitor,
            Section::Downloads => LucideIcon::Download,
            Section::Hidden => LucideIcon::EyeOff,
            Section::About => LucideIcon::Info,
        }
    }

    /// Section of a name such as "playback" (for the debug channel).
    pub fn from_name(name: &str) -> Option<Self> {
        let wanted = name.trim().to_lowercase();
        Section::ALL
            .into_iter()
            .find(|section| section.label().to_lowercase() == wanted)
    }
}

/// What the server keeps for the user. The whole objects stay as the server
/// sent them, so a save changes one field and loses none.
#[derive(Default)]
pub struct Prefs {
    pub loaded: bool,
    /// `Configuration` of the user.
    pub configuration: Value,
    /// Display preferences "usersettings" of the web client; its
    /// `CustomPrefs` hold the home sections and the skip lengths.
    pub display: Value,
    pub has_password: bool,
    pub last_login: Option<String>,
}

impl Prefs {
    pub fn flag(&self, key: &str, default: bool) -> bool {
        self.configuration
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }

    pub fn text(&self, key: &str) -> String {
        self.configuration
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    pub fn custom(&self, key: &str) -> Option<&str> {
        self.display.get("CustomPrefs")?.get(key)?.as_str()
    }

    /// The next episode starts by itself when one ends.
    pub fn auto_play_next(&self) -> bool {
        self.flag("EnableNextEpisodeAutoPlay", true)
    }

    /// The "Up next" card shows near the end of an item.
    pub fn up_next_card(&self) -> bool {
        self.custom("enableNextVideoInfoOverlay")
            .is_none_or(|value| !value.eq_ignore_ascii_case("false"))
    }

    fn millis(&self, key: &str, default: f64) -> f64 {
        self.custom(key)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|millis| *millis >= 1000.)
            .map_or(default, |millis| millis / 1000.)
    }

    /// Seconds the back button of the player goes.
    pub fn skip_back_secs(&self) -> f64 {
        self.millis("skipBackLength", 10.)
    }

    /// Seconds the forward button of the player goes.
    pub fn skip_forward_secs(&self) -> f64 {
        self.millis("skipForwardLength", 30.)
    }

    /// Kind of the home section in a slot, as the web client names it.
    pub fn home_section(&self, slot: usize) -> String {
        self.custom(&format!("homesection{slot}"))
            .map(str::to_string)
            .unwrap_or_else(|| {
                crate::jellyfin::DEFAULT_HOME_SECTIONS
                    .get(slot)
                    .copied()
                    .unwrap_or("none")
                    .to_string()
            })
    }
}

/// What one load of the settings brings.
struct Loaded {
    configuration: Value,
    display: Value,
    has_password: bool,
    last_login: Option<String>,
}

impl Client {
    fn user_prefs(&self) -> Result<Loaded> {
        let user = self.user()?.to_string();
        let (me, display) = std::thread::scope(|scope| {
            let me = scope.spawn(|| self.get::<Value>("/Users/Me", &[]));
            let display = self.get::<Value>(
                "/DisplayPreferences/usersettings",
                &[("userId", user.clone()), ("client", "emby".to_string())],
            );
            (me.join().expect("user thread"), display)
        });
        let me = me?;
        Ok(Loaded {
            configuration: me.get("Configuration").cloned().unwrap_or_default(),
            // The app works without them; the pages then show the defaults.
            display: display.unwrap_or_default(),
            has_password: me.get("HasPassword").and_then(Value::as_bool).unwrap_or(true),
            last_login: me
                .get("LastLoginDate")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    fn save_user_configuration(&self, configuration: &Value) -> Result<()> {
        let user = self.user()?;
        self.post(&format!("/Users/Configuration?userId={user}"), configuration)
            .map(drop)
    }

    fn save_display_preferences(&self, display: &Value) -> Result<()> {
        let user = self.user()?;
        self.post(
            &format!("/DisplayPreferences/usersettings?userId={user}&client=emby"),
            display,
        )
        .map(drop)
    }

    fn change_password(&self, current: &str, new: &str) -> Result<()> {
        let user = self.user()?;
        self.post(
            &format!("/Users/Password?userId={user}"),
            &serde_json::json!({ "CurrentPw": current, "NewPw": new }),
        )
        .map(drop)
        // The error text of `post` holds the path only, never the passwords.
    }
}

/// One entry of the list a select opens.
pub struct Choice {
    pub label: String,
    pub selected: bool,
    pub apply: Rc<dyn Fn(&mut Bloom, &mut Context<Bloom>)>,
}

impl Choice {
    pub fn new(
        label: impl Into<String>,
        selected: bool,
        apply: impl Fn(&mut Bloom, &mut Context<Bloom>) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            selected,
            apply: Rc::new(apply),
        }
    }
}

impl Bloom {
    /// Reads the user's settings from the server. A session that opens calls
    /// this, because playback follows some of them.
    pub fn load_prefs(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.session.as_ref().map(|s| s.user_id.clone()) else {
            return;
        };
        self.fetch(
            cx,
            |client| client.user_prefs(),
            move |this, result, cx| {
                if this.session.as_ref().map(|s| &s.user_id) != Some(&opened) {
                    return;
                }
                match result {
                    Ok(loaded) => {
                        this.prefs = Prefs {
                            loaded: true,
                            configuration: loaded.configuration,
                            display: loaded.display,
                            has_password: loaded.has_password,
                            last_login: loaded.last_login,
                        };
                    }
                    Err(err) => log::warn!("user settings failed: {err:#}"),
                }
                cx.notify();
            },
        );
    }

    pub fn open_settings(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        match &mut self.page {
            Page::Settings(open) => {
                *open = section;
                self.page_scroll
                    .set_offset(gpui_kit::point(px(0.), px(0.)));
                cx.notify();
            }
            _ => self.navigate(Page::Settings(section), cx),
        }
    }

    /// Changes one field of the user's configuration on the server. The page
    /// shows the new value at once; a failed save puts the old one back.
    pub fn set_user_config(&mut self, key: &'static str, value: Value, cx: &mut Context<Self>) {
        if !self.prefs.loaded || !self.prefs.configuration.is_object() {
            return;
        }
        let before = self.prefs.configuration.clone();
        self.prefs.configuration[key] = value;
        let body = self.prefs.configuration.clone();
        cx.notify();
        self.fetch(
            cx,
            move |client| client.save_user_configuration(&body),
            move |this, result, cx| {
                if let Err(err) = result {
                    this.prefs.configuration = before;
                    this.toast("The setting was not saved", format!("{err:#}"), cx);
                    cx.notify();
                }
            },
        );
    }

    /// Changes values of the display preferences on the server, the same
    /// way: shown at once, put back when the save fails.
    pub fn set_custom_prefs(&mut self, changes: Vec<(String, String)>, cx: &mut Context<Self>) {
        if !self.prefs.loaded || !self.prefs.display.is_object() {
            return;
        }
        let before = self.prefs.display.clone();
        if !self.prefs.display["CustomPrefs"].is_object() {
            self.prefs.display["CustomPrefs"] = serde_json::json!({});
        }
        for (key, value) in changes {
            self.prefs.display["CustomPrefs"][key] = Value::String(value);
        }
        let body = self.prefs.display.clone();
        cx.notify();
        self.fetch(
            cx,
            move |client| client.save_display_preferences(&body),
            move |this, result, cx| {
                if let Err(err) = result {
                    this.prefs.display = before;
                    this.toast("The setting was not saved", format!("{err:#}"), cx);
                    cx.notify();
                }
            },
        );
    }

    /// Changes the look of the subtitles in this app. A video that plays
    /// shows the change at once.
    pub fn set_subtitle_look(
        &mut self,
        change: impl FnOnce(&mut crate::config::SubtitleLook),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.config.subtitle_look);
        self.save_config(cx);
        self.apply_subtitle_style();
        cx.notify();
    }

    /// Asks for the current and the new password, then changes it.
    pub fn ask_new_password(&mut self, cx: &mut Context<Self>) {
        use crate::admin::{Field, Prompt};
        let field = |label: &str, required: bool| Field {
            label: label.to_string(),
            placeholder: label.to_string(),
            value: String::new(),
            masked: true,
            required,
        };
        self.ask_prompt(
            Prompt {
                title: "Change password".to_string(),
                message: "Other devices stay signed in.".to_string(),
                action: "Change password".to_string(),
                // An account without a password has no current one.
                fields: vec![
                    field("Current password", self.prefs.has_password),
                    field("New password", true),
                ],
                run: Rc::new(|this, values, cx| {
                    let (current, new) = (values[0].clone(), values[1].clone());
                    this.fetch(
                        cx,
                        move |client| client.change_password(&current, &new),
                        |this, result, cx| match result {
                            Ok(()) => {
                                this.prefs.has_password = true;
                                this.toast("Password changed", "", cx)
                            }
                            Err(_) => this.toast(
                                "The password was not changed",
                                "Check the current password and try again.",
                                cx,
                            ),
                        },
                    );
                }),
            },
            cx,
        );
    }

    /// Opens the list of a select at the pointer.
    pub fn open_choices(
        &mut self,
        choices: Vec<Choice>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.weak_entity();
        let items: Vec<MenuItem> = choices
            .into_iter()
            .enumerate()
            .map(|(n, choice)| {
                let handle = this.clone();
                MenuItem::new(SharedString::from(format!("settings.choice.{n}")), choice.label)
                    .radio(choice.selected)
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |this, cx| (choice.apply)(this, cx)).ok();
                    })
            })
            .collect();
        self.card_menu.update(cx, |menu, cx| {
            menu.set_items(items, cx);
            menu.open_at(Some(position), window, cx);
        });
    }

    pub fn render_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Settings(open) = &self.page else {
            unreachable!()
        };
        let open = *open;
        let t = UiTheme::read(cx).clone();
        // A narrow window has no room for the list beside the page; the
        // sections then show as a row of pills over it.
        let narrow = self.viewport_w < 860.;

        let nav_item = |section: Section, cx: &mut Context<Self>| {
            let selected = section == open;
            let ink = if selected { rgb(INK) } else { t.colors.foreground };
            // The theme's `.navMenuOption`: rounded row, round icon disc,
            // accent fill and dark text when selected or under the pointer.
            div()
                .id(SharedString::from(format!("settings.nav.{section:?}")))
                .group("settings-nav")
                .mb(px(2.))
                .px(px(14.))
                .py(px(8.))
                .rounded(px(12.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(12.))
                .cursor_pointer()
                .text_size(px(17.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(ink)
                .when(selected, |el| el.bg(t.colors.primary))
                .when(!selected, |el| el.hover(|s| s.bg(rgba(0xf5f5f71f))))
                .child(
                    div()
                        .size(px(38.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(if selected { rgba(0x12121226) } else { rgba(0xcccccf69) })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(section.glyph(), 18., ink)),
                )
                .child(section.label())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.open_settings(section, cx)
                }))
        };
        let mut nav = div().flex().flex_shrink_0();
        nav = if narrow {
            nav.w_full().flex_wrap().gap(px(6.)).px(px(20.)).pb(px(10.))
        } else {
            nav.w(px(NAV_W)).h_full().flex_col().px(px(12.)).pt(px(8.))
        };
        for section in Section::ALL {
            if section == Section::Hidden && !self.hidden_on() {
                continue;
            }
            nav = nav.child(nav_item(section, cx));
        }

        let page = match open {
            _ if !self.prefs.loaded => div()
                .py(px(40.))
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child("Loading…"),
            Section::Profile => self.render_settings_profile(cx),
            Section::Playback => self.render_settings_playback(cx),
            Section::Subtitles => self.render_settings_subtitles(cx),
            Section::Home => self.render_settings_home(cx),
            Section::Display => self.render_settings_display(cx),
            Section::Downloads => self.render_settings_downloads(cx),
            Section::Hidden => self.render_settings_hidden(cx),
            Section::About => self.render_settings_about(cx),
        };

        let content = div().flex_1().min_w_0().h_full().child(
            ScrollArea::new("settings.scroll")
                .track(&self.page_scroll)
                .size_full()
                .child(
                    div()
                        .max_w(px(880.))
                        .px(px(if narrow { 20. } else { 28. }))
                        .pt(px(8.))
                        .pb(px(72.))
                        .flex()
                        .flex_col()
                        .gap(px(18.))
                        // The theme sets the title of a settings page large
                        // and at weight 600.
                        .child(
                            div()
                                .text_size(px(30.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child(open.label()),
                        )
                        .child(page),
                ),
        );

        div()
            .relative()
            .size_full()
            .flex()
            .when(narrow, |el| el.flex_col())
            .child(nav)
            .child(content)
            .children(self.render_confirm(cx))
            .children(self.render_prompt(cx))
    }
}

// ----- widgets, in the look of the theme ---------------------------------------

/// A group of settings: the theme's `.verticalSection` on a preferences
/// page (tinted glass, 32 px corners, thin light border).
pub fn group(title: impl Into<SharedString>, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .rounded(px(32.))
        .border_1()
        .border_color(rgba(0xf5f5f733))
        .bg(rgba(0x2a2a2ab0))
        .px(px(22.))
        .pt(px(18.))
        .pb(px(10.))
        .flex()
        .flex_col()
        .child(
            div()
                .mb(px(6.))
                .text_size(px(20.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child(title.into()),
        )
}

/// One setting: its label (weight 500, as `.inputLabel`) and description
/// (`.fieldDescription`) at the left, its control at the right.
pub fn field(
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &Context<Bloom>,
) -> Div {
    let t = UiTheme::read(cx);
    let description: SharedString = description.into();
    div()
        .py(px(11.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(20.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(t.colors.foreground)
                        .child(label.into()),
                )
                .when(!description.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(t.colors.foreground.opacity(0.6))
                            .child(description),
                    )
                }),
        )
        .child(div().flex_shrink_0().child(control))
}

/// The theme's checkbox: 8 px corners, a faint accent outline, and an accent
/// fill with a dark mark when checked. Add `.on_click(...)`.
pub fn checkbox(id: impl Into<SharedString>, checked: bool, cx: &Context<Bloom>) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    div()
        .id(id.into())
        .size(px(26.))
        .rounded(px(8.))
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(checked, |el| {
            el.bg(t.colors.primary)
                .border_color(t.colors.primary)
                .child(icon(LucideIcon::Check, 18., rgb(INK)))
        })
        .when(!checked, |el| {
            el.border_color(rgba(0xf5f5f74d))
                .hover(|s| s.border_color(rgba(0xf5f5f799)))
        })
}

/// The theme's select: 8 px corners and a dark outline, the value at the
/// left and the arrow at the right. `longest` is the longest option, so the
/// field is as wide as its list needs. Add `.on_click(...)`.
pub fn select(
    id: impl Into<SharedString>,
    value: impl Into<SharedString>,
    longest: &str,
    cx: &Context<Bloom>,
) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    let width = (longest.chars().count() as f32 * 8.2 + 58.).clamp(120., 340.);
    div()
        .id(id.into())
        .w(px(width))
        .h(px(38.))
        .pl(px(12.))
        .pr(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(rgba(0x282828cc))
        .bg(rgba(0x00000059))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .cursor_pointer()
        .text_size(px(15.))
        .text_color(t.colors.foreground)
        .hover(|s| s.border_color(rgba(0xf5f5f766)))
        .child(div().min_w_0().truncate().child(value.into()))
        .child(icon(LucideIcon::ChevronDown, 18., t.colors.foreground.opacity(0.7)))
}

/// The theme's `.raised` button: dark fill; accent fill and dark, bold text
/// under the pointer. Add `.on_click(...)`.
pub fn raised(id: impl Into<SharedString>, label: impl Into<SharedString>, cx: &Context<Bloom>) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    div()
        .id(id.into())
        .h(px(38.))
        .px(px(16.))
        .rounded(px(12.))
        .bg(rgba(0x282828cc))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .cursor_pointer()
        .text_size(px(15.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(t.colors.foreground)
        .hover(|s| {
            s.bg(t.colors.primary)
                .text_color(rgb(INK))
                .font_weight(gpui_kit::FontWeight::BOLD)
        })
        .child(label.into())
}

/// A small square button with one icon, for a list row.
pub fn icon_button(id: impl Into<SharedString>, glyph: LucideIcon, enabled: bool, cx: &Context<Bloom>) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    let color = t.colors.foreground.opacity(if enabled { 0.9 } else { 0.25 });
    div()
        .id(id.into())
        .size(px(34.))
        .rounded(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |el| el.cursor_pointer().hover(|s| s.bg(rgba(0x00000066))))
        .child(icon(glyph, 18., color))
}

impl Bloom {
    /// Lets the user choose an image file and makes it the picture of the
    /// profile.
    pub fn pick_profile_picture(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use as profile picture".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.upload_profile_picture(path, cx)).ok();
        })
        .detach();
    }

    pub(crate) fn upload_profile_picture(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();
        let mime = match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            _ => {
                self.toast("Profile picture", "Choose a PNG, JPEG, WebP or GIF image.", cx);
                return;
            }
        };
        self.fetch(
            cx,
            move |client| {
                let bytes = std::fs::read(&path)?;
                anyhow::ensure!(bytes.len() <= 10 * 1024 * 1024, "the image is larger than 10 MB");
                client.upload_user_image(&bytes, mime)?;
                client.user_image_tag()
            },
            |this, result, cx| this.profile_picture_changed(result, "The picture is set.", cx),
        );
    }

    pub fn remove_profile_picture(&mut self, cx: &mut Context<Self>) {
        self.fetch(
            cx,
            |client| {
                client.delete_user_image()?;
                client.user_image_tag()
            },
            |this, result, cx| this.profile_picture_changed(result, "The picture is removed.", cx),
        );
    }

    /// Shows the new picture everywhere and keeps its tag for the next start.
    fn profile_picture_changed(
        &mut self,
        result: anyhow::Result<Option<String>>,
        done: &'static str,
        cx: &mut Context<Self>,
    ) {
        let tag = match result {
            Ok(tag) => tag,
            Err(err) => {
                self.toast("Profile picture", format!("The server refused it: {err:#}"), cx);
                return;
            }
        };
        if let Some(session) = &mut self.session {
            session.user_image = tag
                .as_deref()
                .map(|tag| session.client.user_image_url(&session.user_id, tag));
            let (server_id, user_id) = (session.server_id.clone(), session.user_id.clone());
            if let Some(profile) = self
                .config
                .servers
                .iter_mut()
                .find(|server| server.id == server_id)
                .and_then(|server| server.profiles.iter_mut().find(|p| p.user_id == user_id))
            {
                profile.image_tag = tag;
            }
        }
        self.save_config(cx);
        self.rebuild_menu(cx);
        self.toast("Profile picture", done, cx);
        cx.notify();
    }
}
