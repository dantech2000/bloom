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
    jellyfin::Client,
    ui::scroll_area::ScrollArea,
    views::{hero::HERO_ROWS_START, shell::TOPBAR_H},
    views::cards::{
        CARD_TEXT_H, Cards, empty_state, library_tile, poster_card, section, section_height,
        skeleton_row, wide_card,
    },
};

/// Space between two home rows.
const SECTION_GAP: f32 = 18.;
/// Space under the last home row.
const ROWS_BOTTOM: f32 = 72.;

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
            Some(height) if self.hero_only_frame => {
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

    /// Height of the home rows when it is known without a layout pass.
    fn home_rows_height(&self) -> Option<f32> {
        let Page::Home(data) = &self.page else {
            return None;
        };
        if data.loading || self.session.is_none() {
            return None;
        }
        let m = self.metrics();
        let wide = section_height(m.backdrop_h() + CARD_TEXT_H);
        let mut heights: Vec<f32> = Vec::new();
        for kind in &data.order {
            match kind.as_str() {
                "latestmedia" => heights.extend(
                    data.latest
                        .iter()
                        .map(|_| section_height(m.portrait_h() + CARD_TEXT_H)),
                ),
                "smalllibrarytiles" | "librarybuttons" if !self.views.is_empty() => {
                    heights.push(section_height(m.backdrop_h()))
                }
                "resume" if !data.resume.is_empty() => heights.push(wide),
                "nextup" if !data.next_up.is_empty() => heights.push(wide),
                _ => {}
            }
        }
        if heights.is_empty() {
            return None;
        }
        let gaps = SECTION_GAP * (heights.len() - 1) as f32;
        Some(heights.iter().sum::<f32>() + gaps + ROWS_BOTTOM)
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
            let mut y = if self.hero.is_empty() {
                TOPBAR_H + 16.
            } else {
                self.viewport_h * HERO_ROWS_START + TOPBAR_H + 16.
            };
            for kind in &data.order {
                self.push_home_section(kind, data, &client, &mut y, &mut sections, cx);
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

    /// Adds the rows of one section kind. `y` is the top of the next row in
    /// the page; a row outside the window becomes empty space of its height.
    fn push_home_section(
        &self,
        kind: &str,
        data: &HomeData,
        client: &Client,
        y: &mut f32,
        sections: &mut Vec<gpui_kit::Div>,
        cx: &mut Context<Self>,
    ) {
        let m = self.metrics();
        let wide_row = m.backdrop_h() + CARD_TEXT_H;
        let scrolled = -f32::from(self.page_scroll.offset().y);
        let (view_top, view_bottom) = (scrolled - 200., scrolled + self.viewport_h + 200.);
        let mut place = |row_h: f32, build: &mut dyn FnMut() -> gpui_kit::Div| {
            let height = section_height(row_h);
            let visible = *y + height >= view_top && *y <= view_bottom;
            sections.push(if visible {
                build()
            } else {
                div().h(px(height)).flex_shrink_0()
            });
            *y += height + SECTION_GAP;
        };
        match kind {
            "latestmedia" => {
                for (library, items) in &data.latest {
                    let row_h = m.portrait_h() + CARD_TEXT_H;
                    place(row_h, &mut || {
                        section(
                            self,
                            SharedString::from(format!("home.latest.{}", library.id)),
                            format!("Recently Added in {}", library.name),
                            Some(library.clone()),
                            row_h,
                            Cards::new(m.portrait_w, items.len(), |i, cx| {
                                poster_card(&items[i], client, m.portrait_w, cx)
                            }),
                            cx,
                        )
                    });
                }
            }
            "smalllibrarytiles" | "librarybuttons" if !self.views.is_empty() => {
                place(m.backdrop_h(), &mut || {
                    section(
                        self,
                        "home.media",
                        "My Media",
                        None,
                        m.backdrop_h(),
                        Cards::new(m.backdrop_w, self.views.len(), |i, cx| {
                            library_tile(&self.views[i], client, m.backdrop_w, cx)
                        }),
                        cx,
                    )
                });
            }
            "resume" if !data.resume.is_empty() => {
                place(wide_row, &mut || {
                    section(
                        self,
                        "home.resume",
                        "Continue Watching",
                        None,
                        wide_row,
                        Cards::new(m.backdrop_w, data.resume.len(), |i, cx| {
                            wide_card(&data.resume[i], client, m.backdrop_w, CardRow::Resume, cx)
                        }),
                        cx,
                    )
                });
            }
            "nextup" if !data.next_up.is_empty() => {
                place(wide_row, &mut || {
                    section(
                        self,
                        "home.nextup",
                        "Next Up",
                        None,
                        wide_row,
                        Cards::new(m.backdrop_w, data.next_up.len(), |i, cx| {
                            wide_card(&data.next_up[i], client, m.backdrop_w, CardRow::NextUp, cx)
                        }),
                        cx,
                    )
                });
            }
            _ => {}
        }
    }
}
