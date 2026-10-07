// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared media presentation: cards, section rows, small helpers. Sizes and
//! colours follow the Jellyfin web client with the Abyss theme.

use gpui_icons::{LucideIcon, lucide};
use std::time::Duration;

use gpui_kit::{
    App, ClickEvent, Context, Div, ElementId, InteractiveElement as _, MouseButton, MouseDownEvent,
    ObjectFit, ParentElement as _,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _, Styled, Svg, Window,
    div, prelude::FluentBuilder as _, px, rgb, rgba,
};

use crate::{
    app::{CardRow, Bloom},
    icons::{Filled, filled},
    jellyfin::{Client, Item},
    ui::theme::UiTheme,
};
use crate::ui::tip::tip;

/// Id of the card under the pointer.
pub struct HoveredCard(pub Option<SharedString>);

impl gpui_kit::Global for HoveredCard {}

/// Corner radius of card images.
pub const CARD_RADIUS: f32 = 12.;
/// Space between two card images in a row or grid.
pub const CARD_GAP: f32 = 27.;
/// Height of the two text lines under a card image.
pub const CARD_TEXT_H: f32 = 52.;

/// Sizes that depend on the window width, as the web client's `vw` rules do.
#[derive(Clone, Copy)]
pub struct Metrics {
    /// Left and right page padding.
    pub side: f32,
    /// Image width of a portrait card in a horizontal row.
    pub portrait_w: f32,
    /// Image width of a wide card in a horizontal row.
    pub backdrop_w: f32,
    /// Portrait cards per row in a wrapping grid.
    pub grid_columns: usize,
    /// Image width of a portrait card in a wrapping grid.
    pub grid_w: f32,
}

impl Metrics {
    pub fn new(viewport: f32) -> Self {
        let (portrait_vw, backdrop_vw, grid_columns) = match viewport {
            w if w >= 1920. => (10.41, 18.7, 9),
            w if w >= 1600. => (11.6, 18.7, 8),
            w if w >= 1400. => (13.3, 23.1, 7),
            w if w >= 1200. => (15.5, 23.1, 6),
            _ => (18.4, 30., 5),
        };
        let side = (viewport * 0.033).round() + 9.;
        let inner = viewport - side * 2.;
        let columns = grid_columns as f32;
        Self {
            side,
            portrait_w: (viewport * portrait_vw / 100. - CARD_GAP).floor(),
            backdrop_w: (viewport * backdrop_vw / 100. - CARD_GAP).floor(),
            grid_columns,
            grid_w: ((inner - CARD_GAP * (columns - 1.)) / columns).floor(),
        }
    }

    pub fn portrait_h(&self) -> f32 {
        (self.portrait_w * 1.5).round()
    }

    pub fn backdrop_h(&self) -> f32 {
        (self.backdrop_w * 9. / 16.).round()
    }
}

pub fn icon(name: LucideIcon, size: f32, color: gpui_kit::Rgba) -> Svg {
    lucide(name).size(px(size)).text_color(color)
}

pub fn card_id(prefix: &'static str, item: &Item) -> ElementId {
    ElementId::from(SharedString::from(format!("{prefix}.{}", item.id)))
}

/// Image with a muted placeholder while loading or when the item has no art.
pub fn artwork(
    url: Option<String>,
    w: f32,
    h: f32,
    radius: gpui_kit::Pixels,
    placeholder: LucideIcon,
    cx: &mut Context<Bloom>,
) -> Div {
    let t = UiTheme::read(cx).clone();
    div()
        .relative()
        .w(px(w))
        .h(px(h))
        .rounded(radius)
        .bg(t.colors.muted)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(placeholder, 36., t.colors.muted_foreground))
        .when_some(url, |el, url| {
            el.child(
                crate::images::remote_with(url, radius, ObjectFit::Cover)
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(w))
                    .h(px(h)),
            )
        })
}

