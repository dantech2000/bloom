// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared media presentation: cards, section rows, small helpers. Sizes and
//! colours follow the Jellyfin web client with the Abyss theme.

use gpui_icons::{LucideIcon, lucide};
use std::{collections::HashMap, rc::Rc, time::Duration};

use gpui_kit::{
    App, Bounds, ClickEvent, Context, DispatchPhase, Div, ElementId, InteractiveElement as _,
    LongPressEvent, MouseButton, MouseDownEvent, ObjectFit, ParentElement as _, Pixels,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _, Styled, Svg,
    TouchPhase, Window, canvas, div, prelude::FluentBuilder as _, px, rgb, rgba,
};

use crate::{
    app::{CardRow, Bloom},
    icons::{Filled, filled},
    jellyfin::{Client, Item, ItemKind},
    ui::theme::UiTheme,
};
use crate::ui::tip::tip;

/// Id of the card under the pointer.
pub struct HoveredCard(pub Option<SharedString>);

impl gpui_kit::Global for HoveredCard {}

/// Corner radius of card images.
pub const CARD_RADIUS: f32 = 12.;
/// Space between two card images in a row or grid on the Mac; a phone has
/// `Metrics::gap`.
pub const CARD_GAP: f32 = 27.;
/// The gap on a phone: three posters across at 121 points, not 109.
const PHONE_GAP: f32 = 14.;
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
    /// Space between two cards in a row or a grid.
    pub gap: f32,
}

impl Metrics {
    pub fn new(viewport: f32) -> Self {
        let (portrait_vw, backdrop_vw, grid_columns) = match viewport {
            w if w >= 1920. => (10.41, 18.7, 9),
            w if w >= 1600. => (11.6, 18.7, 8),
            w if w >= 1400. => (13.3, 23.1, 7),
            w if w >= 1200. => (15.5, 23.1, 6),
            // A phone: three posters across, and a card a finger can hit.
            w if w < crate::views::shell::COMPACT_W => (34., 64., 3),
            _ => (18.4, 30., 5),
        };
        let compact = viewport < crate::views::shell::COMPACT_W;
        let side = if compact { 16. } else { (viewport * 0.033).round() + 9. };
        let gap = if compact { PHONE_GAP } else { CARD_GAP };
        let inner = viewport - side * 2.;
        let columns = grid_columns as f32;
        Self {
            side,
            portrait_w: (viewport * portrait_vw / 100. - gap).floor(),
            backdrop_w: (viewport * backdrop_vw / 100. - gap).floor(),
            grid_columns,
            grid_w: ((inner - gap * (columns - 1.)) / columns).floor(),
            gap,
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
    let colors = UiTheme::read(cx).colors;
    div()
        .relative()
        .w(px(w))
        .h(px(h))
        .rounded(radius)
        .bg(colors.muted)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(placeholder, 36., colors.muted_foreground))
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
pub fn progress_bar(progress: Option<f32>) -> Option<Div> {
    Some(progress_bar_sized(progress?, 12., 7., 7., 0x00000059))
}

/// The bar of [`progress_bar`] with its own sizes: `side` from the left
/// and right edges, `bottom` from the bottom, `h` tall, over `track`.
pub fn progress_bar_sized(value: f32, side: f32, bottom: f32, h: f32, track: u32) -> Div {
    div()
        .absolute()
        .bottom(px(bottom))
        .left(px(side))
        .right(px(side))
        .h(px(h))
        .rounded_full()
        .overflow_hidden()
        .bg(rgba(track))
        .child(
            div()
                .h_full()
                .rounded_full()
                .w(gpui_kit::relative(value))
                .bg(rgba(0xf5f5f7f2)),
        )
}

/// The faint light edge the web theme draws inside each image. With
/// `hover` the image dims under the pointer and the edge lights.
pub fn image_edge(hover: bool) -> Div {
    div()
        .absolute()
        .inset_0()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgba(0xc8c8c81f))
        .when(hover, |el| {
            el.hover(|s| s.bg(rgba(0x00000066)).border_color(rgba(0xf5f5f733)))
        })
}

/// The still of an episode: its own picture, or else the wide image.
pub fn still_url(item: &Item, client: &Client, width: u32) -> Option<String> {
    item.image_tags
        .primary
        .as_deref()
        .map(|tag| client.image_url(&item.id, "Primary", Some(tag), width))
        .or_else(|| item.wide_url(client, width))
}

/// "3. Name" for an episode with a number, the name alone without one.
pub fn numbered_name(item: &Item) -> String {
    match item.index_number {
        Some(n) => format!("{n}. {}", item.name),
        None => item.name.clone(),
    }
}

/// Watched check or unplayed count at the top right of a card image.
pub fn indicator(item: &Item) -> Option<Div> {
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
fn card_text(title: String, subtitle: Option<String>, cx: &App) -> Div {
    card_text_with(title, subtitle.map(|s| card_second_line(cx).truncate().child(s)), cx)
}

/// The second, quiet line under a card image, without its content.
pub fn card_second_line(cx: &App) -> Div {
    div()
        .text_size(px(15.))
        .line_height(px(21.))
        .text_color(UiTheme::read(cx).colors.muted_foreground)
}

/// The text under a card image: the title, and a second line the caller
/// made from [`card_second_line`].
pub fn card_text_with(title: String, second: Option<Div>, cx: &App) -> Div {
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
                .text_color(UiTheme::read(cx).colors.foreground)
                .truncate()
                .child(title),
        )
        .children(second)
}

/// The frame of a card: the width of its image, set as least and most
/// width too, which spares a layout pass (`Bloom::render_settings`).
pub fn card_frame(id: impl Into<ElementId>, w: f32) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(w))
        .min_w(px(w))
        .max_w(px(w))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .cursor_pointer()
}

