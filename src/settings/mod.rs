// SPDX-License-Identifier: AGPL-3.0-or-later
//! Settings of the signed-in user: profile, playback, subtitles, home and
//! display. The server keeps most of them, in the user's configuration and
//! in the display preferences the web client also uses; a few belong to this
//! app and are in its config file. A change is saved at once. The look
//! follows the Abyss theme's rules for the web client's settings pages.

mod sections;

use std::{collections::HashSet, rc::Rc};

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Point, SharedString, Stateful, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px, rgb, rgba,
};
use serde_json::Value;

use crate::{
    app::{Bloom, Page, SessionEpoch},
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
    /// The saves of the two documents, see [`DocSave`].
    configuration_save: DocSave,
    display_save: DocSave,
}

/// The two documents the server keeps whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Doc {
    Configuration,
    Display,
}

/// One document of one user on one server: what a save writes whole.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DocId {
    server: String,
    user: String,
    doc: Doc,
}

/// The saves that are on their way, for the life of the app. A session
/// that closes does not end its save: the POST still reaches the server,
/// at a time nobody knows. So the rule "one save of a document at a time"
/// is kept here, by server, user and document, and not in the settings of
/// a session, which are made new when a session opens:
///
/// - No save of a document starts while one is on its way, also when that
///   one is from a session that is gone. It waits and goes when the first
///   one ended, with success or not ([`Bloom::save_ended`]).
/// - The settings of a session are not read while a save of its user is on
///   its way. They are read when it ended, so the page starts from the
///   document that save left on the server, and shows "Loading…" until
///   then (at most the 30 s a request can take). No setting can change
///   before that, so no document made from an older state is ever sent.
///
/// A save that is on its way when the app quits needs nothing: no later
/// save of this app can pass it.
#[derive(Default)]
pub struct SaveLanes {
    busy: HashSet<DocId>,
    /// The session whose settings are being read. One read at a time: the
    /// answer of a second one could put an older document on the page
    /// after a save went out.
    loading: Option<SessionEpoch>,
}

impl SaveLanes {
    /// A save of a document of this user is on its way.
    fn busy_for_user(&self, server: &str, user: &str) -> bool {
        self.busy.iter().any(|id| id.server == server && id.user == user)
    }
}

/// The save of one document, as its session sees it. A save posts the
/// whole document, so one is on its way at a time ([`SaveLanes`]): a change
/// meanwhile waits and goes as one save behind it, and the server ends with
/// the newest document. A failed save puts back what it tried to change,
/// unless a newer change took that place.
#[derive(Default)]
struct DocSave {
    /// The document as the server has it, as far as this app knows: what
    /// it loaded, or the last save the server took.
    acknowledged: Value,
    /// What the save on its way sent, while one is.
    sent: Option<Value>,
    /// A change came while a save was on its way (this session's, or one
    /// of a session before with the same user).
    queued: bool,
}

impl Prefs {
    /// The settings as one load brought them: the server has these.
    pub fn from_server(configuration: Value, display: Value, has_password: bool, last_login: Option<String>) -> Self {
        Self {
            loaded: true,
            configuration_save: DocSave { acknowledged: configuration.clone(), ..Default::default() },
            display_save: DocSave { acknowledged: display.clone(), ..Default::default() },
            configuration,
            display,
            has_password,
            last_login,
        }
    }

