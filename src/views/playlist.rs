// SPDX-License-Identifier: AGPL-3.0-or-later
//! The page of one playlist, and the dialog of the lists ("Add to playlist",
//! "Add to collection", rename, delete).

use crate::ui::tone;
use gpui_icons::LucideIcon;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, InteractiveElement as _, IntoElement, KeyDownEvent,
    ObjectFit, ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled,
    div, prelude::FluentBuilder as _, px, rgba,
};

use crate::{
    admin::{ButtonKind, button},
    app::{Bloom, Page},
    jellyfin::{Client, ItemKind, format_runtime},
    lists::{Dialog, Entry, ListKind},
    ui::{
        input::Input,
        scroll_area::ScrollArea,
        theme::UiTheme,
        tip::tip,
    },
    views::{
        cards::{empty_state, icon},
        parts::{TOOL_HOVER, icon_button, play_all_pill},
    },
};

const ROW_H: f32 = 72.;
/// Space between two rows.
const ROW_GAP: f32 = 4.;
/// The quiet text of the page.
const SOFT: u32 = 0xf5f5f7c7;

/// "1 item", "5 items".
fn items_label(count: usize) -> String {
    if count == 1 { "1 item".to_string() } else { format!("{count} items") }
}

/// One entry of the playlist: its place, picture, title, length, and the
/// buttons to play from it, move it and remove it. `n` is its place among
/// `total` entries.
fn playlist_row(
    n: usize,
    total: usize,
    entry: &Entry,
    client: &Client,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let fg = UiTheme::read(cx).colors.foreground;
    let soft = rgba(SOFT);
    let item = &entry.item;
    let wide_thumb = item.wide_url(client, 320);
    let wide = wide_thumb.is_some();
    let thumb = wide_thumb.or_else(|| item.poster_url(client, 320));
    let title = if item.kind == ItemKind::Episode {
        item.display_title()
    } else {
        item.name.clone()
    };
    let detail = match item.kind {
        ItemKind::Episode => item.series_name.clone().unwrap_or_default(),
        _ => item.year_label().unwrap_or_default(),
    };
    let runtime = item.runtime_secs().map(format_runtime).unwrap_or_default();
    let open = item.clone();
    let small = |name: &'static str, glyph: LucideIcon, label: &'static str| {
        div()
            .id((name, n))
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
                    small("playlist.play", LucideIcon::Play, "Play from here").on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.play_playlist(n, false, window, cx)
                        }),
                    ),
                )
                .child(
                    small("playlist.up", LucideIcon::ChevronUp, "Move up")
                        .when(n == 0, |el| el.opacity(0.3))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            if let Some(to) = crate::lists::step_target(total, n, true) {
                                this.playlist_move(n, to, cx)
                            }
                        })),
                )
                .child(
                    small("playlist.down", LucideIcon::ChevronDown, "Move down")
                        .when(n + 1 >= total, |el| el.opacity(0.3))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            if let Some(to) = crate::lists::step_target(total, n, false) {
                                this.playlist_move(n, to, cx)
                            }
                        })),
                )
                .child(
                    small("playlist.remove", LucideIcon::X, "Remove from the playlist").on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.playlist_remove(n, cx)
                        }),
                    ),
                ),
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.open_item(open.clone(), cx)
        }))
}

