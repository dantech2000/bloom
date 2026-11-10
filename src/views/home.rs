// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Home page. The sections and their order come from the user's settings in
//! the web client: recently added, library tiles, continue watching, next up.

use gpui_kit::{
    Context, IntoElement, ParentElement as _, Render, SharedString, Styled, WeakEntity, Window,
    div, px,
};

use crate::{
    app::{CardRow, HomeData, Bloom, Page},
    jellyfin::{Client, Item},
    ui::scroll_area::ScrollArea,
    views::{hero::HERO_ROWS_START, shell::TOPBAR_H},
    views::cards::{
        CARD_TEXT_H, Cards, empty_state, library_tile, poster_card, section as section_row,
        section_height, skeleton_row, wide_card,
    },
};

/// Space between two home rows.
const SECTION_GAP: f32 = 18.;
/// Space under the last home row.
const ROWS_BOTTOM: f32 = 72.;

/// One row of the home page.
enum HomeSection<'a> {
    /// "Recently Added in" a library, with its items.
    Latest(&'a Item, &'a [Item]),
    /// The libraries as tiles.
    Media,
    Resume,
    NextUp,
}

/// The rows under the home hero as their own view, so GPUI can keep them
/// from one frame to the next while only the hero changes.
pub struct HomeRows {
    pub app: WeakEntity<Bloom>,
}

impl Render for HomeRows {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.app
            .update(cx, |app, cx| app.render_home_rows(cx))
            .unwrap_or_else(|_| div())
    }
}

