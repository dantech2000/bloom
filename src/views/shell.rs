// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Main screen: top bar and page content.

use crate::ui::tone;
use gpui_icons::LucideIcon;
use gpui_kit::{
    Context, Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, linear_color_stop,
    linear_gradient,
    prelude::FluentBuilder as _, px,
};

use crate::ui::motion::{Enter, enter};
use crate::{
    app::{Bloom, Page, Panel},
    icons::{Filled, filled},
    jellyfin::ItemKind,
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        glass::glass,
        menu::{Menu, MenuItem},
        theme::UiTheme,
        tip::tip,
    },
    views::{cards::icon, parts::initial},
};

/// Icon of a top bar tab.
enum Glyph {
    Lucide(LucideIcon),
    /// The Jellyfin mark, for the server tab.
    Logo,
}

/// Height of the top bar. The window title bar is transparent, so the bar
/// also holds the traffic lights at its left edge.
pub const TOPBAR_H: f32 = 64.;
/// Room kept free for the traffic lights.
const TRAFFIC_LIGHTS_W: f32 = 84.;

/// The room above the top bar: the status bar of an iPhone (the top of
/// the safe area); nothing on the Mac, where the bar holds the title bar.
pub fn top_inset() -> f32 {
    #[cfg(target_os = "macos")]
    {
        0.
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::macos::safe_area_insets().0
    }
}

/// The room under the page: the home indicator of an iPhone (the bottom
/// of the safe area); nothing on the Mac.
pub fn bottom_inset() -> f32 {
    #[cfg(target_os = "macos")]
    {
        0.
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::macos::safe_area_insets().1
    }
}

/// The height of the software keyboard over the window, in points: the
/// page keeps that room free at the bottom, so a field stays in sight.
/// While the keyboard slides, the height it covers at this instant, so
/// the page moves with it (`Bloom::render` asks for a frame per tick
/// meanwhile, `keyboard_sliding`). 0 on the Mac, and while the keyboard
/// is down.
pub fn keyboard_inset() -> f32 {
    #[cfg(target_os = "ios")]
    {
        gpui_ios::keyboard_height_now().0
    }
    #[cfg(not(target_os = "ios"))]
    {
        0.
    }
}

/// True while the software keyboard slides up or down.
pub fn keyboard_sliding() -> bool {
    #[cfg(target_os = "ios")]
    {
        gpui_ios::keyboard_height_now().1
    }
    #[cfg(not(target_os = "ios"))]
    {
        false
    }
}

/// A window the width of a phone: the tabs go to a bar at the bottom,
/// the top bar keeps the title and two buttons, menus open as sheets.
/// The Mac window is at least 900 wide, so it never is.
pub fn compact(window: &Window) -> bool {
    f32::from(window.viewport_size().width) < COMPACT_W
}

/// Below this width the window is compact.
pub const COMPACT_W: f32 = 700.;

/// The width of the window at its last frame, for a widget that has no
/// window at hand (the fields of the settings and dashboard pages).
static VIEWPORT_W: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The width of the window at its last frame.
pub fn viewport_w_now() -> f32 {
    f32::from_bits(VIEWPORT_W.load(std::sync::atomic::Ordering::Relaxed))
}

/// The scale of the window at its last frame (2 on a Mac, 3 on a phone):
/// the pixels an image needs for a point of a card.
static SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The scale of the window at its last frame; 2 before the first frame.
pub fn scale_now() -> f32 {
    let scale = f32::from_bits(SCALE.load(std::sync::atomic::Ordering::Relaxed));
    if scale > 0. { scale } else { 2. }
}

/// `compact`, from the width of the last frame.
pub fn compact_now() -> bool {
    let w = viewport_w_now();
    w > 0. && w < COMPACT_W
}

impl Bloom {
    /// `compact`, from the viewport the app keeps (`viewport_w`).
    pub fn compact_w(&self) -> bool {
        self.viewport_w < COMPACT_W
    }

    /// A phone, in either orientation: the short side of the window is
    /// under 500 px (an iPad's is 744 or more, a Mac window's 600). The
    /// player uses it: in landscape the window is wide but not tall.
    pub fn phone(&self) -> bool {
        self.viewport_w.min(self.viewport_h) < PHONE_SHORT_SIDE
    }
}

/// A window with a shorter side than this is a phone's.
pub const PHONE_SHORT_SIDE: f32 = 500.;

/// The safe area on every side: top, left, bottom, right. The notch is at
/// a side in landscape. Zeros on the Mac.
pub fn safe_insets() -> (f32, f32, f32, f32) {
    #[cfg(target_os = "macos")]
    {
        (0., 0., 0., 0.)
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::macos::safe_area()
    }
}

