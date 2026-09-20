// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Home page: continue watching, next up, latest per library.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled, div, px};

use crate::{
    app::{Jellyui, POSTER_H, POSTER_W, Page, WIDE_H, WIDE_W},
    ui::scroll_area::ScrollArea,
    views::cards::{empty_state, poster_card, section, skeleton_row, wide_card},
};

impl Jellyui {
    pub fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Home(data) = &self.page else {
            unreachable!()
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let mut sections: Vec<gpui_kit::Div> = Vec::new();

        if data.loading
            && data.resume.is_empty()
            && data.next_up.is_empty()
            && data.latest.is_empty()
        {
            sections.push(skeleton_row("sk.resume", 5, WIDE_W, WIDE_H, cx));
            sections.push(skeleton_row("sk.latest", 8, POSTER_W, POSTER_H, cx));
        } else {
            if !data.resume.is_empty() {
                let cards = data
                    .resume
                    .iter()
                    .map(|item| wide_card(item, &client, cx))
                    .collect();
                sections.push(section(
                    "home.resume",
                    "Continue watching",
                    WIDE_H,
                    cards,
                    cx,
                ));
            }
            if !data.next_up.is_empty() {
                let cards = data
                    .next_up
                    .iter()
                    .map(|item| wide_card(item, &client, cx))
                    .collect();
                sections.push(section("home.nextup", "Next up", WIDE_H, cards, cx));
            }
            for (library, items) in &data.latest {
                let cards = items
                    .iter()
                    .map(|item| poster_card(item, &client, cx))
                    .collect();
                sections.push(section(
                    SharedString::from(format!("home.latest.{}", library.id)),
                    format!("Latest in {}", library.name),
                    POSTER_H,
                    cards,
                    cx,
                ));
            }
            if sections.is_empty() {
                sections.push(empty_state(
                    "Nothing here yet",
                    "Start watching something and it will show up on your home screen.",
                    cx,
                ));
            }
        }

        div().size_full().child(
            ScrollArea::new("home.scroll").size_full().child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(28.))
                    .py(px(16.))
                    .pb(px(32.))
                    .children(sections),
            ),
        )
    }
}
