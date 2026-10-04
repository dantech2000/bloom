// SPDX-License-Identifier: AGPL-3.0-or-later
//! The panel of "Play on", the chip of the top bar while a target is set,
//! and the entries of the card menu. Modelled on the SyncPlay panel.
//!
//! With no target the panel lists what can play: this device, the Jellyfin
//! sessions of the user, the cast devices on the network, and a row for
//! AirPlay that hosts the system route picker (an AppKit view the frame
//! places over the row's button; see `prepare_cast`). With a target it
//! shows the remote control of it, the same for every kind, with the
//! controls the kind lacks left out.

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled, Window, canvas,
    div, prelude::FluentBuilder as _, px, rgba,
};

use super::target::{Kind, Target};
use crate::{
    app::Bloom,
    icons::{Filled, filled},
    jellyfin::{Item, format_duration},
    ui::{glass::glass, menu::MenuItem, scroll_area::ScrollArea, theme::UiTheme, tip::tip},
    views::cards::icon,
};

/// Width of the panel.
const PANEL_W: f32 = 420.;
/// The box of the AirPlay row where the system picker sits.
const PICKER_SIZE: f32 = 36.;

impl Bloom {
    /// Entries of the card menu while a Jellyfin session is the target:
    /// the item goes to the queue of the target. The other kinds have no
    /// queue of ours.
    pub fn cast_menu_items(&self, item: &Item, cx: &mut Context<Self>) -> Vec<MenuItem> {
        if self.cast.kind() != Kind::Jellyfin || !item.is_playable() {
            return Vec::new();
        }
        let this = cx.weak_entity();
        let name = self.cast.device_name();
        [
            ("card.menu.cast-next", format!("Play next on {name}"), true),
            ("card.menu.cast-queue", format!("Add to the queue of {name}"), false),
        ]
        .into_iter()
        .map(|(id, label, next)| {
            let (handle, item_id) = (this.clone(), item.id.clone());
            MenuItem::new(id, label).on_click(move |_, _, cx| {
                handle
                    .update(cx, |this, cx| this.cast_enqueue(item_id.clone(), next, cx))
                    .ok();
            })
        })
        .collect()
    }

