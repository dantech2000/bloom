// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared media presentation: poster/wide cards, section rows, small helpers.

use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    Axis, Context, Div, ElementId, InteractiveElement as _, ObjectFit, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, StyledImage as _, Svg, div,
    img, prelude::FluentBuilder as _, px,
};

use crate::{
    app::{Jellyui, POSTER_H, POSTER_W, WIDE_H, WIDE_W},
    jellyfin::{Client, Item, format_runtime},
    ui::{scroll_area::ScrollArea, theme::UiTheme},
};

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
    cx: &mut Context<Jellyui>,
) -> Div {
    let t = UiTheme::read(cx).clone();
    let muted = t.colors.muted;
    let fg = t.colors.muted_foreground;
    div()
        .w(px(w))
        .h(px(h))
        .rounded(radius)
        .overflow_hidden()
        .bg(muted)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(placeholder, 28., fg))
        .when_some(
            url.and_then(|url| crate::images::image(&url, cx)),
            |el, image| {
                el.child(
                    img(image)
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(w))
                        .h(px(h))
                        .object_fit(ObjectFit::Cover),
                )
            },
        )
        .relative()
}

fn progress_bar(progress: Option<f32>, cx: &Context<Jellyui>) -> Option<Div> {
    let t = UiTheme::read(cx);
    let value = progress?;
    Some(
        div()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(4.))
            .bg(gpui_kit::black().alpha(0.55))
            .child(
                div()
                    .h_full()
                    .w(gpui_kit::relative(value))
                    .bg(t.colors.primary),
            ),
    )
}

fn badge(text: impl Into<SharedString>, cx: &Context<Jellyui>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .absolute()
        .top(px(6.))
        .right(px(6.))
        .px(px(6.))
        .py(px(1.))
        .rounded(px(5.))
        .bg(t.colors.primary)
        .text_color(t.colors.primary_foreground)
        .text_size(px(11.))
        .line_height(px(16.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child(text.into())
}

fn overlay_badges(item: &Item, cx: &Context<Jellyui>) -> Vec<Div> {
    let t = UiTheme::read(cx);
    let mut out = Vec::new();
    match item.user_data.unplayed_item_count {
        Some(n) if n > 0 && (item.is_series() || item.kind == "Season") => {
            out.push(badge(n.to_string(), cx))
        }
        _ => {}
    }
    if item.user_data.played && item.is_playable() {
        out.push(
            div()
                .absolute()
                .top(px(6.))
                .right(px(6.))
                .size(px(20.))
                .rounded_full()
                .bg(t.colors.primary)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(LucideIcon::Check, 13., t.colors.primary_foreground)),
        );
    }
    out
}

/// Vertical poster card (movies, series, seasons).
pub fn poster_card(item: &Item, client: &Client, cx: &mut Context<Jellyui>) -> Stateful<Div> {
    let t = UiTheme::read(cx).clone();
    let url = item.poster_url(client, 300);
    let subtitle = match item.kind.as_str() {
        "Episode" => item.series_name.clone(),
        "Season" => item.series_name.clone(),
        _ => item.production_year.map(|y| y.to_string()),
    };
    let target = item.clone();
    div()
        .id(card_id("poster", item))
        .w(px(POSTER_W))
        .flex()
        .flex_col()
        .gap(px(6.))
        .cursor_pointer()
        .child(
            div()
                .relative()
                .rounded(t.radius.md)
                .border_2()
                .border_color(gpui_kit::transparent_black())
                .hover(|s| s.border_color(t.colors.primary))
                .child(
                    artwork(
                        url,
                        POSTER_W - 4.,
                        POSTER_H - 4.,
                        t.radius.sm,
                        LucideIcon::Film,
                        cx,
                    )
                    .children(progress_bar(item.progress(), cx))
                    .children(overlay_badges(item, cx)),
                ),
        )
        .child(
            div()
                .px(px(2.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .truncate()
                        .child(item.name.clone()),
                )
                .when_some(subtitle, |el, s| {
                    el.child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.colors.muted_foreground)
                            .truncate()
                            .child(s),
                    )
                }),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.open_item(target.clone(), cx)))
}