/// The glyph of a library, by the kind of its content.
fn library_glyph(view: &crate::jellyfin::Item) -> LucideIcon {
    match view.collection_type.as_deref() {
        Some("movies") => LucideIcon::Clapperboard,
        Some("tvshows") => LucideIcon::Tv,
        Some("music") => LucideIcon::Music,
        Some("homevideos") | Some("photos") => LucideIcon::Image,
        Some("boxsets") => LucideIcon::GalleryVerticalEnd,
        _ => LucideIcon::Folder,
    }
}

/// Height of the tab bar of a compact window, and the room the page
/// keeps under it: the bar floats 10 px over the bottom inset.
pub const TABBAR_H: f32 = 62.;
const TABBAR_MARGIN: f32 = 10.;

impl Bloom {
    pub fn render_shell(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The player covers the window, so the page behind it is not built.
        if self.playback.open {
            return div()
                .flex()
                .size_full()
                .child(self.render_player(window, cx))
                .into_any_element();
        }
        // A page with nothing to show because the server does not answer
        // says so, instead of an empty page.
        let page = match &self.nav.page {
            _ if self.offline_page_shows() => self.render_offline_page(cx).into_any_element(),
            Page::Home(_) => self.render_home(cx).into_any_element(),
            Page::Library(_) => self.render_library(cx).into_any_element(),
            Page::Detail(_) => self.render_detail(cx).into_any_element(),
            Page::Search(_) => self.render_search(cx).into_any_element(),
            Page::Admin(_) => self.render_admin(cx).into_any_element(),
            Page::Settings(_) => self.render_settings(cx).into_any_element(),
            Page::Playlist(_) => self.render_playlist(cx).into_any_element(),
            Page::Downloads => self.render_downloads(cx).into_any_element(),
        };
        // The page comes in (`ui::motion`): on a phone it slides in from
        // the right, or fades when Back brought it; on the Mac it lifts
        // into place. The key names the page, so the next page runs it
        // again and a redraw of the same page does not.
        let page_key = SharedString::from(match &self.nav.page {
            _ if self.offline_page_shows() => "page.offline".to_string(),
            Page::Home(_) => "page.home".to_string(),
            Page::Library(d) => format!("page.library.{}.{:?}", d.view.id, d.show),
            Page::Detail(d) => format!("page.detail.{}", d.item.id),
            Page::Search(_) => "page.search".to_string(),
            Page::Admin(_) => "page.admin".to_string(),
            Page::Settings(_) => "page.settings".to_string(),
            Page::Playlist(d) => format!("page.playlist.{}", d.list.id),
            Page::Downloads => "page.downloads".to_string(),
        });
        let compact = compact(window);
        let page = enter(
            div().size_full().child(page),
            page_key,
            if !compact {
                Enter::Lift
            } else if self.nav.back {
                Enter::Fade
            } else {
                Enter::Push
            },
            cx,
        );
        // On home and detail pages the bar lies over the artwork; other
        // pages start below it.
        let overlay = matches!(self.nav.page, Page::Home(_) | Page::Detail(_));
        let scrolled = f32::from(self.nav.scroll.offset().y) < -8.;
        VIEWPORT_W.store(
            f32::from(window.viewport_size().width).to_bits(),
            std::sync::atomic::Ordering::Relaxed,
        );
        SCALE.store(window.scale_factor().to_bits(), std::sync::atomic::Ordering::Relaxed);
        let keyboard = keyboard_inset();
        use gpui_kit::{MouseButton, NavigationDirection};
        div()
            .id("shell")
            .track_focus(&self.app_focus)
            .on_key_down(cx.listener(Self::shell_key))
            // The back and forward buttons of a mouse, and a three-finger
            // swipe (AppKit sends it as such a button).
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, _, cx| this.back(cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, _, cx| this.forward(cx)),
            )
            // A two-finger swipe comes as scroll events (`swipe.rs`).
            .on_scroll_wheel(cx.listener(Self::shell_scroll))
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        div()
                            .size_full()
                            .when(!overlay, |el| el.pt(px(TOPBAR_H + top_inset())))
                            // The page ends above the tab bar, or above the
                            // keyboard while it is up (the bar is away then).
                            .when(compact && keyboard == 0., |el| {
                                el.pb(px(TABBAR_H + TABBAR_MARGIN + bottom_inset()))
                            })
                            .when(keyboard > 0., |el| el.pb(px(keyboard)))
                            .child(page),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            // A shade keeps the bar readable over the artwork.
                            // A phone: the bar turns to glass instead once
                            // the page scrolls under it, as the bars of iOS do.
                            .when(overlay && !(scrolled && compact), |el| {
                                el.bg(linear_gradient(
                                    180.,
                                    linear_color_stop(gpui_kit::black().alpha(0.55), 0.0),
                                    linear_color_stop(gpui_kit::black().alpha(0.0), 1.0),
                                ))
                            })
                            // Content scrolls under the bar; fade it out there.
                            .when(overlay && scrolled && !compact, |el| el.pb(px(28.)))
                            .when(overlay && scrolled && compact, |el| {
                                el.child(glass(px(0.), crate::ui::glass::POPUP_TINT))
                            })
                            .child(self.render_topbar(scrolled, compact, cx)),
                    )
                    .when(compact && keyboard == 0., |el| el.child(self.render_tabbar(cx))),
            )
            .into_any_element()
    }

    /// The title of the page for the top bar of a compact window.
    fn page_title(&self) -> String {
        match &self.nav.page {
            Page::Home(_) => self
                .session
                .as_ref()
                .map(|s| s.server_name.clone())
                .unwrap_or_default(),
            Page::Library(data) => data.view.name.clone(),
            Page::Detail(data) => data.item.name.clone(),
            Page::Search(_) => "Search".into(),
            // On a phone the list of sections is "Dashboard", the section
            // open has its own name.
            Page::Admin(d) if !d.list && self.compact_w() => {
                d.subpage_title().unwrap_or_else(|| d.section.label().to_string())
            }
            Page::Admin(_) => "Dashboard".into(),
            Page::Settings(open) if !self.settings_list && self.compact_w() => open.label().into(),
            Page::Settings(_) => "Settings".into(),
            Page::Playlist(_) => "Playlist".into(),
            Page::Downloads => "Downloads".into(),
        }
    }

    /// The tab bar of a compact window: a glass pill at the bottom with
    /// home, the libraries and search, as the tab bar of an iPhone app.
    /// With two libraries or fewer, favourites has a place too.
    fn render_tabbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = UiTheme::read(cx).colors.foreground;
        // A page that is no library and has no library behind it lights no
        // tab but its own (search), as the tab bars of iOS do.
        let active = match &self.nav.page {
            Page::Search(_) | Page::Settings(_) | Page::Admin(_) | Page::Downloads => None,
            _ => self.active_tab(),
        };
        let is_home = matches!(self.nav.page, Page::Home(_)) && self.nav.history.is_empty();
        let is_search = matches!(self.nav.page, Page::Search(_));
        let item = |id: SharedString, label: String, glyph: LucideIcon, selected: bool| {
            let color = if selected { fg } else { fg.opacity(0.6) };
            div()
                .id(id)
                .role(gpui_kit::Role::Tab)
                .aria_label(label.clone())
                .flex_1()
                .h(px(TABBAR_H - 8.))
                .rounded(px(18.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .cursor_pointer()
                .text_size(px(10.))
                // The line box as tall as the text: with the shell's 20-point
                // line the label carried 5 points of empty box under it and
                // the pair sat 3 points above the middle of the bar
                // (Daniel, 2026-10-23).
                .line_height(px(12.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(color)
                .when(selected, |el| el.bg(fg.opacity(0.12)))
                .child(icon(glyph, 22., color))
                .child(label)
        };
        let mut bar = div()
            .absolute()
            .left(px(12.))
            .right(px(12.))
            .bottom(px(TABBAR_MARGIN + bottom_inset()))
            .h(px(TABBAR_H))
            .p(px(4.))
            .rounded(px(24.))
            .border_1()
            .border_color(tone::EDGE)
            .child(crate::ui::glass::bar_glass(px(24.), crate::ui::glass::POPUP_TINT))
            .flex()
            .items_center()
            .child(
                item("tab.home".into(), "Home".into(), LucideIcon::House, is_home)
                    .on_click(cx.listener(|this, _, _, cx| this.open_home(cx))),
            );
        let libraries = self.views.len().min(3);
        for view in &self.views[..libraries] {
            let target = view.id.clone();
            bar = bar.child(
                item(
                    SharedString::from(format!("tab.lib.{}", view.id)),
                    view.name.clone(),
                    library_glyph(view),
                    active.as_deref() == Some(view.id.as_str()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_library_tab(&target, cx))),
            );
        }
        if libraries <= 2 {
            bar = bar.child(
                item(
                    "tab.favorites".into(),
                    "Favorites".into(),
                    LucideIcon::Heart,
                    active.as_deref() == Some(crate::app::FAVORITES_ID),
                )
                .on_click(cx.listener(|this, _, _, cx| this.open_favorites(cx))),
            );
        }
        bar.child(
            item("tab.search".into(), "Search".into(), LucideIcon::Search, is_search)
                .on_click(cx.listener(|this, _, window, cx| this.focus_search(window, cx))),
        )
    }

    /// Opens the search page with the text of its field, and puts the
    /// keys into the field.
    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search.input.read(cx).value().to_string();
        self.open_search(query, cx);
        let focus = self.search.input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }

    /// Opens the library of a tab. The tab keeps the id alone: a tab is
    /// built again at each frame, and a copy of the library for each is
    /// work that only a click needs.
    fn open_library_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.views.iter().find(|view| view.id == id).cloned() {
            self.open_library(view, cx);
        }
    }

    /// A scroll over the page: a two-finger swipe between pages goes back
    /// or forward, when the trackpad setting of the Mac turns pages with a
    /// scroll. The decision is in `swipe.rs`; a row of cards that scrolls
    /// sideways claims the gesture (`cards.rs`), and the player has no
    /// page to turn (it is not in this tree).
    fn shell_scroll(
        &mut self,
        event: &gpui_kit::ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::swipe::{Direction, Swipe};
        use gpui_kit::{ScrollDelta, TouchPhase};
        // A mouse wheel scrolls by lines; only a trackpad swipes.
        let ScrollDelta::Pixels(delta) = event.delta else {
            return;
        };
        let phase = event.touch_phase;
        let allowed = !self.playback.open
            && (phase != TouchPhase::Started || crate::macos::swipe_between_pages_with_scroll());
        let turned = cx.default_global::<Swipe>().feed(phase, f32::from(delta.x), f32::from(delta.y), allowed);
        match turned {
            Some(Direction::Back) => self.back(cx),
            Some(Direction::Forward) => self.forward(cx),
            None => {}
        }
    }

    /// Keyboard shortcuts of the main screen. They follow the web client
    /// with the Media Bar and Jellyfin Enhanced plugins.
    fn shell_key(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        // No log of the key while a dialog is open: its keys are the text
        // of its fields, and one of them can be a password.
        if !self.admin_dialog_open() {
            log::debug!("shell key: {keystroke:?}");
        }
        let modifiers = keystroke.modifiers;
        // A dialog of the dashboard takes the keys: Escape closes it, Tab
        // goes to its next field, and the rest is text for a field.
        if self.admin_dialog_open() {
            match keystroke.key.as_str() {
                "escape" => self.close_admin_dialog(window, cx),
                "tab" => self.next_admin_field(window, cx),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        // The dialog of "Request more seasons" closes on Escape, as the
        // other dialogs do.
        if self.panel_is(Panel::Seasons) && keystroke.key == "escape" {
            self.close_seasons_dialog(cx);
            cx.stop_propagation();
            return;
        }
        // Keys typed into the search field are text, not shortcuts. Only
        // Escape acts there: it takes the focus off the field.
        if self.search.input.read(cx).focus_handle(cx).contains_focused(window, cx) {
            if keystroke.key == "escape" {
                window.focus(&self.app_focus, cx);
                cx.stop_propagation();
            }
            return;
        }
        // The same for the filter field of a plugin's settings: "r" there
        // is a letter, not "open a random title".
        if self.plugin_filter_focused(window, cx) {
            if keystroke.key == "escape" {
                window.focus(&self.app_focus, cx);
                cx.stop_propagation();
            }
            return;
        }
        // The hero keys apply while the hero is in view on the home page.
        let on_hero = matches!(self.nav.page, Page::Home(_))
            && !self.hero.slides.is_empty()
            && -f32::from(self.nav.scroll.offset().y) < self.hero_height();
        // The list of keys and the shortcuts of Jellyfin Enhanced.
        if self.enhanced_shell_key(event, window, cx) {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        match keystroke.key.as_str() {
            "[" if modifiers.platform => self.back(cx),
            // Escape closes an open panel first; the page stays.
            "escape" if self.panel_is(Panel::SyncPlay) || self.panel_is(Panel::Cast) => self.close_popups(None, window, cx),
            "escape" | "backspace" if !self.nav.history.is_empty() => self.back(cx),
            "/" => self.focus_search(window, cx),
            "h" if modifiers.shift => self.open_home(cx),
            "r" if !modifiers.modified() => self.open_random(cx),
            "left" if on_hero => self.step_hero(false),
            "right" if on_hero => self.step_hero(true),
            "space" | "p" if on_hero => self.toggle_hero_pause(),
            "m" if on_hero => self.toggle_hero_mute(),
            _ => return,
        }
        cx.notify();
        cx.stop_propagation();
    }

    /// The library a page belongs to, which decides the highlighted tab.
    fn active_tab(&self) -> Option<String> {
        let by_type = |collection: &str| {
            self.views
                .iter()
                .find(|v| v.collection_type.as_deref() == Some(collection))
                .map(|v| v.id.clone())
        };
        let root = self.nav.history.first().map_or(&self.nav.page, |(page, _)| page);
        if let Page::Library(data) = root {
            return Some(data.view.id.clone());
        }
        match &self.nav.page {
            Page::Detail(data) => match data.item.kind {
                ItemKind::Movie => by_type("movies"),
                ItemKind::Series | ItemKind::Season | ItemKind::Episode => by_type("tvshows"),
                ItemKind::BoxSet => by_type("boxsets"),
                _ => None,
            },
            Page::Playlist(_) => Some(crate::lists::PLAYLISTS_ID.to_string()),
            _ => None,
        }
    }

    /// Rebuilds the profile dropdown entries from the current config.
    pub fn rebuild_menu(&mut self, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let mut items = Vec::new();
        let active = self.config.active.clone();
        for server in &self.config.servers {
            items.push(MenuItem::label(
                SharedString::from(format!("menu.server.{}", server.id)),
                server.name.clone(),
            ));
            for profile in &server.profiles {
                let server_id = server.id.clone();
                let user_id = profile.user_id.clone();
                let selected = active
                    .as_ref()
                    .is_some_and(|(s, u)| s == &server_id && u == &user_id);
                let handle = this.clone();
                items.push(
                    MenuItem::new(
                        SharedString::from(format!("menu.profile.{server_id}.{user_id}")),
                        profile.name.clone(),
                    )
                    .icon(LucideIcon::User)
                    .radio(selected)
                    .on_click(move |_, _, cx| {
                        let (server_id, user_id) = (server_id.clone(), user_id.clone());
                        handle
                            .update(cx, move |this, cx| {
                                this.open_session(&server_id, &user_id, cx);
                            })
                            .ok();
                    }),
                );
            }
        }
        if !items.is_empty() {
            items.push(MenuItem::separator());
        }
        if self.is_admin() {
            let handle = this.clone();
            items.push(
                MenuItem::new("menu.dashboard", "Dashboard")
                    .icon(LucideIcon::LayoutGrid)
                    .detail("Users, libraries, tasks and logs of the server")
                    .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| {
                            this.open_admin(crate::admin::Section::Dashboard, cx)
                        })
                        .ok();
                }),
            );
            items.push(MenuItem::separator());
        }
        let handle = this.clone();
        items.push(
            MenuItem::new("menu.manage", "Servers & profiles…")
                .icon(LucideIcon::Server)
                .on_click(move |_, _, cx| {
                handle.update(cx, |this, cx| this.show_connect(cx)).ok();
            }),
        );
        // A phone keeps the sheet short: these two switches are in
        // Settings > Display there.
        if !self.compact_w() {
            let handle = this.clone();
            items.push(
                MenuItem::new("menu.trailers", "Trailer backdrops")
                    .icon(LucideIcon::Clapperboard)
                    .checked(self.config.hero_video.unwrap_or(true))
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |this, cx| this.toggle_hero_video(cx)).ok();
                    }),
            );
            let handle = this.clone();
            items.push(
                MenuItem::new("menu.quality", "Quality tags")
                    .icon(LucideIcon::BadgeCheck)
                    .checked(self.quality_tags())
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |this, cx| this.toggle_quality_tags(cx)).ok();
                    }),
            );
        }
        items.extend(self.enhanced_menu(cx));
        // A phone's top bar has no room for these; the sheet has them.
        if self.compact_w() && self.session.is_some() {
            items.push(MenuItem::separator());
            let handle = this.clone();
            let casting = self.cast.active();
            items.push(
                MenuItem::new("menu.cast", "Play on")
                    .icon(LucideIcon::Cast)
                    .detail(if casting {
                        "Another device plays".to_string()
                    } else {
                        "A device of yours, a Chromecast or AirPlay".to_string()
                    })
                    .on_click(move |_, window, cx| {
                        handle.update(cx, |this, cx| this.toggle_cast_panel(window, cx)).ok();
                    }),
            );
            if self.sync.allowed() && self.sync.session.is_some() {
                let handle = this.clone();
                let in_group = self.sync.in_group();
                items.push(
                    MenuItem::new("menu.syncplay", "SyncPlay")
                        .icon(LucideIcon::Users)
                        .detail(if in_group { "In a group" } else { "Watch together with others" })
                        .on_click(move |_, window, cx| {
                            handle.update(cx, |this, cx| this.toggle_sync_panel(window, cx)).ok();
                        }),
                );
            }
            let handle = this.clone();
            items.push(
                MenuItem::new("menu.random", "Random item")
                    .icon(LucideIcon::Dices)
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |this, cx| this.open_random(cx)).ok();
                    }),
            );
            items.push(MenuItem::separator());
        }
        if self.session.is_some() {
            let handle = this.clone();
            items.push(
                MenuItem::new("menu.settings", "Settings")
                    .icon(LucideIcon::Settings)
                    .detail("Playback, subtitles, home page, profile")
                    .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| {
                            this.open_settings(crate::settings::Section::Profile, cx)
                        })
                        .ok();
                }),
            );
            if self.downloads_allowed() {
                let handle = this.clone();
                items.push(
                    MenuItem::new("menu.downloads", "Downloads")
                        .icon(LucideIcon::Download)
                        .detail(format!("Movies and episodes kept on {}", crate::brand::THIS_DEVICE))
                        .on_click(move |_, _, cx| {
                            handle.update(cx, |this, cx| this.open_downloads(cx)).ok();
                        }),
                );
            }
            let handle = this.clone();
            items.push(
                MenuItem::new("menu.quickconnect", "Quick Connect")
                    .icon(LucideIcon::Link)
                    .detail("Sign in another device with its code")
                    .on_click(
                    move |_, window, cx| {
                        handle
                            .update(cx, |this, cx| this.open_authorize(window, cx))
                            .ok();
                    },
                ),
            );
            if let crate::updates::Status::Ready(version) = crate::updates::status() {
                items.push(
                    MenuItem::new("menu.update", "Restart to update")
                        .icon(LucideIcon::RefreshCw)
                        .detail(format!("{} {version} is downloaded", crate::brand::NAME))
                        .on_click(|_, _, _| {
                            crate::updates::restart_to_update();
                        }),
                );
            }
            let handle = this.clone();
            items.push(
                MenuItem::new("menu.about", format!("About {}", crate::brand::NAME))
                    .icon(LucideIcon::Info)
                    .detail("License and the projects it is made with")
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |this, cx| {
                                this.open_settings(crate::settings::Section::About, cx)
                            })
                            .ok();
                    }),
            );
            let handle = this.clone();
            items.push(
                MenuItem::new("menu.signout", "Sign out")
                    .icon(LucideIcon::LogOut)
                    .on_click(move |_, _, cx| {
                    handle.update(cx, |this, cx| this.sign_out(cx)).ok();
                }),
            );
        }
        self.popups.profile_menu
            .update(cx, |menu, cx| menu.set_items(items, cx));
    }

    fn render_topbar(&self, scrolled: bool, compact: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = UiTheme::read(cx).colors;
        let fg = colors.foreground;
        let active = self.active_tab();
        let is_home = matches!(self.nav.page, Page::Home(_)) && self.nav.history.is_empty();
        let server = self
            .session
            .as_ref()
            .map(|s| s.server_name.clone())
            .unwrap_or_default();

        // An iPad held upright (834 px) has no room for the labels beside
        // the buttons at the right: the tabs show their glyph alone there.
        let icon_only = self.viewport_w < 900.;
        let tab = move |id: SharedString, label: String, glyph: Glyph, selected: bool| {
            let color = if selected { colors.primary_foreground } else { fg };
            let logo = matches!(glyph, Glyph::Logo);
            div()
                .id(id)
                .role(gpui_kit::Role::Tab)
                .aria_label(label.clone())
                .h(px(36.))
                .px(px(14.))
                .rounded_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .text_size(px(13.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(color)
                .when(selected, |el| el.bg(colors.primary))
                .when(!selected, |el| el.hover(|s| s.bg(fg.opacity(0.08))))
                .child(match glyph {
                    Glyph::Lucide(glyph) => icon(glyph, 16., color),
                    // The web theme shows the mark in white, a little dimmed.
                    Glyph::Logo => crate::icons::logo(19., color.opacity(0.85).into()),
                })
                .when(!icon_only || logo, |el| el.child(label))
        };

        // The web gives the tab group a glass pill once the page scrolls.
        let mut tabs = div()
            .p(px(4.))
            .rounded_full()
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .relative()
            .when(scrolled, |el| {
                el.child(glass(px(999.), tone::CARD))
                    .border_color(gpui_kit::rgba(0x5757574d))
            })
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
            tab("nav.home".into(), server, Glyph::Logo, false)
                .on_click(cx.listener(|this, _, _, cx| this.open_home(cx))),
        );
        tabs = tabs.child(
            tab(
                "nav.favorites".into(),
                "Favorites".into(),
                Glyph::Lucide(LucideIcon::Heart),
                active.as_deref() == Some(crate::app::FAVORITES_ID),
            )
            .on_click(cx.listener(|this, _, _, cx| this.open_favorites(cx))),
        );
        for view in &self.views {
            let target = view.id.clone();
            tabs = tabs.child(
                tab(
                    SharedString::from(format!("nav.lib.{}", view.id)),
                    view.name.clone(),
                    Glyph::Lucide(library_glyph(view)),
                    active.as_deref() == Some(view.id.as_str()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_library_tab(&target, cx))),
            );
        }

        // The web shows the Playlists library only when there is a playlist.
        if !self.lists.playlists.is_empty() {
            tabs = tabs.child(
                tab(
                    "nav.playlists".into(),
                    "Playlists".into(),
                    Glyph::Lucide(LucideIcon::ListVideo),
                    active.as_deref() == Some(crate::lists::PLAYLISTS_ID),
                )
                .on_click(cx.listener(|this, _, _, cx| this.open_playlists(cx))),
            );
        }

        let round = |id: &'static str, label: &'static str, glyph: gpui_kit::Svg| {
            Button::new(id)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Icon)
                .aria_label(label)
                .child(glyph)
        };
        // A state the button stands for (a device plays, a group is
        // joined) lights it.
        let lit = tone::EDGE;

        let (name, image) = match &self.session {
            Some(s) => (s.user_name.clone(), s.user_image.clone()),
            None => (String::new(), None),
        };
        let avatar = crate::ui::avatar::Avatar::new("profile.avatar")
            .fallback(initial(&name))
            .aria_label(name)
            .when_some(
                image.and_then(|url| crate::images::image(&url, cx)),
                |a, image| a.image(image),
            );

        if compact {
            return div()
                .relative()
                .h(px(TOPBAR_H + top_inset()))
                .pt(px(top_inset()))
                .flex_shrink_0()
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(4.))
                .when(!is_home, |el| {
                    el.child(round("top.back", "Back", icon(LucideIcon::ArrowLeft, 18., fg)).on_click(
                        cx.listener(|this, _, _, cx| {
                            if this.admin_back_to_list(cx) || this.settings_back_to_list(cx) {
                                // The page went back to its list of sections.
                            } else if this.nav.history.is_empty() {
                                this.open_home(cx)
                            } else {
                                this.back(cx)
                            }
                        }),
                    ))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(8.))
                        .text_size(px(17.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .truncate()
                        .child(self.page_title()),
                )
                .children(self.render_offline_chip(cx))
                .children(self.render_downloads_chip(cx))
                .child(
                    Menu::new(&self.popups.profile_menu, "Account")
                        .trigger(div().id("top.account").child(avatar)),
                )
                .into_any_element();
        }
        div()
            .relative()
            .h(px(TOPBAR_H + top_inset()))
            .pt(px(top_inset()))
            .flex_shrink_0()
            .px(px(20.))
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl(px(if cfg!(target_os = "macos") { TRAFFIC_LIGHTS_W } else { 0. }))
                    .flex()
                    .items_center()
                    .when(!is_home, |el| {
                        el.child(
                            round("top.back", "Back", icon(LucideIcon::ArrowLeft, 18., fg))
                                .tooltip(tip("Back (Esc)"))
                                .on_click(cx.listener(
                                |this, _, _, cx| {
                                    if this.nav.history.is_empty() {
                                        this.open_home(cx)
                                    } else {
                                        this.back(cx)
                                    }
                                },
                            )),
                        )
                    })
                    // While another device plays, what it plays.
                    .children(self.cast_topbar_chip(cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(6.))
                    // "<server> is offline", with Retry, while the server
                    // does not answer.
                    .children(self.render_offline_chip(cx))
                    // A ring with the percent while something downloads.
                    // "Restart to update", while a downloaded version waits.
                    .children(self.render_update_chip(cx))
                    .children(self.render_downloads_chip(cx))
                    .when(self.session.is_some(), |el| {
                        let casting = self.cast.active();
                        el.child(
                            round("top.cast", "Play on", filled(Filled::Cast, 22., fg))
                                .when(casting, |el| el.bg(lit))
                                .tooltip(tip(if casting { "Play on: another device plays" } else { "Play on" }))
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_cast_panel(window, cx))),
                        )
                    })
                    .when(self.sync.allowed() && self.sync.session.is_some(), |el| {
                        let in_group = self.sync.in_group();
                        el.child(
                            round("top.syncplay", "SyncPlay", filled(Filled::Groups, 22., fg))
                                .when(in_group, |el| el.bg(lit))
                                .tooltip(tip(if in_group {
                                    "SyncPlay: in a group"
                                } else {
                                    "SyncPlay"
                                }))
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_sync_panel(window, cx))),
                        )
                    })
                    .child(
                        round("top.random", "Random item", icon(LucideIcon::Dices, 18., fg))
                            .tooltip(tip("Random item (R)"))
                            .on_click(cx.listener(|this, _, _, cx| this.open_random(cx))),
                    )
                    .child(
                        round("top.search", "Search", icon(LucideIcon::Search, 18., fg))
                            .tooltip(tip("Search (/)"))
                            .on_click(cx.listener(|this, _, window, cx| this.focus_search(window, cx))),
                    )
                    .child(
                        Menu::new(&self.popups.profile_menu, "Account")
                            // The avatar is the whole button; no frame around it.
                            .trigger(div().id("top.account").tooltip(tip("Account")).child(avatar)),
                    ),
            )
            // The tabs sit at the centre of the window, not of the space the
            // side groups leave, so the traffic lights do not push them right.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(tabs),
            )
            .into_any_element()
    }

    /// The name of the server for the offline texts.
    fn offline_server(&self) -> String {
        self.session
            .as_ref()
            .map(|s| s.server_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "The server".to_string())
    }

    /// The texts of the offline state: why the server does not answer, in
    /// plain words, and when the app tries again.
    fn offline_texts(&self) -> (String, String) {
        let core = &self.connection.core;
        let reason = core.why.map(crate::connection::plain_words).unwrap_or_default();
        let next = match core.retry_in(std::time::Instant::now()) {
            Some(secs) if secs > 0 => format!("Trying again in {secs} s."),
            _ => "Trying again now…".to_string(),
        };
        (reason, next)
    }

    /// True when the page has nothing to show because the server does not
    /// answer (or a probe runs after a failed load). Content that is on
    /// screen stays; the chip says it then.
    fn offline_page_shows(&self) -> bool {
        use crate::connection::State;
        let state = self.connection.core.state;
        if state == State::Online || self.session.is_none() {
            return false;
        }
        if state == State::Checking && !self.connection.page_failed {
            return false;
        }
        match &self.nav.page {
            Page::Home(d) => {
                !d.loading
                    && d.resume.is_empty()
                    && d.next_up.is_empty()
                    && d.latest.is_empty()
                    && self.hero.slides.is_empty()
            }
            Page::Library(d) => !d.loading && d.items.is_empty(),
            Page::Playlist(d) => !d.loading && d.entries.is_empty(),
            Page::Admin(d) => !d.loading && d.error.is_some(),
            _ => false,
        }
    }

    /// The offline state as a page: the server, the reason in plain words,
    /// the next try, Retry, and Open Downloads when there are downloads.
    fn render_offline_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::connection::State;
        use crate::ui::button::raised;
        let fg = UiTheme::read(cx).colors.foreground;
        let core = &self.connection.core;
        let server = self.offline_server();
        let checking = core.state == State::Checking;
        let title = if checking {
            format!("Checking the connection to {server}…")
        } else {
            format!("{server} is offline")
        };
        let (mut hint, next) = self.offline_texts();
        if !checking {
            hint.push(' ');
            hint.push_str(&next);
        }
        let mut actions = div().flex().items_center().gap(px(10.));
        if !checking {
            actions = actions.child(
                raised("offline.retry", "Retry", cx)
                    .on_click(cx.listener(|this, _: &gpui_kit::ClickEvent, _, cx| this.retry_connection(cx))),
            );
            if self.has_downloads() {
                actions = actions.child(
                    raised("offline.downloads", "Open Downloads", cx)
                        .on_click(cx.listener(|this, _: &gpui_kit::ClickEvent, _, cx| this.open_downloads(cx))),
                );
            }
        }
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.))
                    .max_w(px(520.))
                    .px(px(20.))
                    .child(
                        div()
                            .size(px(64.))
                            .rounded_full()
                            .bg(tone::DISC)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(LucideIcon::WifiOff, 28., fg)),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .text_size(px(22.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .text_color(fg.opacity(0.7))
                            .text_center()
                            .child(hint),
                    )
                    .child(actions.mt(px(12.))),
            )
    }

    /// The quiet chip of the top bar while the server is offline: an amber
    /// dot (a state), the server, Retry, and Downloads when there are
    /// downloads. The reason and the next try are in its tooltip.
    pub fn render_offline_chip(&self, cx: &mut Context<Self>) -> Option<gpui_kit::Stateful<gpui_kit::Div>> {
        if !self.connection.core.is_offline() {
            return None;
        }
        let fg = UiTheme::read(cx).colors.foreground;
        let server = self.offline_server();
        let (reason, next) = self.offline_texts();
        let action = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(26.))
                .px(px(9.))
                .rounded_full()
                .flex()
                .items_center()
                .cursor_pointer()
                .hover(|s| s.bg(gpui_kit::rgba(0xffffff29)))
                .child(label)
        };
        Some(
            div()
                .id("top.offline")
                .h(px(36.))
                .pl(px(12.))
                .pr(px(5.))
                .rounded_full()
                .bg(tone::HOVER)
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(13.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(fg)
                .tooltip(tip(format!("{server} is offline. {reason} {next}")))
                // Amber: the state "needs a look".
                .child(div().size(px(8.)).rounded_full().bg(gpui_kit::rgb(0xe3b341)))
                .child(div().mr(px(4.)).child(format!("{server} is offline")))
                .child(
                    action("top.offline.retry", "Retry")
                        .on_click(cx.listener(|this, _, _, cx| this.retry_connection(cx))),
                )
                .when(self.has_downloads(), |el| {
                    el.child(
                        action("top.offline.downloads", "Downloads")
                            .on_click(cx.listener(|this, _, _, cx| this.open_downloads(cx))),
                    )
                }),
        )
    }
}