/// Rounded bar across the bottom of a card image.
fn progress_bar(progress: Option<f32>) -> Option<Div> {
    let value = progress?;
    Some(
        div()
            .absolute()
            .bottom(px(7.))
            .left(px(12.))
            .right(px(12.))
            .h(px(7.))
            .rounded_full()
            .overflow_hidden()
            .bg(rgba(0x00000059))
            .child(
                div()
                    .h_full()
                    .rounded_full()
                    .w(gpui_kit::relative(value))
                    .bg(rgba(0xf5f5f7f2)),
            ),
    )
}

/// Watched check or unplayed count at the top right of a card image.
fn indicator(item: &Item) -> Option<Div> {
    let chip = || {
        div()
            .absolute()
            .top(px(5.))
            .right(px(5.))
            .min_w(px(26.))
            .h(px(26.))
            .px(px(4.))
            .rounded(px(10.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(0x121212))
            .text_size(px(13.))
            .font_weight(gpui_kit::FontWeight::BOLD)
    };
    match item.user_data.unplayed_item_count {
        Some(n) if n > 0 => {
            let text = if n >= 100 {
                "99+".to_string()
            } else {
                n.to_string()
            };
            return Some(chip().bg(rgba(0xf5f5f7cc)).child(text));
        }
        _ => {}
    }
    if item.user_data.played {
        return Some(
            chip()
                .bg(rgb(0xf5f5f7))
                .child(icon(LucideIcon::Check, 16., rgb(0x121212))),
        );
    }
    None
}

/// The two text lines under a card image.
fn card_text(title: String, subtitle: Option<String>, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
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
                .child(title),
        )
        .when_some(subtitle, |el, s| {
            el.child(
                div()
                    .text_size(px(15.))
                    .line_height(px(21.))
                    .text_color(t.colors.muted_foreground)
                    .truncate()
                    .child(s),
            )
        })
}

/// Dark layer with the play button and the watched, favourite and more
/// actions. It shows while the pointer is over the card.
fn hover_overlay(item: &Item, row: CardRow, cx: &mut Context<Bloom>) -> Div {
    let dim = rgba(0xffffffc2);
    let small = |id: &'static str, glyph: Svg| {
        div()
            .id(SharedString::from(format!("{id}.{}", item.id)))
            .size(px(32.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(rgba(0x00000066)))
            .child(glyph)
    };
    let played = item.user_data.played;
    let favorite = item.user_data.is_favorite;
    let (play_target, played_target, favorite_target, menu_target) =
        (item.clone(), item.clone(), item.clone(), item.clone());

    let mut overlay = div()
        .absolute()
        .inset_0()
        .rounded(px(CARD_RADIUS))
        .bg(rgba(0x000000b3))
        .border_1()
        .border_color(rgba(0xf5f5f733))
        .flex()
        .items_center()
        .justify_center();    if item.is_playable() || item.is_series() {
        overlay = overlay.child(
            div()
                .id(SharedString::from(format!("card.play.{}", item.id))).tooltip(tip("Play"))
                .size(px(58.))
                .rounded(px(CARD_RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(rgba(0x00000066)))
                .child(filled(Filled::Play, 40., dim))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    if play_target.is_series() {
                        this.play_series(&play_target, window, cx);
                    } else {
                        let resume = play_target.resume_secs() > 0;
                        this.play(&play_target, resume, window, cx);
                    }
                })),
        );
    }

    overlay.child(
        div()
            .absolute()
            .bottom(px(4.))
            .right(px(4.))
            .flex()
            .items_center()
            .child(
                small(
                    "card.played",
                    icon(
                        LucideIcon::Check,
                        20.,
                        if played { rgb(0xf5f5f7) } else { dim },
                    ),
                )
                .tooltip(tip(if played { "Mark unplayed" } else { "Mark played" }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.set_item_played(&played_target, !played, cx);
                })),
            )
            .child(
                small(
                    "card.favorite",
                    filled(
                        Filled::Heart,
                        20.,
                        if favorite { rgb(0xf92672) } else { dim },
                    ),
                )
                .tooltip(tip(if favorite {
                    "Remove from favorites"
                } else {
                    "Add to favorites"
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.set_item_favorite(&favorite_target, !favorite, cx);
                })),
            )
            .child(
                small("card.more", filled(Filled::More, 20., dim))
                    .tooltip(tip("More"))
                    .on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_card_menu(&menu_target, row, event.position(), window, cx);
                    },
                )),
            ),
    )
}

