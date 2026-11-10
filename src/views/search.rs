// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search page: a row for each kind of library result, then titles from
//! Seerr that can be requested ("Discover on Seerr").

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, div, px, rgb, rgba,
};

use crate::{
    app::{CardRow, Bloom, Page},
    icons::{Filled, filled},
    jellyfin::{Item, ItemKind, SeerrItem},
    ui::{
        input::Input,
        motion::{Enter, enter},
        scroll_area::ScrollArea,
    },
    views::cards::{
        CARD_RADIUS, CARD_TEXT_H, Cards, artwork, card_frame, card_second_line, card_text_with,
        empty_state, icon, image_edge, poster_card, section, skeleton_row, wide_card,
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
            // One pass over the results sorts them into the three rows.
            let (mut movies, mut shows, mut episodes): (Vec<&Item>, Vec<&Item>, Vec<&Item>) =
                Default::default();
            for item in &data.results {
                match item.kind {
                    ItemKind::Movie => movies.push(item),
                    ItemKind::Series => shows.push(item),
                    ItemKind::Episode => episodes.push(item),
                    _ => {}
                }
            }
            for (id, title, items) in [
                ("search.movies", "Movies", movies),
                ("search.shows", "Shows", shows),
            ] {
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
                                    .keyboard(crate::ui::input::Keyboard::search())
                                    .aria_label("Search")
                                    .w(px(640.))
                                    .h(px(46.)),
                            ),
                        )
                        // The results lift into place when they arrive
                        // (`ui::motion`): the key is the query and whether
                        // there are results, so the skeleton stays still
                        // and the rows come in once.
                        .child(enter(
                            div().flex().flex_col().gap(px(22.)).children(rows),
                            SharedString::from(format!(
                                "search.rows.{}.{}",
                                data.query,
                                !data.results.is_empty() || !data.seerr.is_empty()
                            )),
                            Enter::Lift,
                            cx,
                        )),
                ),
        )
    }
}

/// Portrait card of a Seerr title with its type and request state.
pub(crate) fn seerr_card(item: &SeerrItem, w: f32, cx: &mut Context<Bloom>) -> Stateful<Div> {
    let h = (w * 1.5).round();
    // One ink: the kind of a title is a word on a quiet pill, not a
    // colour. A colour says a state: green for there or on its way,
    // amber for a request that waits for a person.
    let type_label = if item.media_type == "tv" { "SERIES" } else { "MOVIE" };
    let type_color = rgba(0x2a2a2ad9);
    let (green, amber) = (rgba(0x2e9d5bcc), rgba(0xc98a1bcc));
    // Seerr states: 2 pending, 3 processing, 4 partly available, 5 available.
    let status = match item.status() {
        Some(5) => Some((LucideIcon::Check, green)),
        Some(4) => Some((LucideIcon::Minus, green)),
        Some(3) => Some((LucideIcon::Clock, green)),
        Some(2) => Some((LucideIcon::Bell, amber)),
        _ => None,
    };
    let score = item.vote_average.filter(|v| *v > 0.);
    let target = item.clone();
    let mut second = card_second_line(cx).flex().items_center().gap(px(6.));
    if let Some(year) = item.year() {
        second = second.child(year);
    }
    if let Some(score) = score {
        second = second
            .child(filled(Filled::Star, 14., rgb(0xbdbdbd)))
            .child(format!("{score:.1}"));
    }
    card_frame(SharedString::from(format!("seerr.{}.{}", item.media_type, item.id)), w)
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
            .child(image_edge(true)),
        )
        .child(card_text_with(item.display_name(), Some(second), cx))
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            // A title in the library opens there; another one can be requested.
            match target.library_id() {
                Some(id) => this.open_item_id(id, cx),
                None => this.open_seerr_menu(&target, event.position(), window, cx),
            }
        }))
}
