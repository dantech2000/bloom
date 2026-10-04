// SPDX-License-Identifier: AGPL-3.0-or-later
//! The dialog of the metadata manager and its four tabs.

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, KeyDownEvent,
    MouseButton, ObjectFit, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Window,
    base::input::{InputEditorStyle, Textarea},
    div,
    prelude::FluentBuilder as _,
    px, rgb, rgba,
};
use serde_json::Value;

use super::{
    Browse, DETAIL_FIELDS, FIELDS, FIND_IMDB, FIND_NAME, FIND_TMDB, FIND_YEAR, IMAGE_TYPES,
    ImageInfo, LOCKABLE, Manager, RefreshMode, RemoteImage, TAGLINE, Tab,
};
use crate::{
    admin::{ButtonKind, badge, button},
    app::Bloom,
    jellyfin::Client,
    ui::{glass::glass, input::Input, scroll_area::ScrollArea, theme::UiTheme},
    views::cards::icon,
};
use crate::ui::tip::tip;

/// Space between the window edge and the dialog.
const MARGIN: f32 = 44.;
/// Padding at the left and right of the dialog content.
const PAD: f32 = 28.;
/// Space between two fields.
const GAP: f32 = 14.;
/// Width of an image tile of the Images tab.
const TILE_W: f32 = 196.;

