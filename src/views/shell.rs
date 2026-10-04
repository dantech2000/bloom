// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Main screen: top bar and page content.

use gpui_icons::LucideIcon;
use gpui_kit::{
    Context, Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, linear_color_stop,
    linear_gradient,
    prelude::FluentBuilder as _, px,
};

use crate::{
    app::{Bloom, Page},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        glass::glass,
        menu::{Menu, MenuItem},
        theme::UiTheme,
        tip::tip,
    },
    views::cards::icon,
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

impl Bloom {
    pub fn render_shell(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The player covers the window, so the page behind it is not built.
        if self.player_open {
            return div()
                .flex()
                .size_full()
                .child(self.render_player(window, cx))
                .into_any_element();
        }
        let page = match &self.page {
            Page::Home(_) => self.render_home(cx).into_any_element(),
            Page::Library(_) => self.render_library(cx).into_any_element(),
            Page::Detail(_) => self.render_detail(cx).into_any_element(),
            Page::Search(_) => self.render_search(cx).into_any_element(),
            Page::Admin(_) => self.render_admin(cx).into_any_element(),
            Page::Settings(_) => self.render_settings(cx).into_any_element(),
            Page::Playlist(_) => self.render_playlist(cx).into_any_element(),
            Page::Downloads => self.render_downloads(cx).into_any_element(),
        };
        // On home and detail pages the bar lies over the artwork; other
        // pages start below it.
        let overlay = matches!(self.page, Page::Home(_) | Page::Detail(_));
        let scrolled = f32::from(self.page_scroll.offset().y) < -8.;
        div()
            .id("shell")
            .track_focus(&self.app_focus)
            .on_key_down(cx.listener(Self::shell_key))
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    // The server dashboard has the colours of the Jellyfin
                    // dashboard behind it: blue at the left, purple at the
                    // right, fading into the page at the bottom. The cards
                    // are glass, so the colour comes through them.
                    .when(matches!(self.page, Page::Admin(_)), |el| {
                        el.child(div().absolute().inset_0().bg(linear_gradient(
                            100.,
                            linear_color_stop(gpui_kit::rgba(0x0b2f8a61), 0.0),
                            linear_color_stop(gpui_kit::rgba(0x5b1f8f6b), 1.0),
                        )))
                        .child(div().absolute().inset_0().bg(linear_gradient(
                            180.,
                            linear_color_stop(gpui_kit::rgba(0x10101000), 0.25),
                            linear_color_stop(gpui_kit::rgba(0x101010f2), 1.0),
                        )))
                    })
                    .child(
                        div()
                            .size_full()
                            .when(!overlay, |el| el.pt(px(TOPBAR_H)))
                            .child(page),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .when(overlay && !scrolled, |el| {
                                el.bg(linear_gradient(
                                    180.,
                                    linear_color_stop(gpui_kit::black().alpha(0.55), 0.0),
                                    linear_color_stop(gpui_kit::black().alpha(0.0), 1.0),
                                ))
                            })
                            // Content scrolls under the bar; fade it out there.
                            .when(overlay && scrolled, |el| {
                                el.pb(px(28.)).bg(linear_gradient(
                                    180.,
                                    linear_color_stop(gpui_kit::black().alpha(0.55), 0.0),
                                    linear_color_stop(gpui_kit::black().alpha(0.0), 1.0),
                                ))
                            })
                            .child(self.render_topbar(scrolled, cx)),
                    ),
            )
            .into_any_element()
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
        log::debug!("shell key: {keystroke:?}");
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
        // Keys typed into the search field are text, not shortcuts. Only
        // Escape acts there: it takes the focus off the field.
        if self.search_input.read(cx).focus_handle(cx).contains_focused(window, cx) {
            if keystroke.key == "escape" {
                window.focus(&self.app_focus, cx);
                cx.stop_propagation();
            }
            return;
        }
        // The hero keys apply while the hero is in view on the home page.
        let on_hero = matches!(self.page, Page::Home(_))
            && !self.hero.is_empty()
            && -f32::from(self.page_scroll.offset().y) < self.hero_height();
        // The list of keys and the shortcuts of Jellyfin Enhanced.
        if self.enhanced_shell_key(event, window, cx) {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        match keystroke.key.as_str() {
            "[" if modifiers.platform => self.back(cx),
            "escape" | "backspace" if !self.history.is_empty() => self.back(cx),
            "/" => {
                let query = self.search_input.read(cx).value().to_string();
                self.open_search(query, cx);
                let focus = self.search_input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
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
        let root = self.history.first().unwrap_or(&self.page);
        if let Page::Library(data) = root {
            return Some(data.view.id.clone());
        }
        match &self.page {
            Page::Detail(data) => match data.item.kind.as_str() {
                "Movie" => by_type("movies"),
                "Series" | "Season" | "Episode" => by_type("tvshows"),
                "BoxSet" => by_type("boxsets"),
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
        let dark = self.config.dark.unwrap_or(true);
        let handle = this.clone();
        items.push(
            MenuItem::new(
                "menu.theme",
                if dark {
                    "Light appearance"
                } else {
                    "Dark appearance"
                },
            )
            .icon(if dark { LucideIcon::Sun } else { LucideIcon::Moon })
            .on_click(move |_, _, cx| {
                handle.update(cx, |this, cx| this.toggle_theme(cx)).ok();
            }),
        );
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
        items.extend(self.enhanced_menu(cx));
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
                        .detail("Movies and episodes kept on this Mac")
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
        self.profile_menu
            .update(cx, |menu, cx| menu.set_items(items, cx));
    }

    fn render_topbar(&self, scrolled: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let t = UiTheme::read(cx).clone();
        let fg = t.colors.foreground;
        let active = self.active_tab();
        let is_home = matches!(self.page, Page::Home(_)) && self.history.is_empty();
        let server = self
            .session
            .as_ref()
            .map(|s| s.server_name.clone())
            .unwrap_or_default();

        let tab = |id: SharedString, label: String, glyph: Glyph, selected: bool| {
            let color = if selected {
                t.colors.primary_foreground
            } else {
                fg
            };
            div()
                .id(id)
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
                .when(selected, |el| el.bg(t.colors.primary))
                .when(!selected, |el| el.hover(|s| s.bg(fg.opacity(0.08))))
                .child(match glyph {
                    Glyph::Lucide(glyph) => icon(glyph, 16., color),
                    // The web theme shows the mark in white, a little dimmed.
                    Glyph::Logo => crate::icons::logo(19., color.opacity(0.85).into()),
                })
                .child(label)
        };

        // The web gives the tab group a glass pill once the page scrolls.
        let mut tabs = div()
            .p(px(4.))
            .rounded_full()
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .relative()
            .when(scrolled, |el| {
                el.child(glass(px(999.), gpui_kit::rgba(0x2a2a2ab0)))
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
            let glyph = match view.collection_type.as_deref() {
                Some("movies") => LucideIcon::Clapperboard,
                Some("tvshows") => LucideIcon::Tv,
                Some("music") => LucideIcon::Music,
                Some("homevideos") | Some("photos") => LucideIcon::Image,
                Some("boxsets") => LucideIcon::GalleryVerticalEnd,
                _ => LucideIcon::Folder,
            };
            let target = view.clone();
            tabs = tabs.child(
                tab(
                    SharedString::from(format!("nav.lib.{}", view.id)),
                    view.name.clone(),
                    Glyph::Lucide(glyph),
                    active.as_deref() == Some(view.id.as_str()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_library(target.clone(), cx))),
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

        let round = |id: &'static str, label: &'static str, glyph: LucideIcon| {
            Button::new(id)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Icon)
                .aria_label(label)
                .child(icon(glyph, 18., fg))
        };

        let (name, image) = match &self.session {
            Some(s) => (s.user_name.clone(), s.user_image.clone()),
            None => (String::new(), None),
        };
        let initial: String = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        let avatar = crate::ui::avatar::Avatar::new("profile.avatar")
            .fallback(initial)
            .aria_label(name)
            .when_some(
                image.and_then(|url| crate::images::image(&url, cx)),
                |a, image| a.image(image),
            );

        div()
            .relative()
            .h(px(TOPBAR_H))
            .flex_shrink_0()
            .px(px(20.))
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl(px(TRAFFIC_LIGHTS_W))
                    .flex()
                    .items_center()
                    .when(!is_home, |el| {
                        el.child(
                            round("top.back", "Back", LucideIcon::ArrowLeft)
                                .tooltip(tip("Back (Esc)"))
                                .on_click(cx.listener(
                                |this, _, _, cx| {
                                    if this.history.is_empty() {
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
                    // A ring with the percent while something downloads.
                    .children(self.render_downloads_chip(cx))
                    .when(self.session.is_some(), |el| {
                        let casting = self.cast.active();
                        el.child(
                            Button::new("top.cast")
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Icon)
                                .aria_label("Play on")
                                .child(crate::icons::filled(crate::icons::Filled::Cast, 22., fg))
                                .when(casting, |el| el.bg(gpui_kit::rgba(0xf5f5f733)))
                                .tooltip(tip(if casting { "Play on: another device plays" } else { "Play on" }))
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_cast_panel(window, cx))),
                        )
                    })
                    .when(self.sync.allowed() && self.sync.session.is_some(), |el| {
                        let in_group = self.sync.in_group();
                        el.child(
                            Button::new("top.syncplay")
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Icon)
                                .aria_label("SyncPlay")
                                .child(crate::icons::filled(crate::icons::Filled::Groups, 22., fg))
                                .when(in_group, |el| el.bg(gpui_kit::rgba(0xf5f5f733)))
                                .tooltip(tip(if in_group {
                                    "SyncPlay: in a group"
                                } else {
                                    "SyncPlay"
                                }))
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_sync_panel(window, cx))),
                        )
                    })
                    .child(
                        round("top.random", "Random item", LucideIcon::Dices)
                            .tooltip(tip("Random item (R)"))
                            .on_click(cx.listener(|this, _, _, cx| this.open_random(cx))),
                    )
                    .child(
                        round("top.search", "Search", LucideIcon::Search)
                            .tooltip(tip("Search (/)"))
                            .on_click(cx.listener(
                            |this, _, window, cx| {
                                let query = this.search_input.read(cx).value().to_string();
                                this.open_search(query, cx);
                                let focus = this.search_input.read(cx).focus_handle(cx);
                                window.focus(&focus, cx);
                            },
                        )),
                    )
                    .child(
                        Menu::new(&self.profile_menu, "Account")
                            // The avatar is the whole button; no frame around it.
                            .trigger_style_with(|button| button)
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
    }
}