/// Wide card (episodes, continue watching).
pub fn wide_card(item: &Item, client: &Client, cx: &mut Context<Jellyui>) -> Stateful<Div> {
    let t = UiTheme::read(cx).clone();
    let url = item
        .wide_url(client, 500)
        .or_else(|| item.poster_url(client, 500));
    let (title, subtitle) = match item.kind.as_str() {
        "Episode" => (
            item.series_name
                .clone()
                .unwrap_or_else(|| item.name.clone()),
            Some(match item.episode_code() {
                Some(code) => format!("{code} · {}", item.name),
                None => item.name.clone(),
            }),
        ),
        _ => (
            item.name.clone(),
            item.production_year.map(|y| y.to_string()),
        ),
    };
    let target = item.clone();
    div()
        .id(card_id("wide", item))
        .w(px(WIDE_W))
        .flex()
        .flex_col()
        .gap(px(6.))
        .cursor_pointer()
        .child(
            div()
                .relative()
                .rounded(t.radius.md)
                .border_2()
                .border_color(gpui_kit::transparent_black())
                .hover(|s| s.border_color(t.colors.primary))
                .child(
                    artwork(
                        url,
                        WIDE_W - 4.,
                        WIDE_H - 4.,
                        t.radius.sm,
                        LucideIcon::Tv,
                        cx,
                    )
                    .children(progress_bar(item.progress(), cx))
                    .children(overlay_badges(item, cx)),
                ),
        )
        .child(
            div()
                .px(px(2.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .truncate()
                        .child(title),
                )
                .when_some(subtitle, |el, s| {
                    el.child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.colors.muted_foreground)
                            .truncate()
                            .child(s),
                    )
                }),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.open_item(target.clone(), cx)))
}

/// Horizontal list row with a heading.
/// `card_h` is the artwork height; the row reserves room for the two text lines below it.
pub fn section(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    card_h: f32,
    cards: Vec<Stateful<Div>>,
    cx: &Context<Jellyui>,
) -> Div {
    let t = UiTheme::read(cx);
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            div()
                .px(px(28.))
                .text_size(px(18.))
                .line_height(px(24.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child(title.into()),
        )
        .child(
            ScrollArea::new(id)
                .axis(Axis::Horizontal)
                .w_full()
                .h(px(card_h + 62.))
                .child(
                    div()
                        .flex()
                        .gap(px(16.))
                        .px(px(28.))
                        .pb(px(6.))
                        .children(cards),
                ),
        )
}

pub fn skeleton_row(id: &'static str, count: usize, w: f32, h: f32, cx: &Context<Jellyui>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .flex()
        .gap(px(16.))
        .px(px(28.))
        .children((0..count).map(|i| {
            div()
                .id((id, i))
                .w(px(w))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .w(px(w))
                        .h(px(h))
                        .rounded(t.radius.md)
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

pub fn empty_state(title: &str, hint: &str, cx: &Context<Jellyui>) -> Div {
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

pub fn meta_line(item: &Item, cx: &Context<Jellyui>) -> Div {
    let t = UiTheme::read(cx);
    let mut parts: Vec<String> = Vec::new();
    if let Some(year) = item.production_year {
        parts.push(year.to_string());
    }
    if let Some(secs) = item.runtime_secs() {
        parts.push(format_runtime(secs));
    }
    if let Some(rating) = &item.official_rating {
        parts.push(rating.clone());
    }
    if let Some(score) = item.community_rating {
        parts.push(format!("★ {score:.1}"));
    }
    if !item.genres.is_empty() {
        parts.push(
            item.genres
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    div()
        .flex()
        .flex_wrap()
        .gap(px(8.))
        .text_color(t.colors.muted_foreground)
        .children(parts.into_iter().map(|p| div().child(p)))
}