#[allow(clippy::too_many_arguments)]
fn card(
    prefix: &'static str,
    item: &Item,
    url: Option<String>,
    w: f32,
    h: f32,
    title: String,
    subtitle: Option<String>,
    placeholder: LucideIcon,
    row: CardRow,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    crate::perf::count_card();
    // The hover layer has about ten elements, so only the card under the
    // pointer gets one.
    // The row is part of the key: the same episode can be in Continue
    // Watching and in Next Up, and only the card under the pointer is hovered.
    let row_tag = match row {
        CardRow::None => "",
        CardRow::Resume => "resume.",
        CardRow::NextUp => "nextup.",
    };
    let key = SharedString::from(format!("{prefix}.{row_tag}{}", item.id));
    let hovered = cx
        .try_global::<HoveredCard>()
        .is_some_and(|h| h.0.as_ref() == Some(&key));
    let target = item.clone();
    let menu_target = item.clone();
    div()
        .id(ElementId::from(key.clone()))
        .w(px(w))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .cursor_pointer()
        .on_hover(cx.listener(move |_, inside: &bool, _, cx| {
            let current = cx.try_global::<HoveredCard>().and_then(|h| h.0.clone());
            if *inside {
                cx.set_global(HoveredCard(Some(key.clone())));
            } else if current.as_ref() == Some(&key) {
                cx.set_global(HoveredCard(None));
            }
            cx.notify();
        }))
        .child(
            artwork(url, w, h, px(CARD_RADIUS), placeholder, cx)
                // The web theme draws a faint light edge inside each image.
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(CARD_RADIUS))
                        .border_1()
                        .border_color(rgba(0xc8c8c81f)),
                )
                .children(progress_bar(item.progress()))
                .children(indicator(item))
                .when(hovered, |el| el.child(hover_overlay(item, row, cx))),
        )
        .child(card_text(title, subtitle, cx))
        .on_click(cx.listener(move |this, _, _, cx| this.open_item(target.clone(), cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.open_card_menu(&menu_target, row, event.position, window, cx);
            }),
        )
}

/// Vertical poster card (movies, series, seasons, collections).
pub fn poster_card(
    item: &Item,
    client: &Client,
    w: f32,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let h = (w * 1.5).round();
    let width = (w * 2.) as u32;
    let series_poster = match (&item.series_id, &item.series_primary_image_tag) {
        (Some(series), Some(tag)) if item.kind == "Episode" => {
            Some(client.image_url(series, "Primary", Some(tag), width))
        }
        _ => None,
    };
    let url = series_poster.or_else(|| item.poster_url(client, width));
    let (title, subtitle) = match item.kind.as_str() {
        "Episode" => (
            item.series_name
                .clone()
                .unwrap_or_else(|| item.name.clone()),
            Some(episode_line(item)),
        ),
        "Season" => (item.name.clone(), None),
        _ => (item.name.clone(), item.year_label()),
    };
    let placeholder = match item.kind.as_str() {
        "Series" | "Season" | "Episode" => LucideIcon::Tv,
        "BoxSet" => LucideIcon::GalleryVerticalEnd,
        "Playlist" => LucideIcon::ListVideo,
        "Folder" | "CollectionFolder" => LucideIcon::Folder,
        _ => LucideIcon::Clapperboard,
    };
    let card = card(
        "poster",
        item,
        url,
        w,
        h,
        title,
        subtitle,
        placeholder,
        CardRow::None,
        cx,
    );
    if !crate::jellyfin::quality_tags() {
        return card;
    }
    card.children(quality_tags(item))
}

