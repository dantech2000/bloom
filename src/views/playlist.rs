// SPDX-License-Identifier: AGPL-3.0-or-later
//! The page of one playlist, and the dialog of the lists ("Add to playlist",
//! "Add to collection", rename, delete).

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, KeyDownEvent, ObjectFit,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgba,
};

use crate::{
    admin::{ButtonKind, button},
    app::{Bloom, Page},
    icons::{Filled, filled},
    jellyfin::format_runtime,
    lists::{Dialog, ListKind},
    ui::{
        glass::glass, input::Input, scroll_area::ScrollArea, theme::UiTheme, tip::tip,
    },
    views::cards::{empty_state, icon},
};

const ROW_H: f32 = 72.;

impl Bloom {
    pub fn render_playlist(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Playlist(data) = &self.page else {
            unreachable!()
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let t = UiTheme::read(cx).clone();
        let m = self.metrics();
        let fg = t.colors.foreground;
        let soft = rgba(0xf5f5f7c7);
        let total = data.entries.len();
        let secs: i64 = data
            .entries
            .iter()
            .filter_map(|e| e.item.runtime_secs())
            .sum();
        let summary = if data.loading && total == 0 {
            "∙".to_string()
        } else if secs > 0 {
            format!("{total} items · {}", format_runtime(secs))
        } else {
            format!("{total} items")
        };

        let tool = |id: &'static str, glyph: LucideIcon, label: &'static str| {
            div()
                .id(id)
                .tooltip(tip(label))
                .size(px(38.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xf5f5f71f)))
                .child(icon(glyph, 20., fg))
        };
        let play_all = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .text_color(t.colors.primary_foreground)
            .text_size(px(14.))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .child(
                div()
                    .id("playlist.play-all")
                    .h(px(38.))
                    .pl(px(14.))
                    .pr(px(16.))
                    .rounded_l_full()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .bg(t.colors.primary)
                    .hover(|s| s.opacity(0.88))
                    .child(filled(Filled::Play, 20., t.colors.primary_foreground))
                    .child("Play All")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.play_playlist(0, false, window, cx)
                    })),
            )
            .child(
                div()
                    .id("playlist.shuffle")
                    .tooltip(tip("Shuffle"))
                    .h(px(38.))
                    .px(px(14.))
                    .rounded_r_full()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .bg(t.colors.primary)
                    .hover(|s| s.opacity(0.88))
                    .child(icon(LucideIcon::Shuffle, 18., t.colors.primary_foreground))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.play_playlist(0, true, window, cx)
                    })),
            );

        let toolbar = div()
            .px(px(m.side))
            .h(px(54.))
            .flex()
            .items_center()
            .gap(px(18.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(22.))
                    .text_color(soft)
                    .child(data.list.name.clone()),
            )
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(soft)
                    .child(summary),
            )
            .child(div().flex_1())
            .when(total > 0, |el| el.child(play_all))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        tool("playlist.rename", LucideIcon::Pencil, "Rename playlist").on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.ask_rename_playlist(window, cx)
                            }),
                        ),
                    )
                    .child({
                        let list = data.list.clone();
                        tool("playlist.delete", LucideIcon::Trash, "Delete playlist").on_click(
                            cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.ask_delete_list(list.clone(), window, cx)
                            }),
                        )
                    }),
            );

        let rows: Vec<Stateful<Div>> = data
            .entries
            .iter()
            .enumerate()
            .map(|(n, entry)| {
                let item = &entry.item;
                let thumb = item
                    .wide_url(&client, 320)
                    .or_else(|| item.poster_url(&client, 320));
                let wide = item.wide_url(&client, 320).is_some();
                let title = if item.kind == "Episode" {
                    item.display_title()
                } else {
                    item.name.clone()
                };
                let detail = match item.kind.as_str() {
                    "Episode" => item.series_name.clone().unwrap_or_default(),
                    _ => item.year_label().unwrap_or_default(),
                };
                let runtime = item.runtime_secs().map(format_runtime).unwrap_or_default();
                let open = item.clone();
                let small = |id: String, glyph: LucideIcon, label: &'static str| {
                    div()
                        .id(SharedString::from(id))
                        .tooltip(tip(label))
                        .size(px(32.))
                        .rounded(px(8.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(rgba(0xffffff29)))
                        .child(icon(glyph, 17., soft))
                };
                div()
                    .id(SharedString::from(format!("playlist.row.{}", entry.entry_id)))
                    .h(px(ROW_H))
                    .px(px(12.))
                    .rounded(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff14)))
                    .child(
                        div()
                            .w(px(24.))
                            .text_size(px(13.))
                            .text_color(soft)
                            .child((n + 1).to_string()),
                    )
                    .child(
                        div()
                            .relative()
                            .w(px(if wide { 96. } else { 40. }))
                            .h(px(if wide { 54. } else { 60. }))
                            .flex_shrink_0()
                            .rounded(px(8.))
                            .overflow_hidden()
                            .bg(rgba(0xffffff14))
                            .when_some(thumb, |el, url| {
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
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(15.))
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(fg)
                                    .child(title),
                            )
                            .when(!detail.is_empty(), |el| {
                                el.child(
                                    div()
                                        .truncate()
                                        .text_size(px(12.))
                                        .text_color(soft)
                                        .child(detail),
                                )
                            }),
                    )
                    .child(
                        div()
                            .w(px(64.))
                            .text_size(px(13.))
                            .text_color(soft)
                            .child(runtime),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                small(format!("playlist.play.{}", entry.entry_id), LucideIcon::Play, "Play from here")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.play_playlist(n, false, window, cx)
                                    })),
                            )
                            .child(
                                small(format!("playlist.up.{}", entry.entry_id), LucideIcon::ChevronUp, "Move up")
                                    .when(n == 0, |el| el.opacity(0.3))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        if let Some(to) = crate::lists::step_target(total, n, true) {
                                            this.playlist_move(n, to, cx)
                                        }
                                    })),
                            )
                            .child(
                                small(format!("playlist.down.{}", entry.entry_id), LucideIcon::ChevronDown, "Move down")
                                    .when(n + 1 >= total, |el| el.opacity(0.3))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        if let Some(to) = crate::lists::step_target(total, n, false) {
                                            this.playlist_move(n, to, cx)
                                        }
                                    })),
                            )
                            .child(
                                small(format!("playlist.remove.{}", entry.entry_id), LucideIcon::X, "Remove from the playlist")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.playlist_remove(n, cx)
                                    })),
                            ),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.open_item(open.clone(), cx)
                    }))
            })
            .collect();

        let body = if data.loading && rows.is_empty() {
            div().px(px(m.side)).text_color(soft).child("Loading…")
        } else if rows.is_empty() {
            empty_state(
                "This playlist is empty",
                "Use Add to playlist in the menu of a title.",
                cx,
            )
        } else {
            div()
                .px(px(m.side))
                .flex()
                .flex_col()
                .gap(px(4.))
                .children(rows)
        };

        div().relative().size_full().child(
            ScrollArea::new("playlist.scroll")
                .track(&self.page_scroll)
                .size_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .pb(px(72.))
                        .child(toolbar)
                        .child(body),
                ),
        )
    }

    /// The dialog of the lists over the page.
    pub fn render_lists_dialog(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.lists.dialog.clone()?;
        let t = UiTheme::read(cx).clone();
        let soft = rgba(0xf5f5f7b3);

        let (title, message) = match &dialog {
            Dialog::Add { kind, item, .. } => (
                format!("Add to {}", kind.noun()),
                item.display_title(),
            ),
            Dialog::Rename { list } => ("Rename playlist".to_string(), list.name.clone()),
            Dialog::Confirm { title, message, .. } => (title.clone(), message.clone()),
        };
        let mut card = div()
            .id("lists.card")
            .relative()
            .w(px(440.))
            .rounded(px(24.))
            .border_1()
            .border_color(rgba(0xf5f5f733))
            .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(t.colors.foreground)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .text_color(t.colors.foreground.opacity(0.8))
                    .when(!matches!(dialog, Dialog::Confirm { .. }), |el| el.truncate())
                    .child(message),
            );

        let field = |label: &'static str| {
            div()
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
                .child(Input::new(&self.lists.input).aria_label(label).w_full())
        };
        let buttons = |action: &'static str, kind: ButtonKind, cx: &mut Context<Self>| {
            div()
                .mt(px(8.))
                .flex()
                .justify_end()
                .gap(px(10.))
                .child(
                    button("lists.cancel", "Cancel", ButtonKind::Plain, cx).on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.close_lists_dialog(window, cx),
                    )),
                )
                .child(
                    button("lists.run", action, kind, cx).on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.submit_lists_dialog(window, cx),
                    )),
                )
        };

        match &dialog {
            Dialog::Add { kind, item, lists } => {
                let kind = *kind;
                let mut rows = div().flex().flex_col().gap(px(2.));
                match lists {
                    None => {
                        rows = rows.child(
                            div().px(px(12.)).py(px(10.)).text_size(px(14.)).text_color(soft).child("Loading…"),
                        );
                    }
                    Some(lists) if lists.is_empty() => {
                        rows = rows.child(
                            div()
                                .px(px(12.))
                                .py(px(10.))
                                .text_size(px(14.))
                                .text_color(soft)
                                .child(format!("No {}s yet.", kind.noun())),
                        );
                    }
                    Some(lists) => {
                        for list in lists {
                            let (target, picked) = (list.clone(), item.clone());
                            let detail = match list.child_count {
                                Some(1) => "1 item".to_string(),
                                Some(n) => format!("{n} items"),
                                None => String::new(),
                            };
                            rows = rows.child(
                                div()
                                    .id(SharedString::from(format!("lists.row.{}", list.id)))
                                    .min_h(px(44.))
                                    .px(px(12.))
                                    .py(px(6.))
                                    .rounded(px(12.))
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgba(0xffffff1f)))
                                    .child(icon(
                                        match kind {
                                            ListKind::Playlist => LucideIcon::ListVideo,
                                            ListKind::Collection => LucideIcon::GalleryVerticalEnd,
                                        },
                                        18.,
                                        t.colors.foreground,
                                    ))
                                    .child(
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
                                                    .text_color(t.colors.foreground)
                                                    .child(list.name.clone()),
                                            )
                                            .when(!detail.is_empty(), |el| {
                                                el.child(
                                                    div()
                                                        .text_size(px(12.))
                                                        .text_color(soft)
                                                        .child(detail),
                                                )
                                            }),
                                    )
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        this.add_to_list(
                                            kind,
                                            target.clone(),
                                            picked.clone(),
                                            window,
                                            cx,
                                        )
                                    })),
                            );
                        }
                    }
                }
                card = card
                    .child(
                        div()
                            .id("lists.rows")
                            .max_h(px(264.))
                            .overflow_y_scroll()
                            .child(rows),
                    )
                    .child(match kind {
                        ListKind::Playlist => field("New playlist"),
                        ListKind::Collection => field("New collection"),
                    })
                    .child(buttons("Create", ButtonKind::Primary, cx));
            }
            Dialog::Rename { .. } => {
                card = card
                    .child(field("Name"))
                    .child(buttons("Rename", ButtonKind::Primary, cx));
            }
            Dialog::Confirm { action, .. } => {
                let action: &'static str = if action == "Delete" { "Delete" } else { "OK" };
                card = card.child(buttons(action, ButtonKind::Danger, cx));
            }
        }

        Some(
            div()
                .id("lists.overlay")
                .absolute()
                .inset_0()
                .occlude()
                .track_focus(&self.lists.focus)
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_lists_dialog(window, cx);
                    }
                    // The keys of the dialog are not shortcuts of the page.
                    cx.stop_propagation();
                }))
                // A click beside the dialog closes it.
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.close_lists_dialog(window, cx)
                }))
                .child(card),
        )
    }
}