impl Bloom {
    /// The metadata manager over the page.
    pub fn render_metadata(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let manager = self.metadata.open.as_ref()?;
        let client = self.session.as_ref()?.client.clone();
        let t = UiTheme::read(cx).clone();
        let card_w = (self.viewport_w - MARGIN * 2.).clamp(520., 1040.);
        let card_h = (self.viewport_h - MARGIN * 2.).clamp(420., 820.);
        let content_w = card_w - PAD * 2.;

        let poster = manager
            .images
            .as_ref()
            .and_then(|images| images.iter().find(|i| i.image_type == "Primary"))
            .map(|image| image_url(&client, &manager.id, image, 120));
        let header = div()
            .px(px(PAD))
            .pt(px(22.))
            .flex()
            .items_center()
            .gap(px(14.))
            .child(
                div()
                    .relative()
                    .w(px(40.))
                    .h(px(60.))
                    .flex_shrink_0()
                    .rounded(px(8.))
                    .bg(rgba(0xffffff14))
                    .when_some(poster, |el, url| {
                        el.child(
                            crate::images::remote_with(url, px(8.), ObjectFit::Cover)
                                .absolute()
                                .inset_0(),
                        )
                    }),
            )
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
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(t.colors.foreground.opacity(0.55))
                            .child("Metadata manager"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(22.))
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(t.colors.foreground)
                                    .child(manager.name.clone()),
                            )
                            .when(!manager.kind.is_empty(), |el| {
                                el.child(badge(manager.kind.clone(), rgba(0xffffff29)))
                            })
                            .when(manager.lock, |el| {
                                el.child(badge("Locked", rgb(0xb7791f).into()))
                            }),
                    ),
            )
            .child(
                div()
                    .id("meta.close").tooltip(tip("Close (Esc)"))
                    .size(px(36.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff1f)))
                    .child(icon(LucideIcon::X, 18., t.colors.foreground))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.close_metadata(window, cx)
                    })),
            );

        let mut tabs = div()
            .mx(px(PAD))
            .mt(px(16.))
            .p(px(4.))
            .rounded(px(14.))
            .bg(rgba(0x00000047))
            .flex()
            .gap(px(4.));
        for tab in Tab::ALL {
            let selected = tab == manager.tab;
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("meta.tab.{tab:?}")))
                    .flex_1()
                    .h(px(34.))
                    .rounded(px(10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(14.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(if selected {
                        t.colors.primary_foreground
                    } else {
                        t.colors.foreground.opacity(0.8)
                    })
                    .when(selected, |el| el.bg(t.colors.primary))
                    .when(!selected, |el| el.hover(|s| s.bg(rgba(0xffffff14))))
                    .child(tab.label())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_metadata_tab(tab, cx)
                    })),
            );
        }

        let note = |text: String| {
            div()
                .py(px(48.))
                .flex()
                .justify_center()
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child(text)
        };
        let body = match (&manager.item, &manager.error) {
            (None, Some(error)) => note(format!("Could not load the item: {error}")),
            (None, None) => note("Loading…".to_string()),
            (Some(item), _) => match manager.tab {
                Tab::Details => self.metadata_details(manager, item, content_w, window, cx),
                Tab::Images => self.metadata_images(manager, &client, content_w, cx),
                Tab::Identify => self.metadata_identify(manager, content_w, cx),
                Tab::Refresh => self.metadata_refresh(manager, cx),
            },
        };

        let cancel = |label: &'static str, cx: &mut Context<Self>| {
            button("meta.cancel", label, ButtonKind::Plain, cx).on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| this.close_metadata(window, cx),
            ))
        };
        let footer_note = match manager.tab {
            Tab::Details => "A save writes only the fields you changed.",
            Tab::Images => "Images are stored on the server for every user.",
            Tab::Identify => "A match replaces the metadata of the item.",
            Tab::Refresh => "The refresh runs on the server in the background.",
        };
        let mut footer = div()
            .px(px(PAD))
            .py(px(16.))
            .border_t_1()
            .border_color(rgba(0xf5f5f71f))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(t.colors.muted_foreground)
                    .child(if manager.busy { "Saving…" } else { footer_note }),
            );
        let ready = manager.item.is_some() && !manager.busy;
        footer = match manager.tab {
            Tab::Details => footer.child(cancel("Cancel", cx)).child(
                button("meta.save", "Save", ButtonKind::Primary, cx)
                    .when(!ready, |el| el.opacity(0.5))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if ready {
                            this.save_metadata(cx)
                        }
                    })),
            ),
            Tab::Refresh => footer.child(cancel("Cancel", cx)).child(
                button("meta.refresh", "Refresh", ButtonKind::Primary, cx)
                    .when(!ready, |el| el.opacity(0.5))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if ready {
                            this.refresh_metadata(window, cx)
                        }
                    })),
            ),
            _ => footer.child(cancel("Close", cx)),
        };

        let card = div()
            .id("meta.card")
            .relative()
            .w(px(card_w))
            .h(px(card_h))
            .rounded(px(24.))
            .border_1()
            .border_color(rgba(0xf5f5f733))
            .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(tabs)
                    .child(
                        div().flex_1().min_h_0().mt(px(12.)).child(
                            ScrollArea::new("meta.scroll")
                                .track(&self.metadata.scroll)
                                .size_full()
                                .child(div().px(px(PAD)).pb(px(24.)).child(body)),
                        ),
                    )
                    .child(footer),
            )
            .children(self.metadata_confirm(manager, cx));

        Some(
            div()
                .id("meta.overlay")
                .absolute()
                .inset_0()
                .occlude()
                .track_focus(&self.metadata.focus)
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x000000a6))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    let keystroke = &event.keystroke;
                    match keystroke.key.as_str() {
                        "escape" => {
                            let waits = this
                                .metadata
                                .open
                                .as_mut()
                                .and_then(|manager| manager.confirm.take());
                            match waits {
                                Some(_) => cx.notify(),
                                None => this.close_metadata(window, cx),
                            }
                        }
                        "tab" => this.next_metadata_field(keystroke.modifiers.shift, window, cx),
                        "s" if keystroke.modifiers.platform => {
                            let details = this
                                .metadata
                                .open
                                .as_ref()
                                .is_some_and(|m| m.tab == Tab::Details && !m.busy);
                            if details {
                                this.save_metadata(cx);
                            }
                        }
                        _ => {}
                    }
                    // The keys of the dialog are not shortcuts of the page.
                    cx.stop_propagation();
                }))
                // A click beside the dialog closes it.
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.close_metadata(window, cx)
                }))
                .child(card),
        )
    }

    /// The question of the dialog, over its content.
    fn metadata_confirm(&self, manager: &Manager, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let confirm = manager.confirm.clone()?;
        let t = UiTheme::read(cx).clone();
        let run = confirm.run.clone();
        Some(
            div()
                .id("meta.confirm")
                .absolute()
                .inset_0()
                .occlude()
                .rounded(px(24.))
                .bg(rgba(0x000000a6))
                .flex()
                .items_center()
                .justify_center()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(manager) = &mut this.metadata.open {
                        manager.confirm = None;
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(
                    div()
                        .id("meta.confirm.card")
                        .w(px(420.))
                        .rounded(px(20.))
                        .border_1()
                        .border_color(rgba(0xf5f5f733))
                        .bg(rgb(0x2a2a2a))
                        .p(px(22.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .text_size(px(19.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child(confirm.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(14.))
                                .text_color(t.colors.foreground.opacity(0.8))
                                .child(confirm.message.clone()),
                        )
                        .child(
                            div()
                                .mt(px(8.))
                                .flex()
                                .justify_end()
                                .gap(px(10.))
                                .child(
                                    button("meta.confirm.cancel", "Cancel", ButtonKind::Plain, cx)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            if let Some(manager) = &mut this.metadata.open {
                                                manager.confirm = None;
                                            }
                                            cx.stop_propagation();
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    button(
                                        "meta.confirm.run",
                                        confirm.action.clone(),
                                        if confirm.danger {
                                            ButtonKind::Danger
                                        } else {
                                            ButtonKind::Primary
                                        },
                                        cx,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            run(this, cx);
                                        },
                                    )),
                                ),
                        ),
                ),
        )
    }

    /// One labelled text field of `State::inputs`.
    fn metadata_field(&self, index: usize, content_w: f32, cx: &Context<Self>) -> Div {
        let spec = &FIELDS[index];
        labelled(
            spec.label,
            ((content_w + GAP) * spec.span - GAP - 1.).floor(),
            cx,
        )
        .child(
            Input::new(&self.metadata.inputs[index])
                .aria_label(spec.label)
                .w_full(),
        )
    }

    fn metadata_details(
        &self,
        manager: &Manager,
        item: &Value,
        content_w: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = UiTheme::read(cx).clone();
        let colors = t.colors;
        let row = || div().flex().flex_wrap().gap(px(GAP));

        // The overview: a field of several lines in the look of the others.
        let overview = self.metadata.overview.clone();
        overview.update(cx, |state, _| {
            state.set_editor_style(InputEditorStyle {
                foreground: colors.foreground.into(),
                muted_foreground: colors.muted_foreground.into(),
                background: colors.background.into(),
                border: colors.input.into(),
                selection: colors.ring.opacity(0.3).into(),
                caret: colors.foreground.into(),
                ..Default::default()
            });
        });
        let focused = {
            use gpui_kit::Focusable as _;
            overview.read(cx).focus_handle(cx).is_focused(window)
        };
        let click = overview.clone();
        let overview = labelled("Overview", content_w, cx).child(
            div()
                .w_full()
                .h(px(138.))
                .px(t.space(2.5))
                .py(px(8.))
                .rounded(t.radius.lg)
                .border_1()
                .border_color(if focused { colors.ring } else { colors.input })
                .bg(colors.input.opacity(0.3))
                .text_size(t.text(14.))
                .line_height(t.text(20.))
                .text_color(colors.foreground)
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    click.update(cx, |state, cx| state.focus(window, cx));
                })
                .child(Textarea::new(&overview)),
        );

        let mut general = row();
        for index in 0..=TAGLINE {
            general = general.child(self.metadata_field(index, content_w, cx));
        }
        general = general.child(overview);

        let mut dates = row();
        for index in TAGLINE + 1..super::GENRES {
            dates = dates.child(self.metadata_field(index, content_w, cx));
        }
        if manager.kind == "Series" {
            let mut status = div().flex().gap(px(8.));
            for name in ["Continuing", "Ended", "Unreleased"] {
                status = status.child(
                    chip(format!("meta.status.{name}"), name, manager.status == name, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(manager) = &mut this.metadata.open {
                                manager.status = name.to_string();
                            }
                            cx.notify();
                        })),
                );
            }
            dates = dates.child(
                labelled("Status", ((content_w + GAP) * 0.5 - GAP).floor(), cx).child(status),
            );
        }

        let mut classification = row();
        for index in super::GENRES..super::IMDB {
            classification = classification.child(self.metadata_field(index, content_w, cx));
        }
        let mut ids = row();
        for index in super::IMDB..DETAIL_FIELDS {
            ids = ids.child(self.metadata_field(index, content_w, cx));
        }

        let people: Vec<(String, String)> = item["People"]
            .as_array()
            .map(|list| {
                list.iter()
                    .take(40)
                    .map(|p| {
                        let name = p["Name"].as_str().unwrap_or_default().to_string();
                        let role = p["Role"]
                            .as_str()
                            .filter(|r| !r.is_empty())
                            .or(p["Type"].as_str())
                            .unwrap_or_default()
                            .to_string();
                        (name, role)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let total_people = item["People"].as_array().map_or(0, Vec::len);
        let mut cast = div().flex().flex_wrap().gap(px(8.));
        for (name, role) in &people {
            cast = cast.child(
                div()
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(10.))
                    .bg(rgba(0xffffff12))
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .child(div().text_color(colors.foreground).child(name.clone()))
                    .when(!role.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(12.))
                                .text_color(colors.muted_foreground)
                                .child(role.clone()),
                        )
                    }),
            );
        }
        if total_people > people.len() {
            cast = cast.child(
                div()
                    .py(px(5.))
                    .text_size(px(13.))
                    .text_color(colors.muted_foreground)
                    .child(format!("and {} more", total_people - people.len())),
            );
        }

        let mut locks = div().flex().flex_wrap().gap(px(8.));
        for (key, label) in LOCKABLE {
            let on = manager.locked.iter().any(|k| k == key);
            locks = locks.child(
                chip(format!("meta.lock.{key}"), label, on, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        if let Some(manager) = &mut this.metadata.open {
                            match manager.locked.iter().position(|k| k == key) {
                                Some(at) => {
                                    manager.locked.remove(at);
                                }
                                None => manager.locked.push(key.to_string()),
                            }
                        }
                        cx.notify();
                    },
                )),
            );
        }
        let lock_all = check(
            "meta.lock.all",
            "Lock this item",
            "A refresh does not change any field of a locked item.",
            manager.lock,
            true,
            cx,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            if let Some(manager) = &mut this.metadata.open {
                manager.lock = !manager.lock;
            }
            cx.notify();
        }));

        div()
            .flex()
            .flex_col()
            .child(heading("General", cx))
            .child(general)
            .child(heading("Dates and ratings", cx))
            .child(dates)
            .child(heading("Classification", cx))
            .child(classification)
            .child(heading("External ids", cx))
            .child(ids)
            .when(!people.is_empty(), |el| {
                el.child(heading("People", cx)).child(cast)
            })
            .child(heading("Locks", cx))
            .child(lock_all)
            .child(
                div()
                    .mt(px(12.))
                    .mb(px(8.))
                    .text_size(px(13.))
                    .text_color(colors.muted_foreground)
                    .child("Fields a refresh must keep:"),
            )
            .child(locks)
    }

    fn metadata_images(
        &self,
        manager: &Manager,
        client: &Client,
        content_w: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = UiTheme::read(cx).clone();
        let columns = (((content_w + GAP) / (TILE_W + GAP)).floor() as usize).max(2);
        let tile_w = ((content_w - GAP * (columns - 1) as f32) / columns as f32).floor();
        let images = manager.images.clone().unwrap_or_default();

        let mut current = div().flex().flex_wrap().gap(px(GAP));
        for image in &images {
            let target = image.clone();
            let label = match image.image_index {
                Some(index) if image.image_type == "Backdrop" => {
                    format!("{} {}", image.image_type, index + 1)
                }
                _ => image.image_type.clone(),
            };
            let mut facts = Vec::new();
            if let (Some(w), Some(h)) = (image.width, image.height) {
                facts.push(format!("{w} × {h}"));
            }
            if let Some(size) = image.size {
                facts.push(file_size(size));
            }
            current = current.child(
                tile(
                    image_url(client, &manager.id, image, 480),
                    tile_w,
                    label,
                    facts.join(" · "),
                    cx,
                )
                .child(
                    small_button(
                        format!(
                            "meta.image.delete.{}.{}",
                            image.image_type,
                            image.image_index.unwrap_or(0)
                        ),
                        "Delete",
                        true,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.ask_delete_image(&target, cx)
                    })),
                ),
            );
        }
        if images.is_empty() {
            current = current.child(
                div()
                    .py(px(20.))
                    .text_size(px(14.))
                    .text_color(t.colors.muted_foreground)
                    .child("This item has no images."),
            );
        }

        let browsing = manager.browse.as_ref().map(|b| b.kind.as_str());
        let mut types = div().flex().flex_wrap().gap(px(8.));
        for kind in IMAGE_TYPES {
            types = types.child(
                chip(format!("meta.browse.{kind}"), kind, browsing == Some(kind), cx).on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.browse_images(kind, cx)),
                ),
            );
        }

        let found = match &manager.browse {
            None => div()
                .py(px(16.))
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child("Pick an image type to see what the providers offer."),
            Some(Browse { loading: true, .. }) => div()
                .py(px(16.))
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child("Asking the providers…"),
            Some(Browse {
                error: Some(error), ..
            }) => div()
                .py(px(16.))
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child(format!("Could not load the images: {error}")),
            Some(browse) if browse.images.is_empty() => div()
                .py(px(16.))
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child(format!("No provider has a {} image.", browse.kind.to_lowercase())),
            Some(browse) => {
                let mut grid = div().flex().flex_wrap().gap(px(GAP));
                for (n, image) in browse.images.iter().enumerate() {
                    grid = grid.child(self.remote_tile(n, image, tile_w, cx));
                }
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(t.colors.muted_foreground)
                            .child(format!(
                                "{} of {} {} images",
                                browse.images.len(),
                                browse.total.max(browse.images.len()),
                                browse.kind.to_lowercase()
                            )),
                    )
                    .child(grid)
            }
        };

        div()
            .flex()
            .flex_col()
            .child(heading("Images of the item", cx))
            .child(current)
            .child(heading("Find images", cx))
            .child(types)
            .child(div().mt(px(12.)).child(found))
    }

    /// One image a provider offers, with its facts and the Use button.
    fn remote_tile(&self, n: usize, image: &RemoteImage, tile_w: f32, cx: &mut Context<Self>) -> Div {
        let target = image.clone();
        let mut facts = Vec::new();
        if let (Some(w), Some(h)) = (image.width, image.height) {
            facts.push(format!("{w} × {h}"));
        }
        if let Some(language) = image.language.as_deref().filter(|l| !l.is_empty()) {
            facts.push(language.to_uppercase());
        }
        if let Some(rating) = image.community_rating.filter(|r| *r > 0.) {
            facts.push(format!("★ {rating:.1}"));
        }
        tile(
            image.thumbnail_url.clone().unwrap_or_else(|| image.url.clone()),
            tile_w,
            image.provider_name.clone().unwrap_or_else(|| "Provider".to_string()),
            facts.join(" · "),
            cx,
        )
        .child(
            small_button(format!("meta.image.use.{n}"), "Use", false, cx).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| this.ask_use_image(&target, cx),
            )),
        )
    }

    fn metadata_identify(&self, manager: &Manager, content_w: f32, cx: &mut Context<Self>) -> Div {
        let t = UiTheme::read(cx).clone();
        let muted = |text: String| {
            div()
                .py(px(16.))
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child(text)
        };
        if !manager.can_identify() {
            return muted(format!(
                "The providers cannot look up an item of the type {}. Identify its series or \
                 edit the fields on the Details tab.",
                manager.kind
            ));
        }
        let mut form = div().flex().flex_wrap().gap(px(GAP));
        for index in [FIND_NAME, FIND_YEAR, FIND_IMDB, FIND_TMDB] {
            form = form.child(self.metadata_field(index, content_w, cx));
        }
        let searching = manager.searching;
        let actions = div()
            .mt(px(14.))
            .flex()
            .items_center()
            .gap(px(16.))
            .child(
                button(
                    "meta.search",
                    if searching { "Searching…" } else { "Search" },
                    ButtonKind::Primary,
                    cx,
                )
                .when(searching, |el| el.opacity(0.5))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.metadata_search(cx))),
            )
            .child(
                check(
                    "meta.apply.images",
                    "Replace the images too",
                    "",
                    manager.apply_images,
                    true,
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(manager) = &mut this.metadata.open {
                        manager.apply_images = !manager.apply_images;
                    }
                    cx.notify();
                })),
            );

        let columns = (((content_w + GAP) / (150. + GAP)).floor() as usize).max(2);
        let tile_w = ((content_w - GAP * (columns - 1) as f32) / columns as f32).floor();
        let results = match (&manager.search_error, &manager.results) {
            (Some(error), _) => muted(error.clone()),
            (None, None) => muted(
                "Search by name, or give an id for an exact match. Enter starts the search."
                    .to_string(),
            ),
            (None, Some(results)) if results.is_empty() => {
                muted("No provider found a match.".to_string())
            }
            (None, Some(results)) => {
                let mut grid = div().flex().flex_wrap().gap(px(GAP));
                for (n, result) in results.iter().enumerate() {
                    let target = result.clone();
                    let name = result["Name"].as_str().unwrap_or("Unknown").to_string();
                    let mut facts = Vec::new();
                    if let Some(year) = result["ProductionYear"].as_u64() {
                        facts.push(year.to_string());
                    }
                    if let Some(provider) = result["SearchProviderName"].as_str() {
                        facts.push(provider.to_string());
                    }
                    let poster_h = (tile_w * 1.5).floor();
                    grid = grid.child(
                        div()
                            .w(px(tile_w))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .relative()
                                    .w(px(tile_w))
                                    .h(px(poster_h))
                                    .rounded(px(12.))
                                    .bg(rgba(0xffffff12))
                                    .when_some(
                                        result["ImageUrl"].as_str().map(str::to_string),
                                        |el, url| {
                                            el.child(
                                                crate::images::remote_with(
                                                    url,
                                                    px(12.),
                                                    ObjectFit::Cover,
                                                )
                                                .absolute()
                                                .inset_0(),
                                            )
                                        },
                                    ),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(14.))
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(t.colors.foreground)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(t.colors.muted_foreground)
                                    .child(facts.join(" · ")),
                            )
                            .child(
                                small_button(format!("meta.match.{n}"), "Use this match", false, cx)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.ask_apply_match(&target, cx)
                                    })),
                            ),
                    );
                }
                grid
            }
        };

        div()
            .flex()
            .flex_col()
            .child(heading("Look for a title", cx))
            .child(form)
            .child(actions)
            .child(heading("Matches", cx))
            .child(results)
    }

    fn metadata_refresh(&self, manager: &Manager, cx: &mut Context<Self>) -> Div {
        let t = UiTheme::read(cx).clone();
        let mut modes = div().flex().flex_col().gap(px(10.));
        for mode in RefreshMode::ALL {
            let selected = manager.refresh_mode == mode;
            modes = modes.child(
                div()
                    .id(SharedString::from(format!("meta.refresh.{mode:?}")))
                    .p(px(14.))
                    .rounded(px(14.))
                    .border_1()
                    .border_color(if selected {
                        rgba(0xf5f5f7b3)
                    } else {
                        rgba(0xf5f5f71f)
                    })
                    .bg(if selected {
                        rgba(0xffffff1a)
                    } else {
                        rgba(0xffffff0a)
                    })
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff1a)))
                    .child(
                        // Radio mark.
                        div()
                            .size(px(18.))
                            .flex_shrink_0()
                            .rounded_full()
                            .border_2()
                            .border_color(if selected {
                                t.colors.primary
                            } else {
                                t.colors.foreground.opacity(0.4)
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(selected, |el| {
                                el.child(div().size(px(8.)).rounded_full().bg(t.colors.primary))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(t.colors.foreground)
                                    .child(mode.label()),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(t.colors.muted_foreground)
                                    .child(mode.note()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(manager) = &mut this.metadata.open {
                            manager.refresh_mode = mode;
                        }
                        cx.notify();
                    })),
            );
        }
        // The server replaces images only in a refresh that asks the providers.
        let full = manager.refresh_mode != RefreshMode::Scan;
        let options = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                check(
                    "meta.refresh.images",
                    "Replace existing images",
                    "Downloads the artwork again and drops the images the item has.",
                    full && manager.replace_images,
                    full,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(manager) = &mut this.metadata.open
                        && full
                    {
                        manager.replace_images = !manager.replace_images;
                    }
                    cx.notify();
                })),
            )
            .child(
                check(
                    "meta.refresh.trickplay",
                    "Replace trickplay images",
                    "Makes the timeline previews again. This takes long for a series.",
                    full && manager.replace_trickplay,
                    full,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(manager) = &mut this.metadata.open
                        && full
                    {
                        manager.replace_trickplay = !manager.replace_trickplay;
                    }
                    cx.notify();
                })),
            );
        let scope = match manager.kind.as_str() {
            "Series" => "The refresh covers every season and episode of the series.",
            "Season" => "The refresh covers every episode of the season.",
            "BoxSet" => "The refresh covers every item of the collection.",
            _ => "The refresh covers this item.",
        };
        div()
            .flex()
            .flex_col()
            .child(heading("Refresh mode", cx))
            .child(modes)
            .child(heading("Options", cx))
            .child(options)
            .child(
                div()
                    .mt(px(18.))
                    .text_size(px(13.))
                    .text_color(t.colors.muted_foreground)
                    .child(scope),
            )
    }
}