/// Quality tags at the top left of a poster, as Jellyfin Enhanced shows them.
fn quality_tags(item: &Item) -> Option<Div> {
    let labels = item.quality_labels();
    if labels.is_empty() {
        return None;
    }
    Some(
        div()
            .absolute()
            .top(px(8.))
            .left(px(8.))
            .flex()
            .flex_col()
            .items_start()
            .gap(px(4.))
            .children(labels.into_iter().map(crate::quality::pill)),
    )
}

/// Wide card (continue watching, next up).
pub fn wide_card(
    item: &Item,
    client: &Client,
    w: f32,
    row: CardRow,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let h = (w * 9. / 16.).round();
    let width = (w * 2.) as u32;
    let url = item
        .wide_url(client, width)
        .or_else(|| item.poster_url(client, width));
    let (title, subtitle) = match item.kind.as_str() {
        "Episode" => (
            item.series_name
                .clone()
                .unwrap_or_else(|| item.name.clone()),
            Some(episode_line(item)),
        ),
        _ => (item.name.clone(), item.year_label()),
    };
    card(
        "wide",
        item,
        url,
        w,
        h,
        title,
        subtitle,
        LucideIcon::Tv,
        row,
        cx,
    )
}

/// Wide card of one episode with its own still and a single text line.
pub fn episode_card(
    item: &Item,
    client: &Client,
    w: f32,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let h = (w * 9. / 16.).round();
    let width = (w * 2.) as u32;
    let url = item
        .image_tags
        .primary
        .as_deref()
        .map(|tag| client.image_url(&item.id, "Primary", Some(tag), width))
        .or_else(|| item.wide_url(client, width));
    card(
        "episode",
        item,
        url,
        w,
        h,
        episode_line(item),
        None,
        LucideIcon::Tv,
        CardRow::None,
        cx,
    )
}

/// Image-only library tile ("My Media").
pub fn library_tile(
    view: &Item,
    client: &Client,
    w: f32,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let h = (w * 9. / 16.).round();
    let url = view
        .image_tags
        .primary
        .as_deref()
        .map(|tag| client.image_url(&view.id, "Primary", Some(tag), (w * 2.) as u32));
    let target = view.clone();
    div()
        .id(card_id("tile", view))
        .w(px(w))
        .flex_shrink_0()
        .cursor_pointer()
        .child(
            artwork(url, w, h, px(CARD_RADIUS), LucideIcon::Folder, cx).child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(CARD_RADIUS))
                    .border_1()
                    .border_color(rgba(0xc8c8c81f))
                    .hover(|s| s.bg(rgba(0x00000066)).border_color(rgba(0xf5f5f733))),
            ),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.open_library(target.clone(), cx)))
}

/// "S2:E5 - Name" as the web client prints it.
fn episode_line(item: &Item) -> String {
    match item.episode_code() {
        Some(code) => format!("{code} - {}", item.name),
        None => item.name.clone(),
    }
}

/// Moves a row by nearly one view width, the way the web arrows do. The row
/// glides there in about a quarter of a second.
fn page_row(handle: &ScrollHandle, direction: f32, window: &mut Window, cx: &mut App) {
    const STEP: Duration = Duration::from_millis(8);
    const STEPS: u32 = 34;
    let view = handle.bounds().size.width;
    let max = handle.max_offset().x;
    let from = handle.offset().x;
    let to = (from - view * 0.9 * direction).clamp(-max.max(px(0.)), px(0.));
    if to == from {
        return;
    }
    let handle = handle.clone();
    window
        .spawn(cx, async move |cx| {
            for step in 1..=STEPS {
                cx.background_executor().timer(STEP).await;
                // Ease out: fast at the start, slow into the end position.
                let t = step as f32 / STEPS as f32;
                let eased = 1. - (1. - t).powi(3);
                let moved = cx.update(|window, _| {
                    let mut offset = handle.offset();
                    offset.x = from + (to - from) * eased;
                    handle.set_offset(offset);
                    window.refresh();
                });
                if moved.is_err() {
                    break;
                }
            }
        })
        .detach();
}