/// Dark layer with the play button and the watched, favourite and more
/// actions. It shows while the pointer is over the card.
fn hover_overlay(item: &Rc<Item>, row: CardRow, cx: &mut Context<Bloom>) -> Div {
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
    // The listeners share the card's copy of the item.
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
        .justify_center();
    if item.is_playable() || item.is_series() {
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
                    this.play_item(&play_target, window, cx);
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
    // One copy of the item for all the listeners of the card: a card is
    // built again at each frame, and an item is forty fields.
    let item = &Rc::new(item.clone());
    let target = item.clone();
    let menu_target = item.clone();
    // A finger: a long press on the card opens its menu, as the right
    // button does; the hover layer with "More" never shows on a touch
    // screen. The press is claimed (`prevent_default`), or GPUI takes the
    // release for a tap and opens the item under the menu. Only the
    // touch platform sends a long press, so the Mac builds no listener
    // for each card of each frame.
    let long_press = cfg!(not(target_os = "macos")).then(|| {
        let press_target = item.clone();
        let press: Rc<dyn Fn(&LongPressEvent, &mut Window, &mut App)> =
            Rc::new(cx.listener(move |this, event: &LongPressEvent, window, cx| {
                this.open_card_menu(&press_target, row, event.start_position, window, cx);
            }));
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, _| {
                let press = press.clone();
                window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble
                        && event.phase == TouchPhase::Started
                        && bounds.contains(&event.start_position)
                    {
                        window.prevent_default();
                        press(event, window, cx);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    });
    card_frame(ElementId::from(key.clone()), w)
        .relative()
        // For assistive tech and the tests that tap by label.
        .role(gpui_kit::Role::Button)
        .aria_label(title.clone())
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
                .child(image_edge(false))
                .children(progress_bar(item.progress()))
                .children(indicator(item))
                .when(hovered, |el| el.child(hover_overlay(item, row, cx))),
        )
        .child(card_text(title, subtitle, cx))
        .children(long_press)
        .on_click(cx.listener(move |this, _, _, cx| this.open_item((*target).clone(), cx)))
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
    let width = image_px(w);
    let series_poster = match (&item.series_id, &item.series_primary_image_tag) {
        (Some(series), Some(tag)) if item.kind == ItemKind::Episode => {
            Some(client.image_url(series, "Primary", Some(tag), width))
        }
        _ => None,
    };
    let url = series_poster.or_else(|| item.poster_url(client, width));
    let (title, subtitle) = card_titles(item);
    let placeholder = match item.kind {
        ItemKind::Series | ItemKind::Season | ItemKind::Episode => LucideIcon::Tv,
        ItemKind::BoxSet => LucideIcon::GalleryVerticalEnd,
        ItemKind::Playlist => LucideIcon::ListVideo,
        ItemKind::Folder | ItemKind::CollectionFolder => LucideIcon::Folder,
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
    let labels = crate::quality_tags::labels(item);
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
            .children(labels.into_iter().map(crate::quality_tags::pill)),
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
    let width = image_px(w);
    let url = item
        .wide_url(client, width)
        .or_else(|| item.poster_url(client, width));
    let (title, subtitle) = card_titles(item);
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
    let width = image_px(w);
    let url = still_url(item, client, width);
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
        .map(|tag| client.image_url(&view.id, "Primary", Some(tag), image_px(w)));
    let target = view.clone();
    card_frame(card_id("tile", view), w)
        .child(artwork(url, w, h, px(CARD_RADIUS), LucideIcon::Folder, cx).child(image_edge(true)))
        .on_click(cx.listener(move |this, _, _, cx| this.open_library(target.clone(), cx)))
}

/// The two lines under a card: an episode has its series over its own
/// line, a season its name alone, anything else its name over its year.
fn card_titles(item: &Item) -> (String, Option<String>) {
    match item.kind {
        ItemKind::Episode => (
            item.series_name
                .clone()
                .unwrap_or_else(|| item.name.clone()),
            Some(episode_line(item)),
        ),
        ItemKind::Season => (item.name.clone(), None),
        _ => (item.name.clone(), item.year_label()),
    }
}

/// "S2:E5 - Name" as the web client prints it.
fn episode_line(item: &Item) -> String {
    match item.episode_code() {
        Some(code) => format!("{code} - {}", item.name),
        None => item.name.clone(),
    }
}

/// The glide that owns each row, by the id of the row. A click on an arrow
/// while the row still glides starts a second glide; without an owner the
/// two wrote their own offsets to the row in turn, and the row shook.
#[derive(Default)]
struct RowGlides(HashMap<SharedString, u64>);

impl gpui_kit::Global for RowGlides {}

/// Steps of a glide, and the time between two.
const GLIDE_STEP: Duration = Duration::from_millis(8);
const GLIDE_STEPS: u32 = 34;

impl RowGlides {
    /// Starts a glide of a row and returns its number; the glide before
    /// it, when one runs, owns the row no more.
    fn start(&mut self, row: &SharedString) -> u64 {
        let number = self.0.entry(row.clone()).or_default();
        *number += 1;
        *number
    }

    /// Where the glide `number` of a row puts it at a step, between `from`
    /// and `to`; `None` when a later glide took the row.
    fn offset(&self, row: &SharedString, number: u64, from: f32, to: f32, step: u32) -> Option<f32> {
        if self.0.get(row) != Some(&number) {
            return None;
        }
        // Ease out: fast at the start, slow into the end position.
        let t = step as f32 / GLIDE_STEPS as f32;
        let eased = 1. - (1. - t).powi(3);
        Some(from + (to - from) * eased)
    }
}

/// Moves a row by nearly one view width, the way the web arrows do. The row
/// glides there in about a quarter of a second, or jumps under Reduce
/// Motion.
fn page_row(
    row: &SharedString,
    handle: &ScrollHandle,
    direction: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let view = handle.bounds().size.width;
    let max = handle.max_offset().x;
    let from = handle.offset().x;
    let to = (from - view * 0.9 * direction).clamp(-max.max(px(0.)), px(0.));
    if to == from {
        return;
    }
    let number = cx.default_global::<RowGlides>().start(row);
    if UiTheme::read(cx).motion.reduced || cx.reduce_motion() {
        let mut offset = handle.offset();
        offset.x = to;
        handle.set_offset(offset);
        window.refresh();
        return;
    }
    let (row, handle) = (row.clone(), handle.clone());
    let (from, to) = (f32::from(from), f32::from(to));
    window
        .spawn(cx, async move |cx| {
            for step in 1..=GLIDE_STEPS {
                cx.background_executor().timer(GLIDE_STEP).await;
                let moved = cx.update(|window, cx| {
                    let Some(x) = cx.default_global::<RowGlides>().offset(&row, number, from, to, step)
                    else {
                        return false;
                    };
                    let mut offset = handle.offset();
                    offset.x = px(x);
                    handle.set_offset(offset);
                    window.refresh();
                    true
                });
                if !matches!(moved, Ok(true)) {
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
    let id: SharedString = id.into();
    let handle = this.row_scroll(&id);
    let fg = UiTheme::read(cx).colors.foreground;

    // Build the cards in view and one more at each side; empty space of the
    // same width stands in for the rest, so the row scrolls as before.
    let m = this.metrics();
    let gap = m.gap;
    let stride = cards.width + gap;
    let scrolled = -f32::from(handle.offset().x);
    // A row that starts inside the page (the rows of the detail page, at
    // the right of the poster) is cut off at its start: a card that scrolls
    // past it goes out of view there, and not across the poster column.
    let clip = if left > m.side + 1. { left } else { 0. };
    let left = left - clip;
    let first = (((scrolled - left) / stride).floor() - 1.).max(0.) as usize;
    let last = ((((scrolled + this.viewport_w - clip - left) / stride).ceil() + 1.).max(0.)
        as usize)
        .min(cards.count);
    let first = first.min(last);
    let built: Vec<Stateful<Div>> = (first..last)
        .map(|index| (cards.build)(index, cx).mr(px(gap)))
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
        let (row, handle) = (id.clone(), handle.clone());
        div()
            .id(SharedString::from(format!("{id}.{name}")))
            .role(gpui_kit::Role::Button)
            .aria_label(if direction < 0. { "Previous" } else { "Next" })
            .size(px(36.))
            .rounded(px(CARD_RADIUS))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(rgba(0x00000066)))
            .child(icon(glyph, 22., fg))
            .tooltip(tip(if direction < 0. { "Previous" } else { "Next" }))
            .on_click(move |_, window, cx| page_row(&row, &handle, direction, window, cx))
    };

    // A phone pans the row with a finger: no arrows there, and the heading
    // has the whole width (they cut "Recently Added in Movies" short).
    let compact = this.compact_w();
    // The arrows are made before the row takes the id.
    let arrows = (!compact).then(|| {
        div()
            .flex()
            .gap(px(8.))
            .child(arrow("prev", LucideIcon::ChevronLeft, -1.))
            .child(arrow("next", LucideIcon::ChevronRight, 1.))
    });
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
                .children(arrows),
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
    let gap = m.gap;
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
        .gap_x(px(gap))
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
    let m = this.metrics();
    div()
        .flex()
        .gap(px(m.gap))
        .px(px(m.side))
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

/// Pixels to ask the server for an image shown at `w` points: the scale of
/// the window (2 on a Mac, 3 on a phone), so a poster is sharp on both.
fn image_px(w: f32) -> u32 {
    (w * crate::views::shell::scale_now().max(2.)).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two clicks on an arrow inside a quarter of a second ran two glides
    /// at once, and each wrote its own offsets to the row.
    #[test]
    fn a_second_glide_of_a_row_ends_the_first() {
        let row = SharedString::from("home.resume");
        let other = SharedString::from("home.nextup");
        let mut glides = RowGlides::default();
        let first = glides.start(&row);
        assert_eq!(glides.offset(&row, first, 0., -900., GLIDE_STEPS), Some(-900.));
        let second = glides.start(&row);
        assert_eq!(glides.offset(&row, first, 0., -900., 10), None, "a later glide took the row");
        assert!(glides.offset(&row, second, -300., -1200., 10).is_some());
        // Another row has its own glide.
        let third = glides.start(&other);
        assert!(glides.offset(&other, third, 0., -900., 1).is_some());
        assert!(glides.offset(&row, second, -300., -1200., 11).is_some());
    }

    #[test]
    fn the_titles_of_a_card() {
        let episode = Item {
            kind: "Episode".into(),
            name: "Pilot".into(),
            series_name: Some("The Show".into()),
            parent_index_number: Some(1),
            index_number: Some(2),
            ..Default::default()
        };
        assert_eq!(
            card_titles(&episode),
            ("The Show".to_string(), Some("S1:E2 - Pilot".to_string()))
        );
        assert_eq!(numbered_name(&episode), "2. Pilot");
        let season = Item { kind: "Season".into(), name: "Season 1".into(), ..Default::default() };
        assert_eq!(card_titles(&season), ("Season 1".to_string(), None));
        assert_eq!(numbered_name(&season), "Season 1");
    }
}