    fn doc_mut(&mut self, doc: Doc) -> (&mut Value, &mut DocSave) {
        match doc {
            Doc::Configuration => (&mut self.configuration, &mut self.configuration_save),
            Doc::Display => (&mut self.display, &mut self.display_save),
        }
    }

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

/// Puts back what a failed save tried to change: the places where `sent`
/// differs from `acknowledged` (what the server has), as long as `current`
/// still holds what was sent there. A newer change of the same place
/// stays; its own save decides about it.
fn revert_failed(current: &mut Value, sent: &Value, acknowledged: &Value) {
    let (Some(sent_object), Some(acknowledged_object), true) =
        (sent.as_object(), acknowledged.as_object(), current.is_object())
    else {
        if current == sent {
            *current = acknowledged.clone();
        }
        return;
    };
    let keys: Vec<&String> = sent_object
        .keys()
        .chain(acknowledged_object.keys().filter(|key| !sent_object.contains_key(*key)))
        .filter(|key| sent_object.get(*key) != acknowledged_object.get(*key))
        .collect();
    for key in keys {
        let (was_sent, known) = (sent_object.get(key), acknowledged_object.get(key));
        match (current.get_mut(key), was_sent, known) {
            (Some(place), Some(was_sent), Some(known)) if place.is_object() && was_sent.is_object() && known.is_object() => {
                revert_failed(place, was_sent, known)
            }
            (place, was_sent, known) => {
                if place.as_deref() != was_sent {
                    continue;
                }
                match (known, current.as_object_mut()) {
                    (Some(known), Some(object)) => {
                        object.insert(key.clone(), known.clone());
                    }
                    (None, Some(object)) => {
                        object.remove(key);
                    }
                    _ => {}
                }
            }
        }
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
        let Some(session) = &self.session else {
            return;
        };
        let epoch = self.session_epoch;
        // A save of this user is still on its way, from a session before:
        // an answer now could be older than what that save leaves on the
        // server. `save_ended` reads the settings when it is done.
        if self.pref_lanes.loading == Some(epoch)
            || self.pref_lanes.busy_for_user(&session.server_id, &session.user_id)
        {
            return;
        }
        self.pref_lanes.loading = Some(epoch);
        self.fetch(
            cx,
            |client| client.user_prefs(),
            move |this, result, cx| {
                // Another session may be open by now, with settings of its own.
                if this.session_epoch != epoch {
                    return;
                }
                this.pref_lanes.loading = None;
                match result {
                    Ok(loaded) => {
                        this.prefs = Prefs::from_server(
                            loaded.configuration,
                            loaded.display,
                            loaded.has_password,
                            loaded.last_login,
                        );
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
        self.prefs.configuration[key] = value;
        cx.notify();
        self.save_prefs(Doc::Configuration, cx);
    }

    /// Changes values of the display preferences on the server, the same
    /// way: shown at once, put back when the save fails.
    pub fn set_custom_prefs(&mut self, changes: Vec<(String, String)>, cx: &mut Context<Self>) {
        if !self.prefs.loaded || !self.prefs.display.is_object() {
            return;
        }
        if !self.prefs.display["CustomPrefs"].is_object() {
            self.prefs.display["CustomPrefs"] = serde_json::json!({});
        }
        for (key, value) in changes {
            self.prefs.display["CustomPrefs"][key] = Value::String(value);
        }
        cx.notify();
        self.save_prefs(Doc::Display, cx);
    }

    /// The document of the open session, by its place on the server.
    fn doc_id(&self, doc: Doc) -> Option<DocId> {
        let session = self.session.as_ref()?;
        Some(DocId { server: session.server_id.clone(), user: session.user_id.clone(), doc })
    }

    /// Sends a document to the server, whole, when no save of it is on its
    /// way; else the newest state goes when that save ended. See
    /// [`SaveLanes`] and [`DocSave`].
    fn save_prefs(&mut self, doc: Doc, cx: &mut Context<Self>) {
        let (Some(id), Some(client)) = (self.doc_id(doc), self.session.as_ref().map(|s| s.client.clone()))
        else {
            return;
        };
        let (current, save) = self.prefs.doc_mut(doc);
        if self.pref_lanes.busy.contains(&id) {
            save.queued = true;
            return;
        }
        if *current == save.acknowledged {
            return;
        }
        let body = current.clone();
        save.sent = Some(body.clone());
        self.pref_lanes.busy.insert(id.clone());
        let epoch = self.session_epoch;
        self.fetch_with(
            client,
            cx,
            move |client| match doc {
                Doc::Configuration => client.save_user_configuration(&body),
                Doc::Display => client.save_display_preferences(&body),
            },
            move |this, result, cx| {
                // The save ended for the server, whatever session is open.
                this.pref_lanes.busy.remove(&id);
                let failed = result.err();
                // Its answer is for the session that asked: after a switch
                // of the profile it must not put values back, and the save
                // state there is another session's.
                if this.session_epoch == epoch {
                    let (current, save) = this.prefs.doc_mut(doc);
                    if let Some(sent) = save.sent.take() {
                        match &failed {
                            None => save.acknowledged = sent,
                            Some(_) => revert_failed(current, &sent, &save.acknowledged),
                        }
                    }
                }
                // The same user may be open again (A, B, A): the save that
                // failed was that user's, and what waited behind it goes.
                if this.doc_id(doc).as_ref() != Some(&id) {
                    return;
                }
                if let Some(err) = failed {
                    this.toast("The setting was not saved", format!("{err:#}"), cx);
                }
                this.save_ended(doc, cx);
                cx.notify();
            },
        );
    }

    /// A save of a document of the open user ended. What waited for it
    /// goes now: the read of the settings, or the change made meanwhile.
    fn save_ended(&mut self, doc: Doc, cx: &mut Context<Self>) {
        if !self.prefs.loaded {
            self.load_prefs(cx);
            return;
        }
        let (_, save) = self.prefs.doc_mut(doc);
        if std::mem::take(&mut save.queued) {
            self.save_prefs(doc, cx);
        }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::revert_failed;

    #[test]
    fn a_failed_save_puts_back_what_it_changed_and_nothing_newer() {
        let acknowledged = json!({"A": false, "B": false, "CustomPrefs": {"x": "1", "y": "1"}});
        let sent = json!({"A": true, "B": false, "CustomPrefs": {"x": "2", "y": "1"}});
        // Since the save went out, B and y changed as well.
        let mut current = json!({"A": true, "B": true, "CustomPrefs": {"x": "2", "y": "3"}});
        revert_failed(&mut current, &sent, &acknowledged);
        assert_eq!(current, json!({"A": false, "B": true, "CustomPrefs": {"x": "1", "y": "3"}}));
        // A place the save changed and a newer change moved on stays.
        let mut current = json!({"A": 7, "B": false, "CustomPrefs": {"x": "2", "y": "1"}});
        revert_failed(&mut current, &sent, &acknowledged);
        assert_eq!(current, json!({"A": 7, "B": false, "CustomPrefs": {"x": "1", "y": "1"}}));
        // A key the save made is removed again.
        let mut current = json!({"A": false, "New": 1});
        revert_failed(&mut current, &json!({"A": false, "New": 1}), &json!({"A": false}));
        assert_eq!(current, json!({"A": false}));
    }
}

/// Saves that overlap, or outlive the session (review of 2026-10-05, UI
/// finding 2). See `app::race_harness`.
#[cfg(test)]
mod race_tests {
    use gpui_kit::TestAppContext;
    use serde_json::{Value, json};

    use super::Prefs;
    use crate::app::race_harness::{MockServer, add_server, app, plain, session};

    /// A server that takes a configuration unless `refuse(document)` says no.
    fn server(refuse: impl Fn(&Value) -> bool + Send + Sync + 'static) -> MockServer {
        MockServer::start(move |method, path, body| match (method, path) {
            ("POST", p) if p.starts_with("/Users/Configuration") => {
                let document = serde_json::from_str(body).unwrap_or(Value::Null);
                if refuse(&document) { (500, "refused".to_string()) } else { (204, String::new()) }
            }
            _ => plain(method, path),
        })
    }

    #[gpui_kit::test]
    fn a_failed_save_throws_away_a_later_setting(cx: &mut TestAppContext) {
        // The save of A fails; the save with B, sent right after it, succeeds.
        let server = server(|document| document["B"] != json!(true));
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&server.url, "u1"));
            this.prefs = Prefs::from_server(json!({"A": false, "B": false}), json!({}), true, None);
            this.set_user_config("A", json!(true), cx);
            this.set_user_config("B", json!(true), cx);
        });
        cx.run_until_parked();
        assert_eq!(server.count("POST", "/Users/Configuration"), 2);
        bloom.read_with(cx, |this, _| {
            assert_eq!(
                this.prefs.configuration["B"],
                json!(true),
                "the setting B was saved on the server, but the page shows it off again: {:?}",
                server.bodies("POST", "/Users/Configuration")
            );
            assert_eq!(this.prefs.configuration["A"], json!(false), "the refused A is still on");
        });
        // The server ends with what the page shows.
        assert_eq!(server.bodies("POST", "/Users/Configuration").last(), Some(&json!({"A": false, "B": true})));
    }

    /// Saves go one at a time; the changes made while one is on its way go
    /// as one save behind it. Two whole documents can then not pass each
    /// other on the way, and the server ends with the newest.
    #[gpui_kit::test]
    fn changes_during_a_save_go_as_one_save_behind_it(cx: &mut TestAppContext) {
        let server = server(|_| false);
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&server.url, "u1"));
            this.prefs = Prefs::from_server(json!({"A": 0, "B": 0, "C": 0}), json!({}), true, None);
            this.set_user_config("A", json!(1), cx);
            this.set_user_config("B", json!(1), cx);
            this.set_user_config("C", json!(1), cx);
            this.set_user_config("A", json!(2), cx);
        });
        cx.run_until_parked();
        let bodies = server.bodies("POST", "/Users/Configuration");
        assert_eq!(bodies.len(), 2, "one save on its way at a time: {bodies:?}");
        assert_eq!(bodies[0], json!({"A": 1, "B": 0, "C": 0}));
        assert_eq!(bodies[1], json!({"A": 2, "B": 1, "C": 1}));
        bloom.read_with(cx, |this, _| {
            assert_eq!(this.prefs.configuration, json!({"A": 2, "B": 1, "C": 1}));
        });
    }

    /// The profile changes while a save is on its way: its answer must not
    /// touch the settings of the new profile, nor send the old ones again.
    #[gpui_kit::test(iterations = 20)]
    fn a_save_of_the_profile_before_leaves_the_new_profile_alone(cx: &mut TestAppContext) {
        let one = server(|_| true);
        let two = MockServer::start(|method, path, _| match (method, path) {
            ("GET", "/Users/Me") => (200, r#"{"Configuration":{"Z":1},"HasPassword":true}"#.into()),
            _ => plain(method, path),
        });
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            add_server(this, "two", &two.url, "u2");
            this.session = Some(session(&one.url, "u1"));
            this.prefs = Prefs::from_server(json!({"A": false}), json!({}), true, None);
            this.set_user_config("A", json!(true), cx);
            assert!(this.open_session("two", "u2", cx));
        });
        cx.run_until_parked();
        assert_eq!(one.count("POST", "/Users/Configuration"), 1);
        assert_eq!(two.count("POST", "/Users/Configuration"), 0, "the old settings went to the new server");
        bloom.read_with(cx, |this, _| {
            assert!(this.prefs.loaded);
            assert_eq!(
                this.prefs.configuration,
                json!({"Z": 1}),
                "the failed save of the profile before put its settings into the new profile"
            );
        });
    }

    // ----- a save that outlives its session (review round 3, item 4) -----

    use std::sync::{Arc, Mutex};

    use crate::app::Bloom;

    /// A server that keeps the configuration of its user as the real one
    /// does: a POST replaces it whole unless `refuse(document)` says no,
    /// and `/Users/Me` answers with what it has. `document()` reads it.
    fn account(
        first: Value,
        refuse: impl Fn(&Value) -> bool + Send + Sync + 'static,
    ) -> (MockServer, impl Fn() -> Value) {
        let kept = Arc::new(Mutex::new(first));
        let state = kept.clone();
        let server = MockServer::start(move |method, path, body| match (method, path) {
            ("POST", p) if p.starts_with("/Users/Configuration") => {
                let document = serde_json::from_str(body).unwrap_or(Value::Null);
                if refuse(&document) {
                    return (500, "refused".to_string());
                }
                *state.lock().expect("kept") = document;
                (204, String::new())
            }
            ("GET", "/Users/Me") => {
                let configuration = state.lock().expect("kept").clone();
                (200, json!({"Configuration": configuration, "HasPassword": true}).to_string())
            }
            _ => plain(method, path),
        });
        (server, move || kept.lock().expect("kept").clone())
    }

    /// Profile "one" is open and loaded; a save of A (D1) goes out; the
    /// user switches to profile "two" and back before any task ran, so D1
    /// is on its way while "one" is open for the second time.
    fn switch_away_and_back_during_a_save(
        bloom: &gpui_kit::Entity<Bloom>,
        cx: &mut gpui_kit::VisualTestContext,
        one: &MockServer,
        two: &MockServer,
    ) {
        bloom.update(cx, |this, cx| {
            add_server(this, "one", &one.url, "u1");
            add_server(this, "two", &two.url, "u2");
            assert!(this.open_session("one", "u1", cx));
        });
        cx.run_until_parked();
        bloom.update(cx, |this, cx| {
            assert_eq!(this.prefs.configuration, json!({"A": false, "B": false}));
            this.set_user_config("A", json!(true), cx);
            assert!(this.open_session("two", "u2", cx));
            assert!(this.open_session("one", "u1", cx));
        });
        assert_eq!(one.count("POST", "/Users/Configuration"), 0, "the save D1 is still on its way");
    }

    const D1: fn() -> Value = || json!({"A": true, "B": false});
    const D2: fn() -> Value = || json!({"A": false, "B": true});

    /// The settings of the profile that is open again were read before the
    /// old save landed (the test puts that answer in), and the user changes
    /// B. That document D2 must not pass D1 on the way to the server.
    fn change_b_on_a_page_from_before_the_save(bloom: &gpui_kit::Entity<Bloom>, cx: &mut gpui_kit::VisualTestContext) {
        bloom.update(cx, |this, cx| {
            this.prefs = Prefs::from_server(json!({"A": false, "B": false}), json!({}), true, None);
            this.set_user_config("B", json!(true), cx);
        });
        cx.run_until_parked();
    }

    #[gpui_kit::test(iterations = 20)]
    fn a_save_waits_for_the_save_of_the_session_before(cx: &mut TestAppContext) {
        let (one, document) = account(json!({"A": false, "B": false}), |_| false);
        let two = MockServer::start(|method, path, _| plain(method, path));
        let (bloom, cx) = app(cx);
        switch_away_and_back_during_a_save(&bloom, cx, &one, &two);
        change_b_on_a_page_from_before_the_save(&bloom, cx);
        assert_eq!(
            one.bodies("POST", "/Users/Configuration"),
            vec![D1(), D2()],
            "the newer document D2 did not wait for D1, the save of the session before"
        );
        assert_eq!(document(), D2(), "the server ends with the older document");
        assert_eq!(two.count("POST", "/Users/Configuration"), 0);
        bloom.read_with(cx, |this, _| assert_eq!(this.prefs.configuration, D2()));
    }

    /// The same, and the server refuses D1: nothing of it is put back into
    /// the settings of the session that is open now, and D2 still goes.
    #[gpui_kit::test(iterations = 20)]
    fn a_failed_save_of_the_session_before_lets_the_next_save_go(cx: &mut TestAppContext) {
        let (one, document) = account(json!({"A": false, "B": false}), |document| document["A"] == json!(true));
        let two = MockServer::start(|method, path, _| plain(method, path));
        let (bloom, cx) = app(cx);
        switch_away_and_back_during_a_save(&bloom, cx, &one, &two);
        change_b_on_a_page_from_before_the_save(&bloom, cx);
        assert_eq!(
            one.bodies("POST", "/Users/Configuration"),
            vec![D1(), D2()],
            "D2 did not wait for the end of D1, or did not go after it failed"
        );
        assert_eq!(document(), D2());
        bloom.read_with(cx, |this, _| assert_eq!(this.prefs.configuration, D2()));
    }

    /// Without the test's hand: the profile that is open again reads its
    /// settings when the old save ended, so its page shows what that save
    /// left on the server, and no setting can change before.
    #[gpui_kit::test(iterations = 20)]
    fn a_profile_opened_again_reads_its_settings_after_the_save_on_its_way(cx: &mut TestAppContext) {
        let (one, document) = account(json!({"A": false, "B": false}), |_| false);
        let two = MockServer::start(|method, path, _| plain(method, path));
        let (bloom, cx) = app(cx);
        switch_away_and_back_during_a_save(&bloom, cx, &one, &two);
        bloom.update(cx, |this, cx| {
            assert!(!this.prefs.loaded);
            // The page shows "Loading…" and takes no change.
            this.set_user_config("B", json!(true), cx);
        });
        cx.run_until_parked();
        assert_eq!(one.bodies("POST", "/Users/Configuration"), vec![D1()]);
        assert_eq!(document(), D1());
        bloom.read_with(cx, |this, _| {
            assert!(this.prefs.loaded, "the settings were not read after the save ended");
            assert_eq!(
                this.prefs.configuration,
                D1(),
                "the page shows the settings from before the save that the server took"
            );
        });
    }

    /// The same with a refused D1: the page shows what the server kept.
    #[gpui_kit::test(iterations = 20)]
    fn a_profile_opened_again_reads_its_settings_after_a_failed_save(cx: &mut TestAppContext) {
        let first = json!({"A": false, "B": false});
        let (one, document) = account(first.clone(), |document| document["A"] == json!(true));
        let two = MockServer::start(|method, path, _| plain(method, path));
        let (bloom, cx) = app(cx);
        switch_away_and_back_during_a_save(&bloom, cx, &one, &two);
        cx.run_until_parked();
        assert_eq!(one.bodies("POST", "/Users/Configuration"), vec![D1()]);
        assert_eq!(document(), first);
        bloom.read_with(cx, |this, _| {
            assert!(this.prefs.loaded, "the settings were not read after the save failed");
            assert_eq!(this.prefs.configuration, first);
        });
    }

    /// One read of the settings at a time: the answer of a second one could
    /// put an older document on the page after a save went out.
    #[gpui_kit::test]
    fn the_settings_are_read_once_at_a_time(cx: &mut TestAppContext) {
        let (one, _) = account(json!({"A": false}), |_| false);
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&one.url, "u1"));
            this.load_prefs(cx);
            this.load_prefs(cx);
        });
        cx.run_until_parked();
        assert_eq!(one.count("GET", "/DisplayPreferences/usersettings"), 1);
        bloom.read_with(cx, |this, _| assert!(this.prefs.loaded));
    }
}