/// Address of an image of the item, at most `width` pixels wide.
fn image_url(client: &Client, id: &str, image: &ImageInfo, width: u32) -> String {
    client.url(
        &format!(
            "/Items/{id}/Images/{}/{}",
            image.image_type,
            image.image_index.unwrap_or(0)
        ),
        &[
            ("tag", image.image_tag.clone().unwrap_or_default()),
            ("maxWidth", width.to_string()),
            ("quality", "90".to_string()),
        ],
    )
}

/// "1.2 MB" for a number of bytes.
fn file_size(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{:.0} KB", bytes as f64 / 1e3),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}

/// Heading of a group of fields.
fn heading(title: &'static str, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .mt(px(22.))
        .mb(px(12.))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground.opacity(0.6))
                .child(title),
        )
        .child(div().flex_1().h(px(1.)).bg(rgba(0xf5f5f71a)))
}

/// A column of `width` with a label; add the field as its child.
fn labelled(label: &'static str, width: f32, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground.opacity(0.7))
                .child(label),
        )
}

/// A pill that is on or off. Add `.on_click(...)`.
fn chip(
    id: impl Into<SharedString>,
    label: &'static str,
    on: bool,
    cx: &Context<Bloom>,
) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    div()
        .id(id.into())
        .h(px(32.))
        .px(px(14.))
        .rounded_full()
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(px(13.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .map(|el| {
            if on {
                el.bg(t.colors.primary).text_color(t.colors.primary_foreground)
            } else {
                el.bg(rgba(0xffffff14))
                    .text_color(t.colors.foreground.opacity(0.85))
                    .hover(|s| s.bg(rgba(0xffffff26)))
            }
        })
        .child(label)
}

/// A check box with its label and a note. Add `.on_click(...)`.
fn check(
    id: &'static str,
    label: &'static str,
    note: &'static str,
    on: bool,
    enabled: bool,
    cx: &Context<Bloom>,
) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(12.))
        .when(enabled, |el| el.cursor_pointer())
        .when(!enabled, |el| el.opacity(0.45))
        .child(
            div()
                .size(px(20.))
                .flex_shrink_0()
                .rounded(px(6.))
                .border_2()
                .flex()
                .items_center()
                .justify_center()
                .map(|el| {
                    if on {
                        el.border_color(t.colors.primary)
                            .bg(t.colors.primary)
                            .child(icon(LucideIcon::Check, 14., t.colors.primary_foreground))
                    } else {
                        el.border_color(t.colors.foreground.opacity(0.4))
                    }
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(t.colors.foreground)
                        .child(label),
                )
                .when(!note.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(13.))
                            .text_color(t.colors.muted_foreground)
                            .child(note),
                    )
                }),
        )
}

