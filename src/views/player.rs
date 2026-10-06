// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Embedded video view, laid out like the web player with the Abyss theme:
//! the video fills the window, a title bar lies over its top, and the
//! controls sit in a floating glass panel at the bottom.

use gpui_icons::LucideIcon;
use gpui_kit::{
    Bounds, ClickEvent, Context, Corners, Div, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ObjectFit, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled, Svg, Window, canvas, div, linear_color_stop,
    linear_gradient, prelude::FluentBuilder as _, px, relative, rgb, rgba, surface,
};

use crate::{
    app::Bloom,
    icons::{Filled, filled},
    jellyfin::format_duration,
    player::PlayState,
    queue::PreviewFrame,
    ui::{glass::glass, menu::Menu, slider::Slider, theme::UiTheme, tip::tip},
    views::cards::icon,
};

/// Space between the control panel and the window edges.
const PANEL_MARGIN: f32 = 24.;
/// Width of the preview image over the timeline.
const PREVIEW_W: f32 = 240.;
/// Width of the preview when the item has no preview images: time and
/// chapter name only.
const PREVIEW_TEXT_W: f32 = 160.;

impl Bloom {
    pub fn render_player(&self, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        if self.pip.is_some() {
            return self.render_pip(window, cx);
        }
        let t = UiTheme::read(cx).clone();
        let s = &self.player_status;
        let white = rgb(0xffffff);
        let soft = rgba(0xffffffb3);
        let starting = s.state == PlayState::Starting;
        let fullscreen = window.is_fullscreen();

        let player = self.player.clone();
        let scale = window.scale_factor();
        // Reports the video area to the renderer so frames are sized to fit.
        let measure = canvas(
            move |bounds, _, _| {
                player.set_target_size(
                    (f32::from(bounds.size.width) * scale) as u32,
                    (f32::from(bounds.size.height) * scale) as u32,
                );
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let video = div()
            .id("player.surface")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(measure)
            // A click on the video closes the episode list when it is open;
            // otherwise it pauses or resumes.
            .on_click(cx.listener(|this, _, _, cx| {
                if this.episode_picker.open || this.sync.panel_open || this.cast.panel_open || this.subs.panel_open {
                    this.subs.panel_open = false;
                    this.episode_picker.open = false;
                    this.sync.panel_open = false;
                    this.cast.panel_open = false;
                    cx.notify();
                } else {
                    this.request_toggle_pause(cx)
                }
            }))
            .when_some(self.current_frame.clone(), |el, frame| {
                el.child(
                    surface(frame.buffer())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
            })
            .when(
                self.current_frame.is_none() || starting || s.buffering,
                |el| {
                    el.child(
                        div()
                            .absolute()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(10.))
                            .text_color(soft)
                            .child(icon(LucideIcon::LoaderCircle, 28., soft))
                            .child(if starting {
                                "Opening stream…"
                            } else {
                                "Buffering…"
                            }),
                    )
                },
            );

        // Hitboxes don't block by default, so overlay clicks would reach the
        // video and toggle pause too. Swallow them here, keeping keyboard
        // focus on the player.
        fn swallow(
            this: &mut Bloom,
            _: &MouseDownEvent,
            window: &mut Window,
            cx: &mut Context<Bloom>,
        ) {
            window.focus(&this.player_focus, cx);
            cx.stop_propagation();
        }

        let button = |id: &'static str, glyph: Svg| {
            div()
                .id(id)
                .size(px(40.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xffffff1f)))
                .child(glyph)
        };
        // A menu whose trigger looks like the other panel buttons.
        let menu_button = |state: &gpui_kit::Entity<crate::ui::menu::MenuState>,
                           label: &'static str,
                           glyph: Svg| {
            Menu::new(state, label)
                .trigger_style_with(|button| button)
                .trigger(
                    div()
                        .id(label)
                        .tooltip(tip(label))
                        .size(px(40.))
                        .rounded(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(rgba(0xffffff1f)))
                        .child(glyph),
                )
        };

        // Controls overlay the video and fade with pointer activity.
        let visibility = crate::ui::theme::transition_value(
            "player.controls-opacity",
            if self.controls_visible { 1. } else { 0. },
            t.motion.normal,
            window,
            cx,
        );
        // The header and the panel are built only while they show or fade.
        // A video frame redraws the whole player; hidden controls must not
        // cost a build on each one.
        let chrome = (self.controls_visible || visibility > 0.).then(|| {
            let remaining = (s.duration - s.position).max(0.);
            let label = |text: String| {
                div()
                    .w(px(64.))
                    .flex_shrink_0()
                    .text_size(px(13.))
                    .text_color(soft)
                    .child(text)
            };
            let seek = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(label(format_duration(s.position as i64)))
                .child(self.render_timeline(cx))
                .child(
                    label(if s.duration > 0. {
                        format!("-{}", format_duration(remaining as i64))
                    } else {
                        String::new()
                    })
                    .flex()
                    .justify_end(),
                );

            // The header shows the rating when that feature is on; then the panel
            // does not show it a second time.
            let rating = self
                .playing
                .as_ref()
                .and_then(|item| item.community_rating)
                .filter(|_| self.enhanced_player_rating().is_none());
            let favorite = self
                .playing
                .as_ref()
                .is_some_and(|item| item.user_data.is_favorite);
            let has_subs = s.tracks.iter().any(|track| track.kind == "sub");
            let silent = self.muted || self.volume == 0.;

            let has_previous = !self.queue.history.is_empty();
            let has_next = !self.queue.upcoming.is_empty();
            let mut left = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .when(has_previous || has_next, |el| {
                    el.child(
                        button(
                            "player.previous",
                            icon(LucideIcon::SkipBack, 20., white),
                        )
                        .tooltip(tip("Previous (Shift+P)"))
                        .on_click(cx.listener(|this, _, _, cx| this.request_previous(cx))),
                    )
                })
                .child(
                    button("player.rewind", filled(Filled::Rewind, 24., white))
                        .tooltip(tip(format!("Back {} seconds", self.prefs.skip_back_secs() as u32)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_seek_by(-this.prefs.skip_back_secs(), cx)
                        })),
                )
                .child(
                    button(
                        "player.pause",
                        filled(
                            if s.paused {
                                Filled::Play
                            } else {
                                Filled::Pause
                            },
                            26.,
                            white,
                        ),
                    )
                    .tooltip(tip(if s.paused { "Play (Space)" } else { "Pause (Space)" }))
                    .on_click(cx.listener(|this, _, _, cx| this.request_toggle_pause(cx))),
                )
                .child(
                    button("player.forward", filled(Filled::Forward, 24., white))
                        .tooltip(tip(format!(
                            "Forward {} seconds",
                            self.prefs.skip_forward_secs() as u32
                        )))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_seek_by(this.prefs.skip_forward_secs(), cx)
                        })),
                )
                .when(has_next, |el| {
                    el.child(
                        button("player.next", icon(LucideIcon::SkipForward, 20., white))
                            .tooltip(tip("Next (Shift+N)"))
                            .on_click(
                            cx.listener(|this, _, _, cx| {
                                this.request_next(cx);
                            }),
                        ),
                    )
                });
            if let Some(score) = rating {
                left = left.child(
                    div()
                        .ml(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_size(px(14.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(rgb(0xffc107))
                        .child(filled(Filled::Star, 16., rgb(0xffc107)))
                        .child(format!("{score:.1}")),
                );
            }
            if s.duration > 0. {
                left = left.child(
                    div()
                        .ml(px(10.))
                        .text_size(px(14.))
                        .text_color(soft)
                        .child(format!(
                            "Ends at {}",
                            crate::macos::ends_at((remaining / self.speed.max(0.25) as f64) as i64)
                        )),
                );
            }

            let right = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    button(
                        "player.favorite",
                        filled(
                            Filled::Heart,
                            24.,
                            if favorite { rgb(0xf92672) } else { white },
                        ),
                    )
                    .tooltip(tip(if favorite {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    }))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_playing_favorite(cx))),
                )
                .child(menu_button(
                    &self.subtitle_menu,
                    "Subtitles",
                    filled(Filled::Captions, 24., if has_subs { white } else { soft }),
                ))
                .child(
                    button(
                        "player.mute",
                        filled(
                            if silent {
                                Filled::VolumeOff
                            } else {
                                Filled::VolumeUp
                            },
                            24.,
                            white,
                        ),
                    )
                    .tooltip(tip(if silent { "Unmute (M)" } else { "Mute (M)" }))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx))),
                )
                .child(
                    div().w(px(120.)).mr(px(8.)).child(
                        Slider::new(&self.volume_slider)
                            .aria_label("Volume")
                            .w_full(),
                    ),
                )
                .children(self.bookmark_button(cx))
                .when(self.can_pick_episode(), |el| {
                    el.child(
                        button("player.episodes", icon(LucideIcon::ListVideo, 22., white))
                            .tooltip(tip("Episodes (E)"))
                            .on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.toggle_episode_picker(window, cx)
                            }),
                        ),
                    )
                })
                .child(menu_button(
                    &self.settings_menu,
                    "Settings",
                    filled(Filled::Settings, 24., white),
                ))
                .when(self.sync.allowed() && self.sync.session.is_some(), |el| {
                    let in_group = self.sync.in_group();
                    el.child(
                        button(
                            "player.syncplay",
                            filled(Filled::Groups, 26., if in_group { rgb(0x7ee787) } else { white }),
                        )
                        .tooltip(tip(if in_group { "SyncPlay: in a group" } else { "SyncPlay" }))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_sync_panel(window, cx)
                        })),
                    )
                })
                .child(
                    button("player.cast", filled(Filled::Cast, 24., if self.cast.active() { rgb(0x7ee787) } else { white }))
                        .tooltip(tip(if self.cast.active() { "Play on: another device plays" } else { "Play on" }))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_cast_panel(window, cx)
                        })),
                )
                .child(
                    button(
                        "player.pip",
                        icon(LucideIcon::PictureInPicture2, 22., white),
                    )
                    .tooltip(tip("Picture in picture"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_pip(window, cx)
                    })),
                )
                .child(
                    button(
                        "player.fullscreen",
                        filled(
                            if fullscreen {
                                Filled::FullscreenExit
                            } else {
                                Filled::Fullscreen
                            },
                            24.,
                            white,
                        ),
                    )
                    .tooltip(tip(if fullscreen {
                        "Exit full screen (F)"
                    } else {
                        "Enter full screen (F)"
                    }))
                    .on_click(|_: &ClickEvent, window, _| window.toggle_fullscreen()),
                );

            let panel = div()
                .absolute()
                .left(px(PANEL_MARGIN))
                .right(px(PANEL_MARGIN))
                .bottom(px(PANEL_MARGIN))
                .rounded(px(24.))
                .opacity(visibility)
                .on_mouse_down(MouseButton::Left, cx.listener(swallow))
                .child(glass(px(24.), rgba(0x1c1c1cb8)))
                .px(px(20.))
                .pt(px(10.))
                .pb(px(8.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(seek)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(left)
                        .child(right),
                );

            let header = div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(110.))
                .opacity(visibility)
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(gpui_kit::black().alpha(0.75), 0.0),
                    linear_color_stop(gpui_kit::black().alpha(0.0), 1.0),
                ))
                .child(
                    div()
                        .absolute()
                        .top(px(12.))
                        // The traffic lights sit at the left edge of a window.
                        .left(px(if fullscreen { 20. } else { 84. }))
                        .right(px(24.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .on_mouse_down(MouseButton::Left, cx.listener(swallow))
                        .child(
                            // Back ends playback and returns to the page before.
                            button("player.close", icon(LucideIcon::ArrowLeft, 20., white))
                                .tooltip(tip("Back (Esc)"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_stop(cx);
                                })),
                        )
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(white)
                                .truncate()
                                .child(s.title.clone()),
                        )
                        .children(self.enhanced_player_rating())
                        .children(self.sync_player_chip(cx)),
                );
            (header, panel)
        });

        // Near the end, the card of the next item takes the place of the
        // skip button.
        let up_next = self.render_up_next(cx);
        // "Skip Intro" and its relatives, from the server's marked ranges.
        let skip = self.current_segment().filter(|_| up_next.is_none()).map(|segment| {
            let end = segment.end_secs();
            let text = match segment.kind.as_str() {
                "Intro" => "Skip Intro",
                "Outro" => "Skip Credits",
                "Recap" => "Skip Recap",
                "Preview" => "Skip Preview",
                _ => "Skip",
            };
            div()
                .id("player.skip")
                .absolute()
                .right(px(PANEL_MARGIN + 24.))
                .bottom(px(PANEL_MARGIN + 110.))
                .h(px(44.))
                .px(px(20.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .child(glass(px(12.), rgba(0x303030cc)))
                .text_size(px(16.))
                .font_weight(gpui_kit::FontWeight::BOLD)
                .text_color(rgba(0xffffffde))
                .on_mouse_down(MouseButton::Left, cx.listener(swallow))
                .child(text)
                .child(icon(LucideIcon::SkipForward, 18., white))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.request_seek_to(end, cx)
                }))
        });

        div()
            .id("player.root")
            .track_focus(&self.player_focus)
            .relative()
            .size_full()
            .bg(gpui_kit::black())
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    let shift = event.keystroke.modifiers.shift;
                    match event.keystroke.key.as_str() {
                        // The label field of the bookmarks panel gets the typing.
                        "escape" if this.bookmarks_escape(window, cx) => {}
                        _ if this.bookmark_typing(window, cx) => return,
                        "n" if shift => {
                            this.request_next(cx);
                        }
                        "p" if shift => this.request_previous(cx),
                        "space" | "k" => this.request_toggle_pause(cx),
                        "left" => this.request_seek_by(-5., cx),
                        "right" => this.request_seek_by(5., cx),
                        "j" => this.request_seek_by(-10., cx),
                        "l" => this.request_seek_by(10., cx),
                        "up" => this.nudge_volume(5., window, cx),
                        "down" => this.nudge_volume(-5., window, cx),
                        "m" => this.toggle_mute(cx),
                        "f" => window.toggle_fullscreen(),
                        "escape" if this.episode_picker.open || this.sync.panel_open || this.cast.panel_open || this.subs.panel_open => {
                            this.subs.panel_open = false;
                            this.episode_picker.open = false;
                            this.sync.panel_open = false;
                            this.cast.panel_open = false;
                        }
                        "e" => this.toggle_episode_picker(window, cx),
                        "escape" => {
                            if window.is_fullscreen() {
                                window.toggle_fullscreen();
                            } else {
                                this.request_stop(cx);
                            }
                        }
                        // The player shortcuts of Jellyfin Enhanced.
                        _ if this.enhanced_player_key(event, cx) => {}
                        _ if this.subs_key(event, cx) => {}
                        _ => return,
                    }
                    this.show_controls();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.player_focus, cx);
            }))
            .on_mouse_move(cx.listener(|this, _, _, cx| {
                let was_hidden = !this.controls_visible;
                this.show_controls();
                if was_hidden {
                    cx.notify();
                }
            }))
            .child(video)
            .when(!self.pause_screen, |el| {
                el.children(skip)
                    .children(up_next)
                    .when_some(chrome, |el, (header, panel)| el.child(header).child(panel))
                    .children(self.render_episode_picker(PANEL_MARGIN + 110., cx))
                    .children(self.render_bookmarks_panel(cx))
                    .children(self.render_sync_panel(None, cx))
                    .children(self.render_cast_panel(None, cx))
                    .children(self.render_subs_panel(None, cx))
                    .children(self.render_stream_info(cx))
            })
            .when(self.pause_screen, |el| {
                el.children(self.render_pause_screen(window, cx))
            })
    }

    /// The seek slider with its chapter marks, and over it the preview of
    /// the time under the pointer.
    fn render_timeline(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let duration = self.player_status.duration;
        let bounds = self.queue.timeline_bounds.clone();
        let measure = canvas(move |painted, _, _| bounds.set(painted), |_, _, _, _| {})
            .absolute()
            .size_full();
        // A chapter starts at each mark. The first one starts with the video.
        let marks = self
            .queue
            .timeline
            .chapters
            .iter()
            .map(|chapter| chapter.start_secs())
            .filter(|start| duration > 0. && *start > 1. && *start < duration - 1.)
            .map(|start| {
                div()
                    .absolute()
                    .left(relative((start / duration) as f32))
                    .ml(px(-1.))
                    .top(px(7.))
                    .w(px(2.))
                    .h(px(6.))
                    .bg(rgba(0x000000cc))
            })
            .collect::<Vec<_>>();

        let width = f32::from(self.queue.timeline_bounds.get().size.width);
        let preview = self
            .timeline_hover_secs(cx)
            .filter(|_| width > 0.)
            .map(|secs| {
                let frame = self.preview_frame(secs);
                let box_w = if frame.is_some() {
                    PREVIEW_W
                } else {
                    PREVIEW_TEXT_W
                };
                let x = (secs / duration) as f32 * width;
                let left = (x - box_w / 2.).clamp(0., (width - box_w).max(0.));
                let chapter = self
                    .chapter_at(secs)
                    .map(|chapter| chapter.name.clone())
                    .filter(|name| !name.is_empty());
                div()
                    .absolute()
                    .left(px(left))
                    .bottom(px(30.))
                    .w(px(box_w))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.))
                    .children(frame.map(|frame| {
                        div()
                            .w(px(PREVIEW_W))
                            .h(px(PREVIEW_W * frame.aspect))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(rgba(0xffffff66))
                            .bg(rgb(0x101010))
                            .child(preview_image(frame, self.queue.preview_last.clone()))
                    }))
                    .child(
                        div()
                            .relative()
                            .max_w(px(box_w))
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(8.))
                            .child(glass(px(8.), rgba(0x1c1c1ce6)))
                            .flex()
                            .flex_col()
                            .items_center()
                            .text_color(rgb(0xffffff))
                            .children(chapter.map(|name| {
                                div()
                                    .max_w(px(box_w - 20.))
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(rgba(0xffffffb3))
                                    .child(name)
                            }))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(format_duration(secs as i64)),
                            ),
                    )
            });

        div()
            .id("player.timeline")
            .relative()
            .flex_1()
            .min_w_0()
            .child(measure)
            .child(Slider::new(&self.seek_slider).aria_label("Seek").w_full())
            .children(marks)
            .children(self.bookmark_marks())
            .children(preview)
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let bounds = this.queue.timeline_bounds.get();
                if bounds.size.width <= px(0.) {
                    return;
                }
                let place =
                    ((event.position.x - bounds.origin.x) / bounds.size.width).clamp(0., 1.);
                if this.queue.hover != Some(place) {
                    this.queue.hover = Some(place);
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.queue.hover.take().is_some() {
                    cx.notify();
                }
            }))
    }

    /// The card of the item that plays next, in the last moments of an item.
    fn render_up_next(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let (next, secs) = self.up_next()?;
        let client = self.session.as_ref()?.client.clone();
        let white = rgb(0xffffff);
        let thumb = next.wide_url(&client, 480).map(|url| {
            crate::images::remote_with(url, px(10.), ObjectFit::Cover)
                .w(px(160.))
                .h(px(90.))
                .flex_shrink_0()
        });
        let action = |id: &'static str, text: &'static str, primary: bool| {
            div()
                .id(id)
                .h(px(34.))
                .px(px(14.))
                .rounded(px(10.))
                .flex()
                .items_center()
                .cursor_pointer()
                .text_size(px(14.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .when(primary, |el| el.bg(white).text_color(rgb(0x121212)))
                .when(!primary, |el| el.bg(rgba(0xffffff24)).text_color(white))
                .hover(|s| s.opacity(0.88))
                .child(text)
        };
        Some(
            div()
                .id("player.up-next")
                .absolute()
                .right(px(PANEL_MARGIN + 24.))
                .bottom(px(PANEL_MARGIN + 110.))
                .w(px(440.))
                .p(px(12.))
                .rounded(px(16.))
                .border_1()
                .border_color(rgba(0xffffff26))
                .child(glass(px(16.), rgba(0x1c1c1cd9)))
                .flex()
                .gap(px(14.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        window.focus(&this.player_focus, cx);
                        cx.stop_propagation();
                    }),
                )
                .children(thumb)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgba(0xffffffb3))
                                .child(format!(
                                    "Up next in {secs} second{}",
                                    if secs == 1 { "" } else { "s" }
                                )),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(16.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(white)
                                .child(match next.episode_code() {
                                    // The series is the one that plays now.
                                    Some(code)
                                        if next.series_id.is_some()
                                            && self.playing.as_ref().map(|i| &i.series_id)
                                                == Some(&next.series_id) =>
                                    {
                                        format!("{code} · {}", next.name)
                                    }
                                    _ => next.display_title(),
                                }),
                        )
                        .child(
                            div()
                                .mt(px(8.))
                                .flex()
                                .gap(px(8.))
                                .child(action("player.up-next.play", "Play Now", true).on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.request_next(cx);
                                    }),
                                ))
                                .child(action("player.up-next.hide", "Hide", false).on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.queue.up_next_hidden = true;
                                        cx.notify();
                                    }),
                                )),
                        ),
                ),
        )
    }

    /// The player as a small window: the video, and on hover a few controls
    /// to seek, pause, stop, and go back to the full player. The window moves
    /// by a drag anywhere on the video.
    fn render_pip(&self, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = UiTheme::read(cx).clone();
        let s = &self.player_status;
        let white = rgb(0xffffff);

        let player = self.player.clone();
        let scale = window.scale_factor();
        let measure = canvas(
            move |bounds, _, _| {
                player.set_target_size(
                    (f32::from(bounds.size.width) * scale) as u32,
                    (f32::from(bounds.size.height) * scale) as u32,
                );
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        fn swallow(
            this: &mut Bloom,
            _: &MouseDownEvent,
            window: &mut Window,
            cx: &mut Context<Bloom>,
        ) {
            window.focus(&this.player_focus, cx);
            cx.stop_propagation();
        }
        let visibility = crate::ui::theme::transition_value(
            "player.pip-opacity",
            if self.controls_visible { 1. } else { 0. },
            t.motion.normal,
            window,
            cx,
        );
        // No tooltip comes up over hidden controls.
        let shown = self.controls_visible;
        let button = |id: &'static str, glyph: Svg, size: f32| {
            div()
                .id(id)
                .size(px(size))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(rgba(0x00000066))
                .hover(|s| s.bg(rgba(0x000000a6)))
                .on_mouse_down(MouseButton::Left, cx.listener(swallow))
                .child(glyph)
        };

        let controls = div()
            .absolute()
            .inset_0()
            .opacity(visibility)
            .bg(rgba(0x00000059))
            .child(
                div().absolute().top(px(8.)).left(px(8.)).child(
                    button(
                        "pip.expand",
                        filled(Filled::Fullscreen, 20., white),
                        32.,
                    )
                    .when(shown, |el| el.tooltip(tip("Back to the full player")))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_pip(window, cx)
                    })),
                ),
            )
            .child(
                div().absolute().top(px(8.)).right(px(8.)).child(
                    button("pip.stop", icon(LucideIcon::X, 18., white), 32.)
                        .when(shown, |el| el.tooltip(tip("Stop")))
                        .on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.request_stop(cx);
                        }),
                    ),
                ),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(14.))
                    // Keeps the pause button in the middle when Next shows.
                    .when(!self.queue.upcoming.is_empty(), |el| {
                        el.child(div().size(px(38.)).flex_shrink_0())
                    })
                    .child(
                        button("pip.rewind", filled(Filled::Rewind, 22., white), 38.)
                            .when(shown, |el| el.tooltip(tip("Back")))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_seek_by(-this.prefs.skip_back_secs(), cx)
                            })),
                    )
                    .child(
                        button(
                            "pip.pause",
                            filled(
                                if s.paused {
                                    Filled::Play
                                } else {
                                    Filled::Pause
                                },
                                28.,
                                white,
                            ),
                            50.,
                        )
                        .when(shown, |el| el.tooltip(tip(if s.paused { "Play" } else { "Pause" })))
                        .on_click(cx.listener(|this, _, _, cx| this.request_toggle_pause(cx))),
                    )
                    .child(
                        button("pip.forward", filled(Filled::Forward, 22., white), 38.)
                            .when(shown, |el| el.tooltip(tip("Forward")))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_seek_by(this.prefs.skip_forward_secs(), cx)
                            })),
                    )
                    .when(!self.queue.upcoming.is_empty(), |el| {
                        el.child(
                            button("pip.next", icon(LucideIcon::SkipForward, 18., white), 38.)
                                .when(shown, |el| el.tooltip(tip("Next")))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_next(cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .right(px(12.))
                    .bottom(px(4.))
                    .on_mouse_down(MouseButton::Left, cx.listener(swallow))
                    .child(Slider::new(&self.seek_slider).aria_label("Seek").w_full()),
            );

        div()
            .id("player.root")
            .track_focus(&self.player_focus)
            .relative()
            .size_full()
            .bg(gpui_kit::black())
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "space" | "k" => this.request_toggle_pause(cx),
                        "left" => this.request_seek_by(-5., cx),
                        "right" => this.request_seek_by(5., cx),
                        "m" => this.toggle_mute(cx),
                        "escape" => this.toggle_pip(window, cx),
                        _ => return,
                    }
                    this.show_controls();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, _, _, cx| {
                let was_hidden = !this.controls_visible;
                this.show_controls();
                if was_hidden {
                    cx.notify();
                }
            }))
            // A drag anywhere on the picture moves the window.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(measure)
            .when_some(self.current_frame.clone(), |el, frame| {
                el.child(
                    div().absolute().inset_0().child(
                        surface(frame.buffer())
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    ),
                )
            })
            .when(self.controls_visible || visibility > 0., |el| {
                el.child(controls)
            })
    }

    /// Details of the paused item over the dimmed, blurred video, after the
    /// pause screen of the Jellyfin Enhanced plugin.
    fn render_pause_screen(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let item = self.playing.as_ref()?;
        let client = self.session.as_ref()?.client.clone();
        let s = &self.player_status;
        let size = window.viewport_size();
        let (vw, vh) = (f32::from(size.width), f32::from(size.height));
        let left = vw * 0.08;
        let white = rgb(0xffffff);
        let soft = rgba(0xffffffcc);

        let backdrop = item
            .backdrop_url(&client, (vw * 2.).min(1920.) as u32)
            .or_else(|| item.wide_url(&client, 1280))
            .map(|url| {
                div().absolute().inset_0().opacity(0.28).child(
                    crate::images::remote_with(url, px(0.), ObjectFit::Cover)
                        .absolute()
                        .inset_0(),
                )
            });
        let title = match item.logo_url(&client, 900) {
            Some(url) => div()
                .w(px(vw * 0.45))
                .h(px(vh * 0.2))
                .child(crate::images::remote_logo(url).size_full()),
            None => div()
                .text_size(px(44.))
                .font_weight(gpui_kit::FontWeight::BOLD)
                .text_color(white)
                .child(item.display_title()),
        };

        let mut details: Vec<String> = Vec::new();
        if let Some(year) = item.production_year {
            details.push(year.to_string());
        }
        if let Some(rating) = &item.official_rating {
            details.push(rating.clone());
        }
        if let Some(secs) = item.runtime_secs() {
            details.push(crate::jellyfin::format_runtime(secs));
        }

        let fraction = if s.duration > 0. {
            (s.position / s.duration).clamp(0., 1.) as f32
        } else {
            0.
        };
        let remaining = (s.duration - s.position).max(0.);
        let progress = div()
            .absolute()
            .left(px(left))
            .top(px(vh * 0.85))
            .w(px(vw * 0.48))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .h(px(6.))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(rgba(0xffffff2e))
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .w(gpui_kit::relative(fraction))
                            .bg(rgba(0xffffffe6)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(15.))
                    .text_color(soft)
                    .child(format!(
                        "{} / {}",
                        format_duration(s.position as i64),
                        format_duration(s.duration as i64)
                    ))
                    .child(format!("{:.0}% watched", fraction * 100.))
                    .child(format!(
                        "Ends at {}",
                        crate::macos::ends_at((remaining / self.speed.max(0.25) as f64) as i64)
                    )),
            );

        Some(
            div()
                .id("player.pause-screen")
                .absolute()
                .inset_0()
                .child(glass(px(0.), rgba(0x000000c7)))
                .children(backdrop)
                .child(div().absolute().left(px(left)).top(px(vh * 0.2)).child(title))
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(vh * 0.45))
                        .flex()
                        .gap(px(30.))
                        .text_size(px(18.))
                        .text_color(soft)
                        .children(details.into_iter().map(|text| div().child(text))),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(vh * 0.55))
                        .max_w(px(vw * 0.5))
                        .max_h(px(vh * 0.25))
                        .overflow_hidden()
                        .text_size(px(19.))
                        .line_height(gpui_kit::relative(1.6))
                        .text_color(white)
                        .child(item.overview.clone().unwrap_or_default()),
                )
                .child(progress)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        // A click resumes playback, as on the web.
                        window.focus(&this.player_focus, cx);
                        this.request_toggle_pause(cx);
                        this.show_controls();
                        cx.stop_propagation();
                        cx.notify();
                    }),
                ),
        )
    }
}

/// Paints one frame out of a sheet of preview images.
fn preview_image(
    frame: PreviewFrame,
    last: std::rc::Rc<std::cell::RefCell<Option<std::sync::Arc<gpui_kit::RenderImage>>>>,
) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<gpui_kit::Pixels>, _, window: &mut Window, cx: &mut gpui_kit::App| {
            let tile = crate::images::tile(
                &frame.url,
                (frame.columns, frame.rows),
                (frame.column, frame.row),
                cx,
            );
            // While the sheet of this frame is not decoded, the frame shown
            // before stays up; an empty box would flicker during a drag.
            let image = match tile {
                Some(image) => {
                    *last.borrow_mut() = Some(image.clone());
                    image
                }
                None => match last.borrow().clone() {
                    Some(shown) => shown,
                    None => return,
                },
            };
            let _ = window.paint_image(bounds, bounds, Corners::all(px(9.)), image, 0, false);
        },
    )
    .size_full()
}