impl Bloom {
    pub fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The rows start inside the lower part of the hero, as on the web.
        let hero = self.render_hero(cx);
        let overlap = match &hero {
            // The web offsets the rows from their place under the top bar.
            Some(_) => self.hero_height() - self.viewport_h * HERO_ROWS_START - TOPBAR_H - 16.,
            None => -(TOPBAR_H + 16.),
        };
        // A redraw for the hero alone (a trailer frame, the progress line)
        // reuses the rows of the last frame: they are a cached view, so GPUI
        // does not build or lay them out again. Any other redraw builds them.
        let rows = match self.home_rows_height() {
            Some(height) if self.hero.only_frame => {
                let mut size = div().w_full().h(px(height));
                self.rows_view
                    .clone()
                    .cached(size.style().clone())
                    .into_any_element()
            }
            _ => self.rows_view.clone().into_any_element(),
        };
        div().size_full().child(
            ScrollArea::new("home.scroll")
                .track(&self.page_scroll)
                .size_full()
                .children(hero)
                .child(div().relative().mt(px(-overlap)).child(rows)),
        )
    }

    /// The rows of the page in the order of the user's settings, each
    /// with the height of its cards. The height of the cached rows and the
    /// rows themselves both come from this list, so they cannot differ.
    fn home_sections<'a>(&'a self, data: &'a HomeData) -> Vec<(HomeSection<'a>, f32)> {
        let m = self.metrics();
        let portrait = m.portrait_h() + CARD_TEXT_H;
        let wide = m.backdrop_h() + CARD_TEXT_H;
        let mut sections = Vec::new();
        for kind in &data.order {
            match kind.as_str() {
                "latestmedia" => sections.extend(
                    data.latest
                        .iter()
                        .map(|(library, items)| (HomeSection::Latest(library, items), portrait)),
                ),
                "smalllibrarytiles" | "librarybuttons" if !self.views.is_empty() => {
                    sections.push((HomeSection::Media, m.backdrop_h()))
                }
                "resume" if !data.resume.is_empty() => sections.push((HomeSection::Resume, wide)),
                "nextup" if !data.next_up.is_empty() => sections.push((HomeSection::NextUp, wide)),
                _ => {}
            }
        }
        sections
    }

    /// Height of the home rows when it is known without a layout pass.
    fn home_rows_height(&self) -> Option<f32> {
        let Page::Home(data) = &self.page else {
            return None;
        };
        if data.loading || self.session.is_none() {
            return None;
        }
        let sections = self.home_sections(data);
        if sections.is_empty() {
            return None;
        }
        let heights: f32 = sections.iter().map(|(_, row_h)| section_height(*row_h)).sum();
        let gaps = SECTION_GAP * (sections.len() - 1) as f32;
        Some(heights + gaps + ROWS_BOTTOM)
    }

    /// The rows under the hero. [`HomeRows`] calls this for its render.
    pub fn render_home_rows(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let Page::Home(data) = &self.page else {
            return div();
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let m = self.metrics();
        let mut sections: Vec<gpui_kit::Div> = Vec::new();

        if data.loading
            && data.resume.is_empty()
            && data.next_up.is_empty()
            && data.latest.is_empty()
        {
            sections.push(skeleton_row(
                self,
                "sk.latest",
                8,
                m.portrait_w,
                m.portrait_h(),
                cx,
            ));
            sections.push(skeleton_row(
                self,
                "sk.resume",
                5,
                m.backdrop_w,
                m.backdrop_h(),
                cx,
            ));
        } else {
            // Top of the first row in the page, as laid out by `render_home`.
            let mut y = if self.hero.slides.is_empty() {
                TOPBAR_H + 16.
            } else {
                self.viewport_h * HERO_ROWS_START + TOPBAR_H + 16.
            };
            // A row outside the window, and 200 points around it, becomes
            // empty space of its height.
            let scrolled = -f32::from(self.page_scroll.offset().y);
            let (view_top, view_bottom) = (scrolled - 200., scrolled + self.viewport_h + 200.);
            for (section, row_h) in self.home_sections(data) {
                let height = section_height(row_h);
                let visible = y + height >= view_top && y <= view_bottom;
                sections.push(if visible {
                    self.home_section(section, row_h, data, &client, cx)
                } else {
                    div().h(px(height)).flex_shrink_0()
                });
                y += height + SECTION_GAP;
            }
            if sections.is_empty() {
                sections.push(empty_state(
                    "Nothing here yet",
                    "Start watching something and it will show up on your home screen.",
                    cx,
                ));
            }
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(SECTION_GAP))
            .pb(px(ROWS_BOTTOM))
            .children(sections)
    }

    /// One row of the page, with cards `row_h` tall.
    fn home_section(
        &self,
        section: HomeSection,
        row_h: f32,
        data: &HomeData,
        client: &Client,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let m = self.metrics();
        match section {
            HomeSection::Latest(library, items) => section_row(
                self,
                SharedString::from(format!("home.latest.{}", library.id)),
                format!("Recently Added in {}", library.name),
                Some(library.clone()),
                row_h,
                Cards::new(m.portrait_w, items.len(), |i, cx| {
                    poster_card(&items[i], client, m.portrait_w, cx)
                }),
                cx,
            ),
            HomeSection::Media => section_row(
                self,
                "home.media",
                "My Media",
                None,
                row_h,
                Cards::new(m.backdrop_w, self.views.len(), |i, cx| {
                    library_tile(&self.views[i], client, m.backdrop_w, cx)
                }),
                cx,
            ),
            HomeSection::Resume => section_row(
                self,
                "home.resume",
                "Continue Watching",
                None,
                row_h,
                Cards::new(m.backdrop_w, data.resume.len(), |i, cx| {
                    wide_card(&data.resume[i], client, m.backdrop_w, CardRow::Resume, cx)
                }),
                cx,
            ),
            HomeSection::NextUp => section_row(
                self,
                "home.nextup",
                "Next Up",
                None,
                row_h,
                Cards::new(m.backdrop_w, data.next_up.len(), |i, cx| {
                    wide_card(&data.next_up[i], client, m.backdrop_w, CardRow::NextUp, cx)
                }),
                cx,
            ),
        }
    }
}