/// An image with two lines of text under it; add a button as its child.
fn tile(url: String, width: f32, title: String, facts: String, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .w(px(width))
        .p(px(8.))
        .rounded(px(14.))
        .border_1()
        .border_color(rgba(0xf5f5f71a))
        .bg(rgba(0xffffff0a))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .relative()
                .w_full()
                .h(px((width * 0.7).floor()))
                .rounded(px(8.))
                .bg(rgba(0x00000059))
                .child(
                    crate::images::remote_with(url, px(8.), ObjectFit::Contain)
                        .absolute()
                        .inset_0(),
                ),
        )
        .child(
            div()
                .truncate()
                .text_size(px(14.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child(title),
        )
        .child(
            div()
                .truncate()
                .text_size(px(12.))
                .text_color(t.colors.muted_foreground)
                .child(if facts.is_empty() { "\u{a0}".to_string() } else { facts }),
        )
}

/// A button that fills the width of a tile.
fn small_button(
    id: impl Into<SharedString>,
    label: &'static str,
    danger: bool,
    cx: &Context<Bloom>,
) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    div()
        .id(id.into())
        .h(px(30.))
        .rounded(px(10.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(rgba(0xffffff1a))
        .hover(|s| s.bg(rgba(0xffffff2e)))
        .text_size(px(13.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(if danger {
            rgb(0xff8a80).into()
        } else {
            t.colors.foreground
        })
        .child(label)
}