/// The cards of a row or grid, built on demand: only the cards inside the
/// window are built for a render.
pub struct Cards<'a> {
    count: usize,
    /// Image width of one card.
    width: f32,
    build: Box<dyn FnMut(usize, &mut Context<Bloom>) -> Stateful<Div> + 'a>,
}

impl<'a> Cards<'a> {
    pub fn new(
        width: f32,
        count: usize,
        build: impl FnMut(usize, &mut Context<Bloom>) -> Stateful<Div> + 'a,
    ) -> Self {
        Self {
            count,
            width,
            build: Box::new(build),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Height of a row section: heading, gap, cards.
pub fn section_height(row_h: f32) -> f32 {
    30. + 10. + row_h
}

/// Horizontal row with a heading and arrow buttons at the top right.
/// `row_h` is the height of the tallest card in the row.
pub fn section(
    this: &Bloom,
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    link: Option<Item>,
    row_h: f32,
    cards: Cards,
    cx: &mut Context<Bloom>,
) -> Div {
    let side = this.metrics().side;
    section_padded(this, id, title, link, row_h, cards, side, side, cx)
}

/// [`section`] with its own left and right padding.
#[allow(clippy::too_many_arguments)]
pub fn section_padded(
    this: &Bloom,
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    link: Option<Item>,
    row_h: f32,
    mut cards: Cards,
    left: f32,
    right: f32,
    cx: &mut Context<Bloom>,
) -> Div {
    let t = UiTheme::read(cx).clone();
    let id: SharedString = id.into();
    let handle = this.row_scroll(&id);
    let fg = t.colors.foreground;

    // Build the cards in view and one more at each side; empty space of the
    // same width stands in for the rest, so the row scrolls as before.
    let stride = cards.width + CARD_GAP;
    let scrolled = -f32::from(handle.offset().x);
    // A row that starts inside the page (the rows of the detail page, at
    // the right of the poster) is cut off at its start: a card that scrolls
    // past it goes out of view there, and not across the poster column.
    let clip = if left > this.metrics().side + 1. { left } else { 0. };
    let left = left - clip;
    let first = (((scrolled - left) / stride).floor() - 1.).max(0.) as usize;
    let last = ((((scrolled + this.viewport_w - clip - left) / stride).ceil() + 1.).max(0.)
        as usize)
        .min(cards.count);
    let first = first.min(last);
    let built: Vec<Stateful<Div>> = (first..last)
        .map(|index| (cards.build)(index, cx).mr(px(CARD_GAP)))
        .collect();

    let heading = div()
        .id(SharedString::from(format!("{id}.title")))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(CARD_RADIUS))
        .text_size(px(22.))
        .line_height(px(30.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(fg)
        .child(title.into())
        .when_some(link, |el, target| {
            el.cursor_pointer()
                .child(icon(LucideIcon::ChevronRight, 22., fg))
                .on_click(cx.listener(move |this, _, _, cx| this.open_library(target.clone(), cx)))
        });

    let arrow = |name: &'static str, glyph: LucideIcon, direction: f32| {
        let handle = handle.clone();
        div()
            .id(SharedString::from(format!("{id}.{name}")))
            .size(px(36.))
            .rounded(px(CARD_RADIUS))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(rgba(0x00000066)))
            .child(icon(glyph, 22., fg))
                .tooltip(tip(if direction < 0. { "Previous" } else { "Next" }))
            .on_click(move |_, window, cx| page_row(&handle, direction, window, cx))
    };

    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .pl(px(left + clip))
                .pr(px(right))
                .h(px(30.))
                .flex()
                .items_center()
                .justify_between()
                .child(heading)
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(arrow("prev", LucideIcon::ChevronLeft, -1.))
                        .child(arrow("next", LucideIcon::ChevronRight, 1.)),
                ),
        )
        .child(
            div()
                .id(id)
                .ml(px(clip))
                .w(px(this.viewport_w - clip))
                .h(px(row_h))
                .overflow_x_scroll()
                // Only a sideways gesture moves the row. Without this, gpui
                // turns an up or down gesture over the row into a sideways
                // scroll, and the page cannot be scrolled from a row.
                .restrict_scroll_to_axis()
                .track_scroll(&handle)
                // A sideways gesture over the row scrolls it and never
                // turns the page (`swipe.rs`).
                .on_scroll_wheel(|_, _, cx| cx.default_global::<crate::swipe::Swipe>().claim())
                .child(
                    div()
                        // The full width of all cards, so the row can scroll
                        // to cards that are not built.
                        .w(px(left + cards.count as f32 * stride + right))
                        .flex_shrink_0()
                        .flex()
                        .pl(px(left))
                        .pr(px(right))
                        .child(div().flex_shrink_0().w(px(first as f32 * stride)))
                        .children(built)
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px((cards.count - last) as f32 * stride)),
                        ),
                ),
        )
}