impl Bloom {
    pub fn render_playlist(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Playlist(data) = &self.page else {
            unreachable!()
        };
        if self.session.is_none() {
            return div();
        }
        let m = self.metrics();
        let fg = UiTheme::read(cx).colors.foreground;
        let soft = rgba(SOFT);
        let total = data.entries.len();
        let secs: i64 = data
            .entries
            .iter()
            .filter_map(|e| e.item.runtime_secs())
            .sum();
        let summary = if data.loading && total == 0 {
            "∙".to_string()
        } else if secs > 0 {
            format!("{} · {}", items_label(total), format_runtime(secs))
        } else {
            items_label(total)
        };

        let tool = |id: &'static str, glyph: LucideIcon, label: &'static str| {
            icon_button(id, label, icon(glyph, 20., fg), 38., TOOL_HOVER).tooltip(tip(label))
        };
        let play_all = play_all_pill(
            ("playlist.play-all", "playlist.shuffle"),
            38.,
            cx.listener(|this, _: &ClickEvent, window, cx| this.play_playlist(0, false, window, cx)),
            cx.listener(|this, _: &ClickEvent, window, cx| this.play_playlist(0, true, window, cx)),
            cx,
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
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.ask_delete_list(list.clone(), cx)
                            }),
                        )
                    }),
            );

        let body = if data.loading && total == 0 {
            div().px(px(m.side)).text_color(soft).child("Loading…")
        } else if total == 0 {
            empty_state(
                "This playlist is empty",
                "Use Add to playlist in the menu of a title.",
                cx,
            )
        } else {
            // Only the rows in view are built (`ui::rows`): a playlist
            // of five hundred entries cost five hundred rows at each frame
            // of a scroll.
            div().px(px(m.side)).child(crate::ui::rows::rows(
                cx,
                total,
                ROW_H + ROW_GAP,
                |this, range, cx| {
                    let (Page::Playlist(data), Some(session)) = (&this.page, &this.session) else {
                        return Vec::new();
                    };
                    let total = data.entries.len();
                    (range.start.min(total)..range.end.min(total))
                        .map(|n| {
                            playlist_row(n, total, &data.entries[n], &session.client, cx)
                                .into_any_element()
                        })
                        .collect()
                },
            ))
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
    pub fn render_lists_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // The question of the app ("Delete playlist?", "Delete user?")
        // draws here as well: this is the place the root render has for
        // a dialog over any page, and one of the two shows at a time.
        let Some(dialog) = self.lists.dialog.as_ref() else {
            return self.render_confirm(cx);
        };
        let fg = UiTheme::read(cx).colors.foreground;
        let soft = tone::INK_70;

        let (title, message) = match dialog {
            Dialog::Add { kind, item, .. } => (
                format!("Add to {}", kind.noun()),
                item.display_title(),
            ),
            Dialog::Rename { list } => ("Rename playlist".to_string(), list.name.clone()),
        };
        // A phone: the width of the window less the margins.
        let mut card = crate::ui::dialog::card("lists.card", (self.viewport_w - 32.).min(440.))
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .text_color(fg.opacity(0.8))
                    .truncate()
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
                        .text_color(fg.opacity(0.7))
                        .child(label),
                )
                .child(Input::new(&self.lists.input)
                        .keyboard(crate::ui::input::Keyboard::name())
                        .aria_label(label)
                        .w_full())
        };
        let buttons = |action: SharedString, kind: ButtonKind, cx: &mut Context<Self>| {
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

        match dialog {
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
                                    .hover(|s| s.bg(tone::HOVER))
                                    .child(icon(
                                        match kind {
                                            ListKind::Playlist => LucideIcon::ListVideo,
                                            ListKind::Collection => LucideIcon::GalleryVerticalEnd,
                                        },
                                        18.,
                                        fg,
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
                                                    .text_color(fg)
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
                    .child(buttons("Create".into(), ButtonKind::Primary, cx));
            }
            Dialog::Rename { .. } => {
                card = card
                    .child(field("Name"))
                    .child(buttons("Rename".into(), ButtonKind::Primary, cx));
            }
        }

        Some(
            crate::ui::dialog::layer("lists.overlay", card, cx)
                .track_focus(&self.lists.focus)
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
                })),
        )
        .map(|layer| crate::ui::dialog::shown(layer, "lists.overlay", cx))
    }
}

#[cfg(test)]
mod tests {
    use super::items_label;

    /// The page said "1 items".
    #[test]
    fn one_item_is_singular() {
        assert_eq!(items_label(1), "1 item");
        assert_eq!(items_label(0), "0 items");
        assert_eq!(items_label(12), "12 items");
    }
}
