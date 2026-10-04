// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Library grid and search results.

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgba,
};

use crate::{
    app::{Bloom, LibraryShow, PAGE_SIZE, Page},
    icons::{Filled, filled},
    ui::{glass::glass, menu::Menu, scroll_area::ScrollArea, theme::UiTheme},
    views::cards::{empty_state, grid, icon, skeleton_row},
};
use crate::ui::tip::tip;

impl Bloom {
    pub fn render_library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Library(data) = &self.page else {
            unreachable!()
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let t = UiTheme::read(cx).clone();
        let m = self.metrics();
        let fg = t.colors.foreground;
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

        let tool = |id: &'static str, glyph: gpui_kit::Svg| {
            div()
                .id(id)
                .size(px(38.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xf5f5f71f)))
                .child(glyph)
        };
        let has_previous = data.start > 0;
        let has_next = data.start + PAGE_SIZE < data.total;
        let page_arrow = |id: &'static str, glyph: LucideIcon, enabled: bool, forward: bool| {
            tool(id, icon(glyph, 22., fg))
                .tooltip(tip(if forward { "Next page" } else { "Previous page" }))
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

        let play_all = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .text_color(t.colors.primary_foreground)
            .text_size(px(14.))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .child(
                div()
                    .id("library.play-all")
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
                        this.play_library(false, window, cx)
                    })),
            )
            .child(
                div()
                    .id("library.shuffle").tooltip(tip("Shuffle"))
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
                        this.play_library(true, window, cx)
                    })),
            );

        let menu = |state: &gpui_kit::Entity<crate::ui::menu::MenuState>,
                    label: &'static str,
                    glyph: LucideIcon,
                    active: bool| {
            Menu::new(state, label)
                .trigger_style_with(|button| button)
                .trigger(
                    div()
                        .id(label)
                        .tooltip(tip(label))
                        .size(px(38.))
                        .rounded(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(rgba(0xf5f5f71f)))
                        .when(active, |el| el.bg(rgba(0xf5f5f71f)))
                        .child(icon(glyph, 20., fg)),
                )
        };

        let toolbar = div()
            .px(px(m.side))
            .h(px(54.))
            .flex()
            .items_center()
            .gap(px(18.))
            .child({
                let title = div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(22.))
                    .text_color(soft)
                    .child(data.show.label(&data.title));
                // The title opens the views of the library, when it has some.
                if LibraryShow::of(data.view.collection_type.as_deref()).is_empty() {
                    title.into_any_element()
                } else {
                    Menu::new(&self.show_menu, "View")
                        .trigger_style_with(|button| button)
                        .trigger(title.child(filled(Filled::DropDown, 24., soft)))
                        .into_any_element()
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
                        range
                    }),
            )
            .child(div().flex_1())
            .when(playable, |el| el.child(play_all))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    // The page of a collection has a menu of its own.
                    .when(data.view.kind == "BoxSet", |el| {
                        el.child(
                            Menu::new(&self.more_menu, "More")
                                .trigger_style_with(|button| button)
                                .trigger(
                                    div()
                                        .id("library.more")
                                        .tooltip(tip("More"))
                                        .size(px(38.))
                                        .rounded(px(12.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .hover(|s| s.bg(rgba(0xf5f5f71f)))
                                        .child(filled(Filled::More, 22., fg)),
                                ),
                        )
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
                    .child(page_arrow(
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
                    )),
            );

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
            // The grid starts under the toolbar and its gap.
            grid(self, &data.items, &client, 54. + 12., cx)
        };

        // Letter picker at the right edge, as on the web. It needs a tall
        // window and a sort by name.
        let picker = (data.sort == "SortName" && self.viewport_h >= 610.).then(|| {
            let letters = std::iter::once('#').chain('A'..='Z');
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
                        .font_family(t.fonts.mono.clone())
                        .text_size(px(11.))
                        .children(letters.map(|letter| {
                            let selected = data.letter == Some(letter);
                            div()
                                .id(SharedString::from(format!("library.letter.{letter}")))
                                .w(px(22.))
                                .h(px(16.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_color(if selected {
                                    t.colors.primary_foreground
                                } else {
                                    soft
                                })
                                .when(selected, |el| el.bg(t.colors.primary))
                                .when(!selected, |el| el.hover(|s| s.bg(rgba(0xf5f5f733))))
                                .child(letter.to_string())
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
                            .child(body),
                    ),
            )
            .children(picker)
    }
}
