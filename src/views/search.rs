// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search page: a row for each kind of library result, then titles from
//! Seerr that can be requested ("Discover on Seerr").

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, div, px, rgb, rgba,
};

use crate::{
    app::{CardRow, Bloom, Page},
    icons::{Filled, filled},
    jellyfin::{Item, SeerrItem},
    ui::{input::Input, scroll_area::ScrollArea, theme::UiTheme},
    views::cards::{
        CARD_RADIUS, CARD_TEXT_H, Cards, artwork, empty_state, icon, poster_card, section, skeleton_row,
        wide_card,
    },
};

impl Bloom {
    pub fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Search(data) = &self.page else {
            unreachable!()
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let m = self.metrics();
        let mut rows: Vec<Div> = Vec::new();

        if data.query.is_empty() && data.typed.is_empty() {
            rows.push(empty_state(
                "Search your libraries",
                "Type a title and press Enter.",
                cx,
            ));
        } else if data.loading && data.results.is_empty() && data.seerr.is_empty() {
            rows.push(skeleton_row(
                self,
                "sk.search",
                m.grid_columns,
                m.portrait_w,
                m.portrait_h(),
                cx,
            ));
        } else {
            let of_kind = |kind: &str| -> Vec<&Item> {
                data.results.iter().filter(|i| i.kind == kind).collect()
            };
            for (id, title, kind) in [
                ("search.movies", "Movies", "Movie"),
                ("search.shows", "Shows", "Series"),
            ] {
                let items = of_kind(kind);
                if items.is_empty() {
                    continue;
                }
                let cards = Cards::new(m.portrait_w, items.len(), |i, cx| {
                    poster_card(items[i], &client, m.portrait_w, cx)
                });
                rows.push(section(
                    self,
                    id,
                    title,
                    None,
                    m.portrait_h() + CARD_TEXT_H,
                    cards,
                    cx,
                ));
            }
            let episodes = of_kind("Episode");
            if !episodes.is_empty() {
                let cards = Cards::new(m.backdrop_w, episodes.len(), |i, cx| {
                    wide_card(episodes[i], &client, m.backdrop_w, CardRow::None, cx)
                });
                rows.push(section(
                    self,
                    "search.episodes",
                    "Episodes",
                    None,
                    m.backdrop_h() + CARD_TEXT_H,
                    cards,
                    cx,
                ));
            }
            if !data.seerr.is_empty() {
                let cards = Cards::new(m.portrait_w, data.seerr.len(), |i, cx| {
                    seerr_card(&data.seerr[i], m.portrait_w, cx)
                });
                rows.push(section(
                    self,
                    "search.seerr",
                    "Discover on Seerr",
                    None,
                    m.portrait_h() + CARD_TEXT_H,
                    cards,
                    cx,
                ));
            }
            if rows.is_empty() {
                rows.push(empty_state(
                    "No results",
                    &format!("Nothing matched “{}”.", data.query),
                    cx,
                ));
            }
        }

        div().size_full().child(
            ScrollArea::new("search.scroll")
                .track(&self.page_scroll)
                .size_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(22.))
                        .py(px(12.))
                        .pb(px(72.))
                        .child(
                            div().px(px(m.side)).flex().justify_center().child(
                                Input::new(&self.search_input)
                                    .aria_label("Search")
                                    .w(px(640.))
                                    .h(px(46.)),
                            ),
                        )
                        .children(rows),
                ),
        )
    }
}

/// Portrait card of a Seerr title with its type and request state.
pub(crate) fn seerr_card(item: &SeerrItem, w: f32, cx: &mut Context<Bloom>) -> Stateful<Div> {
    let t = UiTheme::read(cx).clone();
    let h = (w * 1.5).round();
    let (type_label, type_color) = if item.media_type == "tv" {
        ("SERIES", rgba(0xf333d6e6))
    } else {
        ("MOVIE", rgba(0x3b82f6e6))
    };
    // Seerr states: 2 pending, 3 processing, 4 partly available, 5 available.
    let status = match item.status() {
        Some(5) => Some((LucideIcon::Check, rgba(0x22c55eb3))),
        Some(4) => Some((LucideIcon::Minus, rgba(0x22c55eb3))),
        Some(3) => Some((LucideIcon::Clock, rgba(0x6366f1b3))),
        Some(2) => Some((LucideIcon::Bell, rgba(0xfb923cb3))),
        _ => None,
    };
    let mut details: Vec<String> = item.year().into_iter().collect();
    let score = item.vote_average.filter(|v| *v > 0.);
    let target = item.clone();
    let mut second = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(15.))
        .line_height(px(21.))
        .text_color(t.colors.muted_foreground);
    if let Some(year) = details.pop() {
        second = second.child(year);
    }
    if let Some(score) = score {
        second = second
            .child(filled(Filled::Star, 14., rgb(0xbdbdbd)))
            .child(format!("{score:.1}"));
    }
    div()
        .id(SharedString::from(format!(
            "seerr.{}.{}",
            item.media_type, item.id
        )))
        .w(px(w))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .cursor_pointer()
        .child(
            artwork(
                item.poster_url(),
                w,
                h,
                px(CARD_RADIUS),
                LucideIcon::Clapperboard,
                cx,
            )
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .left(px(8.))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .bg(type_color)
                    .text_size(px(10.))
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .text_color(rgb(0xffffff))
                    .child(type_label),
            )
            .children(status.map(|(glyph, color)| {
                div()
                    .absolute()
                    .top(px(8.))
                    .right(px(8.))
                    .size(px(24.))
                    .rounded_full()
                    .bg(color)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(glyph, 14., rgb(0xffffff)))
            }))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(CARD_RADIUS))
                    .border_1()
                    .border_color(rgba(0xc8c8c81f))
                    .hover(|s| s.bg(rgba(0x00000066)).border_color(rgba(0xf5f5f733))),
            ),
        )
        .child(
            div()
                .mt(px(4.))
                .h(px(CARD_TEXT_H - 4.))
                .pl(px(1.))
                .pr(px(8.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(17.))
                        .line_height(px(24.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground)
                        .truncate()
                        .child(item.display_name()),
                )
                .child(second),
        )
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            // A title in the library opens there; another one can be requested.
            match target.library_id() {
                Some(id) => this.open_item_id(id, cx),
                None => this.open_seerr_menu(&target, event.position(), window, cx),
            }
        }))
}