/// Wrapping grid of portrait cards. `top` is where the grid starts in the
/// page; only the rows inside the window are built.
pub fn grid(
    this: &Bloom,
    items: &[Item],
    client: &Client,
    top: f32,
    cx: &mut Context<Bloom>,
) -> Div {
    const ROW_GAP: f32 = 14.;
    let m = this.metrics();
    let columns = m.grid_columns.max(1);
    let stride = (m.grid_w * 1.5).round() + CARD_TEXT_H + ROW_GAP;
    let rows = items.len().div_ceil(columns);
    let scrolled = -f32::from(this.page_scroll.offset().y) - top;
    let first = ((scrolled / stride).floor() - 1.).max(0.) as usize;
    let last = ((((scrolled + this.viewport_h) / stride).ceil() + 1.).max(0.) as usize).min(rows);
    let first = first.min(last);
    let visible = &items[(first * columns).min(items.len())..(last * columns).min(items.len())];
    div()
        .px(px(m.side))
        .flex()
        .flex_wrap()
        .gap_x(px(CARD_GAP))
        .child(div().w_full().h(px(first as f32 * stride)))
        .children(
            visible
                .iter()
                .map(|item| poster_card(item, client, m.grid_w, cx).mb(px(ROW_GAP))),
        )
        .child(div().w_full().h(px((rows - last) as f32 * stride)))
}

pub fn skeleton_row(
    this: &Bloom,
    id: &'static str,
    count: usize,
    w: f32,
    h: f32,
    cx: &Context<Bloom>,
) -> Div {
    let t = UiTheme::read(cx);
    div()
        .flex()
        .gap(px(CARD_GAP))
        .px(px(this.metrics().side))
        .overflow_hidden()
        .children((0..count).map(|i| {
            div()
                .id((id, i))
                .w(px(w))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .w(px(w))
                        .h(px(h))
                        .rounded(px(CARD_RADIUS))
                        .bg(t.colors.muted),
                )
                .child(
                    div()
                        .w(px(w * 0.7))
                        .h(px(12.))
                        .rounded(px(4.))
                        .bg(t.colors.muted),
                )
        }))
}

pub fn empty_state(title: &str, hint: &str, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .w_full()
        .py(px(64.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .child(icon(LucideIcon::Sparkles, 28., t.colors.muted_foreground))
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_color(t.colors.muted_foreground)
                .child(hint.to_string()),
        )
}