    /// A small label at the left of the top bar while a target is set:
    /// what the device plays and where it is. A click opens the panel.
    pub fn cast_topbar_chip(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !self.cast.active() {
            return None;
        }
        let view = self.target_view();
        let t = UiTheme::read(cx).clone();
        let text = match &view.title {
            Some(title) => format!("{title} · {}", format_duration(view.position as i64)),
            None if !view.connected => "Connecting…".to_string(),
            None => "Nothing plays".to_string(),
        };
        let tooltip = format!("Playing on {}; click for its controls", view.name);
        let paused = view.paused;
        Some(
            div()
                .id("top.cast-chip")
                .h(px(28.))
                .pl(px(10.))
                .pr(px(4.))
                .ml(px(4.))
                .max_w(px(260.))
                .rounded_full()
                .bg(rgba(0xffffff26))
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .text_size(px(12.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(t.colors.foreground)
                .tooltip(tip(tooltip))
                .child(kind_glyph(self.cast.kind(), 16., t.colors.foreground))
                .child(div().truncate().child(text))
                .when(view.loaded, |el| {
                    el.child(
                        div()
                            .id("top.cast-pause")
                            .size(px(22.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(rgba(0xffffff29)))
                            .tooltip(tip(if paused { "Resume" } else { "Pause" }))
                            .child(filled(
                                if paused { Filled::Play } else { Filled::Pause },
                                14.,
                                t.colors.foreground,
                            ))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.target_set_paused(!paused, cx);
                            })),
                    )
                })
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.toggle_cast_panel(window, cx)
                })),
        )
    }

    /// The panel. `top` places it under the top bar; without it the panel
    /// sits over the controls of the player.
    pub fn render_cast_panel(&self, top: Option<f32>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.cast.panel_open {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let fg = t.colors.foreground;
        let soft = rgba(0xf5f5f7b3);

        fn swallow(_: &mut Bloom, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Bloom>) {
            cx.stop_propagation();
        }
        let row = |id: SharedString| {
            div()
                .id(id)
                .min_h(px(44.))
                .px(px(12.))
                .py(px(6.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .gap(px(12.))
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xffffff1f)))
        };
        let label = |title: String, detail: String| {
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
                        .text_color(fg)
                        .child(title),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(div().truncate().text_size(px(12.)).text_color(soft).child(detail))
                })
        };
        let heading = |text: String| {
            div()
                .px(px(12.))
                .pt(px(6.))
                .text_size(px(12.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(soft)
                .child(text)
        };
        let note = |text: String| {
            div().px(px(12.)).py(px(8.)).text_size(px(14.)).text_color(soft).child(text)
        };
        // A round button of the transport row.
        let button = |id: &'static str, glyph: gpui_kit::Svg, label: &'static str| {
            div()
                .id(id)
                .size(px(40.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xffffff1f)))
                .tooltip(tip(label))
                .child(glyph)
        };
        // A bar with a filled part; a click on it gives a fraction.
        let bar = |id: &'static str,
                   fraction: f32,
                   bounds: std::rc::Rc<std::cell::Cell<gpui_kit::Bounds<gpui_kit::Pixels>>>,
                   on_pick: Box<dyn Fn(&mut Bloom, f32, &mut Context<Bloom>)>| {
            let measure = bounds.clone();
            div()
                .id(id)
                .relative()
                .flex_1()
                .h(px(20.))
                .flex()
                .items_center()
                .cursor_pointer()
                .child(canvas(move |painted, _, _| measure.set(painted), |_, _, _, _| {}).absolute().size_full())
                .child(
                    div()
                        .w_full()
                        .h(px(4.))
                        .rounded_full()
                        .bg(rgba(0xffffff33))
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(fg)
                                .w(gpui_kit::relative(fraction.clamp(0., 1.))),
                        ),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let painted = bounds.get();
                        let width = f32::from(painted.size.width);
                        if width <= 0. {
                            return;
                        }
                        let fraction = (f32::from(event.position.x - painted.origin.x) / width).clamp(0., 1.);
                        on_pick(this, fraction, cx);
                    }),
                )
        };

        let mut body = div().flex().flex_col().gap(px(2.));
        let title = if self.cast.target.is_local() {
            // ----- the list of what can play -----
            body = body.child(heading("This device".into()));
            body = body.child(
                row("cast.here".into())
                    .bg(rgba(0xffffff14))
                    .child(icon(LucideIcon::Laptop, 20., fg))
                    .child(label("This device".into(), "Plays here".into()))
                    .child(icon(LucideIcon::Check, 16., fg)),
            );

            body = body.child(heading("Jellyfin devices".into()));
            if self.cast.sessions.is_empty() {
                body = body.child(note(if self.cast.loading {
                    "Looking for devices…".into()
                } else {
                    "No other device of yours can be controlled. A device shows up here \
                     when it is signed in with this account and allows remote control."
                        .into()
                }));
            }
            for session in &self.cast.sessions {
                let id = session.id.clone();
                let detail = match session.now_playing_title() {
                    Some(title) => format!("{} · {title}", session.client),
                    None => format!("{} · nothing plays", session.client),
                };
                body = body.child(
                    row(SharedString::from(format!("cast.to.{}", session.id)))
                        .child(filled(Filled::Cast, 20., fg))
                        .child(label(session.device_name.clone(), detail))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let _ = this.cast_to(&id, cx);
                        })),
                );
            }

            // Cast devices: the ones found, or "Searching…" for a moment
            // after the search starts, or nothing.
            let devices = self.chromecast_devices();
            if !devices.is_empty() {
                body = body.child(heading("Chromecast".into()));
                for device in devices {
                    let chosen = device.clone();
                    body = body.child(
                        row(SharedString::from(format!("cast.cc.{}", device.id)))
                            .child(icon(LucideIcon::Tv, 20., fg))
                            .child(label(device.name.clone(), device.model.clone()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.select_target(Target::Chromecast(chosen.clone()), cx);
                            })),
                    );
                }
            } else if self.cast.searching() {
                body = body.child(heading("Chromecast".into()));
                body = body.child(note("Searching…".into()));
            }

            // AirPlay: the system picker chooses the route; macOS gives no
            // call for it. The button of the picker sits over the box at
            // the right of the row.
            let routes = self.airplay.routes();
            if !routes.is_empty() {
                let measure = self.cast.picker_bounds.clone();
                body = body.child(heading("AirPlay".into()));
                body = body.child(
                    div()
                        .id("cast.airplay")
                        .min_h(px(44.))
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .tooltip(tip("The button at the right opens the AirPlay menu of macOS"))
                        .child(icon(LucideIcon::Airplay, 20., fg))
                        .child(label("Choose an Apple TV or AirPlay device".into(), routes.join(", ")))
                        .child(
                            div()
                                .id("cast.airplay.picker")
                                .relative()
                                .size(px(PICKER_SIZE))
                                .rounded_full()
                                .bg(rgba(0xffffff1f))
                                .child(
                                    canvas(
                                        move |painted, window, _| {
                                            // The picker follows at the next frame.
                                            if measure.replace(painted) != painted {
                                                window.request_animation_frame();
                                            }
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                ),
                        ),
                );
            }
            "Play on".to_string()
        } else {
            // ----- the remote control of the target -----
            let view = self.target_view();
            let kind = self.cast.kind();
            // The header has the name; the kind of device goes under it.
            body = body.child(div().px(px(12.)).text_size(px(12.)).text_color(soft).child(view.detail.clone()));
            if !view.connected {
                body = body.child(note(match &view.error {
                    Some(error) => format!("Not connected: {error}"),
                    None => "Connecting…".into(),
                }));
            } else if view.loaded {
                let paused = view.paused;
                let duration = view.duration;
                body = body.child(div().px(px(12.)).pt(px(4.)).child(label(
                    view.title.clone().unwrap_or_default(),
                    format!(
                        "{}{} / {}",
                        if paused { "Paused · " } else { "" },
                        format_duration(view.position as i64),
                        format_duration(duration as i64)
                    ),
                )));
                if view.can_seek && duration > 0. {
                    body = body.child(div().px(px(12.)).child(bar(
                        "cast.seek",
                        view.fraction(),
                        self.cast.seek_bounds.clone(),
                        Box::new(move |this, fraction, cx| this.target_seek(f64::from(fraction) * duration, cx)),
                    )));
                }
                let mut transport = div().px(px(12.)).flex().items_center().justify_center().gap(px(8.));
                if view.can_skip {
                    transport = transport.child(
                        button("cast.previous", icon(LucideIcon::SkipBack, 22., fg), "Previous")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.target_skip(false, cx))),
                    );
                }
                transport = transport
                    .child(
                        button(
                            "cast.pause",
                            filled(if paused { Filled::Play } else { Filled::Pause }, 28., fg),
                            if paused { "Resume" } else { "Pause" },
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.target_set_paused(!paused, cx))),
                    )
                    .child(
                        button("cast.stop", icon(LucideIcon::Square, 20., fg), "Stop")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.target_stop(cx))),
                    );
                if view.can_skip {
                    transport = transport.child(
                        button("cast.next", icon(LucideIcon::SkipForward, 22., fg), "Next")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.target_skip(true, cx))),
                    );
                }
                body = body.child(transport);
            } else {
                body = body.child(note(match kind {
                    Kind::AirPlay if !self.airplay.status().external => {
                        "Nothing plays. Pick something; it goes to the AirPlay engine.".into()
                    }
                    _ => "Nothing plays. Pick something; it plays there.".into(),
                }));
            }
            // Volume and mute, for a kind that has them.
            if let Some(volume) = view.volume.filter(|_| view.connected) {
                let muted = view.muted;
                body = body.child(
                    div()
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            button(
                                "cast.mute",
                                filled(if muted { Filled::VolumeOff } else { Filled::VolumeUp }, 22., fg),
                                if muted { "Unmute" } else { "Mute" },
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.target_toggle_mute(cx))),
                        )
                        .child(bar(
                            "cast.volume",
                            if muted { 0. } else { volume / 100. },
                            self.cast.volume_bounds.clone(),
                            Box::new(|this, fraction, cx| this.target_set_volume(fraction * 100., cx)),
                        ))
                        .child(
                            div()
                                .w(px(32.))
                                .text_size(px(12.))
                                .text_color(soft)
                                .child(format!("{volume:.0}")),
                        ),
                );
            }
            // The tracks of the item that plays, for a kind that takes a
            // stream index.
            let item = match &self.cast.target {
                Target::JellyfinSession(session) => session.now_playing_item.as_ref(),
                Target::Chromecast(_) => self.cast.cast_item.as_ref(),
                _ => None,
            };
            if let Some(item) = item.filter(|_| view.can_tracks && view.loaded) {
                let mut tracks = div().flex().flex_col().gap(px(2.));
                let mut count = 0;
                for (kind, current, off) in [
                    ("Audio", view.audio_index, false),
                    ("Subtitle", view.subtitle_index, true),
                ] {
                    let streams = item.streams(kind);
                    if streams.is_empty() {
                        continue;
                    }
                    tracks = tracks.child(heading(if off { "Subtitles" } else { "Audio" }.into()));
                    count += 1;
                    let mut choices: Vec<(i64, String)> = Vec::new();
                    if off {
                        choices.push((-1, "Off".into()));
                    }
                    choices.extend(streams.iter().map(|s| {
                        (s.index, s.display_title.clone().unwrap_or_else(|| format!("Track {}", s.index)))
                    }));
                    for (index, text) in choices {
                        count += 1;
                        let chosen = current == Some(index) || (off && index == -1 && current.is_none());
                        tracks = tracks.child(
                            row(SharedString::from(format!("cast.track.{kind}.{index}")))
                                .min_h(px(38.))
                                .child(div().w(px(16.)).child(if chosen {
                                    icon(LucideIcon::Check, 16., fg)
                                } else {
                                    icon(LucideIcon::Check, 16., rgba(0x00000000))
                                }))
                                .child(label(text, String::new()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.target_set_track(kind, index, cx)
                                })),
                        );
                    }
                }
                if count > 0 {
                    let height = (count as f32 * 40.).min(200.);
                    body = body.child(
                        div().h(px(height)).child(ScrollArea::new("cast.tracks").size_full().child(tracks)),
                    );
                }
            }
            if let Some(error) = view.error.as_ref().filter(|_| view.connected) {
                body = body.child(note(error.clone()));
            }
            body = body.child(heading("This device".into()));
            let detail = match kind {
                Kind::Jellyfin => "Leaves the other device as it is.",
                Kind::Chromecast => "Goes on here from where the device was.",
                Kind::AirPlay => "Goes on here from where the receiver was.",
                Kind::Local => "",
            };
            body = body.child(
                row("cast.off".into())
                    .child(icon(LucideIcon::Laptop, 20., fg))
                    .child(label("Play here instead".into(), detail.into()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.cast_play_here(window, cx))),
            );
            view.name
        };

        let panel = div()
            .absolute()
            .right(px(24.))
            .w(px(PANEL_W.min(self.viewport_w - 48.)))
            .rounded(px(24.))
            .border_1()
            .border_color(rgba(0xf5f5f733))
            .on_mouse_down(MouseButton::Left, cx.listener(swallow))
            // A click anywhere else closes the panel, as it closes a menu.
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                if std::mem::take(&mut this.cast.panel_open) {
                    this.cast.panel_closed = Some(std::time::Instant::now());
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
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kind_glyph(self.cast.kind(), 20., fg))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(17.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child(title),
                    ),
            )
            .child(body);
        Some(match top {
            Some(top) => panel.top(px(top)),
            None => panel.bottom(px(134.)),
        })
    }
}

/// The glyph of a kind of target, in the one ink.
fn kind_glyph(kind: Kind, size: f32, color: gpui_kit::Rgba) -> gpui_kit::Svg {
    match kind {
        Kind::Local | Kind::Jellyfin => filled(Filled::Cast, size, color),
        Kind::Chromecast => icon(LucideIcon::Tv, size, color),
        Kind::AirPlay => icon(LucideIcon::Airplay, size, color),
    }
}
