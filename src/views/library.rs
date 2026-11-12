// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Library grid and search results.

use crate::ui::tone;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgba,
};

use crate::{
    app::{Bloom, LibraryShow, PAGE_SIZE, Page},
    icons::{Filled, filled},
    jellyfin::ItemKind,
    ui::{
        glass::glass,
        menu::Menu,
        motion::{Enter, enter},
        scroll_area::ScrollArea,
        theme::UiTheme,
    },
    views::{
        cards::{empty_state, grid, icon, skeleton_row},
        parts::{TOOL_HOVER, icon_button, icon_menu, menu_over, play_all_pill},
    },
};
use crate::ui::tip::tip;

/// The letters of the rail at the right edge: "#" for a title that starts
/// with no letter, then the alphabet.
const LETTERS: [&str; 27] = [
    "#", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
    "S", "T", "U", "V", "W", "X", "Y", "Z",
];

impl Bloom {
    pub fn render_library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Library(data) = &self.page else {
            unreachable!()
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let colors = UiTheme::read(cx).colors;
        let m = self.metrics();
        let fg = colors.foreground;
        let soft = rgba(0xf5f5f7c7);
        let playable = matches!(
            data.view.collection_type.as_deref(),
            Some("movies") | Some("tvshows")
        );

        let range = if data.total == 0 {
            String::new()
        } else {
            let last = (data.start + data.items.len()).max(data.start + 1);
            format!("{}-{} of {}", data.start + 1, last.min(data.total), data.total)
        };

        // On a phone the toolbar takes two rows: the view with the range
        // and the pages, then the actions. Its tools are 44 points there,
        // the size a thumb needs; 38 on the Mac.
        let compact = self.compact_w();
        let tool_size = if compact { 44. } else { 38. };
        let has_previous = data.start > 0;
        let has_next = data.start + PAGE_SIZE < data.total;
        let page_arrow = |id: &'static str, glyph: LucideIcon, enabled: bool, forward: bool| {
            let label = if forward { "Next page" } else { "Previous page" };
            icon_button(id, label, icon(glyph, 22., fg), tool_size, TOOL_HOVER)
                .tooltip(tip(label))
                .when(!enabled, |el| el.opacity(0.3))
                .when(enabled, |el| {
                    el.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.update_library(
                            |d| {
                                d.start = if forward {
                                    d.start + PAGE_SIZE
                                } else {
                                    d.start.saturating_sub(PAGE_SIZE)
                                }
                            },
                            cx,
                        )
                    }))
                })
        };

        let play_all = play_all_pill(
            ("library.play-all", "library.shuffle"),
            tool_size,
            cx.listener(|this, _: &ClickEvent, window, cx| this.play_library(false, window, cx)),
            cx.listener(|this, _: &ClickEvent, window, cx| this.play_library(true, window, cx)),
            cx,
        );

        let menu = |state: &gpui_kit::Entity<crate::ui::menu::MenuState>,
                    label: &'static str,
                    glyph: LucideIcon,
                    active: bool| {
            menu_over(
                state,
                label,
                icon_button(label, label, icon(glyph, 20., fg), tool_size, TOOL_HOVER)
                    .tooltip(tip(label))
                    // A filter or a sort that is not the usual one lights
                    // its button.
                    .when(active, |el| el.bg(rgba(TOOL_HOVER))),
            )
        };

        let toolbar_h = if compact { 98. } else { 54. };
        let views = !LibraryShow::of(data.view.collection_type.as_deref()).is_empty();
        let toolbar = div()
            .px(px(m.side))
            .h(px(toolbar_h))
            .flex()
            .when(compact, |el| el.flex_wrap())
            .items_center()
            .gap(px(if compact { 10. } else { 18. }))
            .child({
                // The Mac: the title, which opens the views of the library
                // when it has some. A phone has the title in the top bar,
                // so the row holds the view as a small pill instead (the
                // title stood twice, 2026-10-23), or nothing without views.
                if compact {
                    if views {
                        Menu::new(&self.show_menu, "View")
                            .trigger(
                                div()
                                    .id("library.view")
                                    .role(gpui_kit::Role::Button)
                                    .aria_label("View")
                                    .h(px(36.))
                                    .pl(px(14.))
                                    .pr(px(8.))
                                    .rounded_full()
                                    .bg(tone::HOVER)
                                    .flex()
                                    .items_center()
                                    .gap(px(2.))
                                    .text_size(px(15.))
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(fg)
                                    .child(data.show.label(&data.title))
                                    .child(filled(Filled::DropDown, 22., soft)),
                            )
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }
                } else {
                    let title = div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_size(px(22.))
                        .text_color(soft)
                        .child(data.show.label(&data.title));
                    if views {
                        Menu::new(&self.show_menu, "View")
                            .trigger(title.child(filled(Filled::DropDown, 24., soft)))
                            .into_any_element()
                    } else {
                        title.into_any_element()
                    }
                }
            })
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(soft)
                    .child(if data.loading && data.items.is_empty() {
                        "∙".to_string()
                    } else {
                        range.clone()
                    }),
            )
            .child(div().flex_1())
            .when(compact, |el| {
                el.child(page_arrow("library.previous", LucideIcon::ChevronLeft, has_previous, false))
                    .child(page_arrow("library.next", LucideIcon::ChevronRight, has_next, true))
                    // The second row starts here.
                    .child(div().w_full().h_0())
            })
            .when(playable, |el| el.child(play_all))
            .when(compact, |el| el.child(div().flex_1()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    // The page of a collection has a menu of its own.
                    .when(data.view.kind == ItemKind::BoxSet, |el| {
                        el.child(icon_menu(
                            &self.more_menu,
                            "library.more",
                            "More",
                            filled(Filled::More, 22., fg),
                            tool_size,
                            TOOL_HOVER,
                        ))
                    })
                    .child(menu(
                        &self.filter_menu,
                        "Filter",
                        LucideIcon::Funnel,
                        data.filter.is_some(),
                    ))
                    .child(menu(
                        &self.sort_menu,
                        "Sort",
                        LucideIcon::ArrowDownAZ,
                        data.sort != "SortName" || data.descending,
                    ))
                    .when(!compact, |el| {
                        el.child(page_arrow(
                            "library.previous",
                            LucideIcon::ChevronLeft,
                            has_previous,
                            false,
                        ))
                        .child(page_arrow(
                            "library.next",
                            LucideIcon::ChevronRight,
                            has_next,
                            true,
                        ))
                    }),
            );

        // The pages again under the grid: a page of a hundred is ten screens
        // on a phone, and the arrows at the top were the only way on.
        let footer = (data.total > PAGE_SIZE && !(data.loading && data.items.is_empty())).then(|| {
            div()
                .px(px(m.side))
                .pt(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(14.))
                .child(page_arrow("library.previous.end", LucideIcon::ChevronLeft, has_previous, false))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(soft)
                        .child(range.clone()),
                )
                .child(page_arrow("library.next.end", LucideIcon::ChevronRight, has_next, true))
        });

        let body = if data.loading && data.items.is_empty() {
            skeleton_row(
                self,
                "sk.library",
                m.grid_columns,
                m.grid_w,
                m.grid_w * 1.5,
                cx,
            )
        } else if data.items.is_empty() {
            empty_state(
                "No items found",
                "Change the filter or the letter to see more.",
                cx,
            )
        } else {
            // The grid starts under the toolbar and its gap. A new page of
            // it lifts into place (`ui::motion`); the key names the page,
            // so a redraw of the same page runs nothing.
            let key = SharedString::from(format!("library.grid.{}.{:?}.{}", data.view.id, data.show, data.start));
            div()
                .child(enter(grid(self, &data.items, &client, toolbar_h + 12., cx), key, Enter::Lift, cx))
        };

        // Letter picker at the right edge, as on the web. It needs a tall
        // window and a sort by name.
        let picker = (data.sort == "SortName" && self.viewport_h >= 610. && !compact).then(|| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(6.))
                .flex()
                .flex_col()
                .justify_center()
                .child(
                    div()
                        .relative()
                        .p(px(2.))
                        .rounded_full()
                        .child(glass(px(999.), rgba(0x2a2a2a66)))
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .font_family(UiTheme::read(cx).fonts.mono.clone())
                        .text_size(px(11.))
                        .children(LETTERS.iter().enumerate().map(|(n, text)| {
                            let letter = text.chars().next().unwrap_or('#');
                            let selected = data.letter == Some(letter);
                            div()
                                .id(("library.letter", n))
                                .w(px(22.))
                                .h(px(16.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_color(if selected { colors.primary_foreground } else { soft })
                                .when(selected, |el| el.bg(colors.primary))
                                .when(!selected, |el| el.hover(|s| s.bg(tone::EDGE)))
                                .child(*text)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.update_library(
                                        |d| {
                                            d.letter =
                                                (d.letter != Some(letter)).then_some(letter)
                                        },
                                        cx,
                                    )
                                }))
                        })),
                )
        });

        div()
            .relative()
            .size_full()
            .child(
                ScrollArea::new("library.scroll")
                    .track(&self.page_scroll)
                    .size_full()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .pb(px(72.))
                            .child(toolbar)
                            .child(body)
                            .children(footer),
                    ),
            )
            .children(picker)
    }
}
