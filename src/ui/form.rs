// SPDX-License-Identifier: AGPL-3.0-or-later
//! The form widgets of the pages, in the look of the theme: the card and
//! the group of a settings page, a field with its label and control, the
//! checkbox and the select. Settings, the dashboard and the pages of the
//! features use them.

use gpui_icons::LucideIcon;
use gpui_kit::base::Button as BaseButton;
use gpui_kit::{
    App, Div, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgba,
};

use super::{button, glass, theme::UiTheme, tone};
use crate::icons::icon;

/// The card of a settings page: the theme's `.verticalSection` (tinted
/// glass, 32 px corners, thin light border), with no content and no
/// padding yet.
pub fn card(cx: &App) -> Div {
    // Liquid glass prototype: the card is a glass panel with the ink of the
    // theme; off, it is the flat tint of the dark theme as before.
    let liquid = glass::liquid();
    let edge = if glass::liquid_light(cx) { tone::ON_ACCENT.opacity(0.2) } else { tone::EDGE };
    div()
        .rounded(px(32.))
        .border_1()
        .border_color(edge)
        .when(!liquid, |el| el.bg(tone::CARD))
        .when(liquid, |el| {
            el.relative().child(glass::card_glass(px(32.), glass::POPUP_TINT))
        })
}

/// A group of settings: a [`card`] with a title.
pub fn group(title: impl Into<SharedString>, cx: &App) -> Div {
    let t = UiTheme::read(cx);
    card(cx)
        .px(px(22.))
        .pt(px(18.))
        .pb(px(10.))
        .flex()
        .flex_col()
        .child(
            div()
                .mb(px(6.))
                .text_size(px(20.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child(title.into()),
        )
}

/// One setting: its label (weight 500, as `.inputLabel`) and description
/// (`.fieldDescription`) at the left, its control at the right.
pub fn field(
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    let t = UiTheme::read(cx);
    let description: SharedString = description.into();
    // A phone: a wide control (a choice, a value) goes under the label
    // instead of squeezing it to a letter a line.
    let compact = crate::views::shell::compact_now();
    div()
        .py(px(11.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(20.))
        .when(compact, |el| el.flex_wrap().gap(px(10.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .when(compact, |el| el.min_w(px(230.)))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(t.colors.foreground)
                        .child(label.into()),
                )
                .when(!description.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(t.colors.foreground.opacity(0.6))
                            .child(description),
                    )
                }),
        )
        .child(div().flex_shrink_0().child(control))
}

/// The theme's checkbox: 8 px corners, a faint accent outline, and an accent
/// fill with a dark mark when checked. Add `.on_click(...)`.
pub fn checkbox(id: impl Into<SharedString>, checked: bool, cx: &App) -> BaseButton {
    let t = UiTheme::read(cx);
    let id: SharedString = id.into();
    // For assistive tech and the tests that tap by label: the last part of
    // the id in words, as the player's buttons ("RememberAudioSelections",
    // a key of the server, reads as "Remember audio selections").
    let label: SharedString = control_label(id.rsplit('.').next().unwrap_or(&id)).into();
    button::bare(id, label)
        .role(gpui_kit::Role::CheckBox)
        .aria_toggled(if checked { gpui_kit::accesskit::Toggled::True } else { gpui_kit::accesskit::Toggled::False })
        .size(px(26.))
        .rounded(px(8.))
        .border_1()
        .when(checked, |el| {
            el.bg(t.colors.primary)
                .border_color(t.colors.primary)
                .child(icon(LucideIcon::Check, 18., tone::ON_ACCENT))
        })
        .when(!checked, |el| {
            el.border_color(tone::INK_30)
                .hover(|s| s.border_color(tone::INK_60))
        })
}

/// The theme's select: 8 px corners and a dark outline, the value at the
/// left and the arrow at the right. `longest` is the longest option, so the
/// field is as wide as its list needs. Add `.on_click(...)`.
pub fn select(
    id: impl Into<SharedString>,
    value: impl Into<SharedString>,
    longest: &str,
    cx: &App,
) -> BaseButton {
    select_capped(id, value, longest, control_max_w(), cx)
}

/// `select` with its own most width (a row that has buttons beside it).
pub fn select_capped(
    id: impl Into<SharedString>,
    value: impl Into<SharedString>,
    longest: &str,
    max_w: f32,
    cx: &App,
) -> BaseButton {
    let t = UiTheme::read(cx);
    let width = (longest.chars().count() as f32 * 8.2 + 58.).clamp(120., max_w.max(120.));
    let value: SharedString = value.into();
    // For assistive tech and the tests: a choice, named by its value.
    button::bare(id.into(), value.clone())
        .role(gpui_kit::Role::ComboBox)
        .w(px(width))
        .h(px(38.))
        .pl(px(12.))
        .pr(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(tone::RAISED)
        .bg(rgba(0x00000059))
        .justify_between()
        .gap(px(8.))
        .text_size(px(15.))
        .text_color(t.colors.foreground)
        .hover(|s| s.border_color(tone::INK_40))
        .child(div().min_w_0().truncate().child(value))
        .child(icon(LucideIcon::ChevronDown, 18., t.colors.foreground.opacity(0.7)))
}

/// The words of a control's key, in lower case: "PlayDefaultAudioTrack"
/// and "remember_audio" become "play default audio track" and "remember
/// audio" (the accessibility bridge capitalizes the first letter).
pub fn control_label(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let mut prev_lower = false;
    for c in key.chars() {
        if c == '_' || c == '-' {
            out.push(' ');
            prev_lower = false;
        } else if c.is_uppercase() {
            if prev_lower {
                out.push(' ');
            }
            out.extend(c.to_lowercase());
            prev_lower = false;
        } else {
            out.push(c);
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

/// The most width a control of a field takes: 340, or on a phone the
/// window less the margins of the page and the group.
pub fn control_max_w() -> f32 {
    let w = crate::views::shell::viewport_w_now();
    if crate::views::shell::compact_now() { (w - 80.).max(120.) } else { 340. }
}

/// A row of a list of sections, as Settings and the dashboard have one:
/// the theme's `.navMenuOption`, a rounded row with a round icon disc,
/// the accent fill and dark text when selected. `disc`, `glyph_size` and
/// `text` are the sizes of the disc, its icon and the label, which the
/// two lists set a little apart. Add the padding and `.on_click(...)`.
pub fn nav_row(
    id: impl Into<SharedString>,
    glyph: LucideIcon,
    label: &'static str,
    selected: bool,
    (disc, glyph_size, text): (f32, f32, f32),
    cx: &App,
) -> BaseButton {
    let colors = UiTheme::read(cx).colors;
    let ink = if selected { colors.primary_foreground } else { colors.foreground };
    button::bare(id.into(), label)
        .justify_start()
        .px(px(14.))
        .mb(px(2.))
        .rounded(px(12.))
        .gap(px(12.))
        .text_size(px(text))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(ink)
        .when(selected, |el| el.bg(colors.primary))
        .when(!selected, |el| el.hover(|s| s.bg(tone::FAINT)))
        .child(
            div()
                .size(px(disc))
                .flex_shrink_0()
                .rounded_full()
                .bg(if selected { rgba(0x12121226) } else { tone::DISC })
                .flex()
                .items_center()
                .justify_center()
                .child(icon(glyph, glyph_size, ink)),
        )
        .child(label)
}

/// A small square button with one icon, for a list row. Its label is
/// the last part of its id in words.
pub fn row_icon(id: impl Into<SharedString>, glyph: LucideIcon, enabled: bool, cx: &App) -> BaseButton {
    let t = UiTheme::read(cx);
    let color = t.colors.foreground.opacity(if enabled { 0.9 } else { 0.25 });
    let id: SharedString = id.into();
    let label = control_label(id.rsplit('.').next().unwrap_or(&id));
    button::bare(id, label)
        .disabled(!enabled)
        .size(px(34.))
        .rounded(px(12.))
        .when(enabled, |el| el.hover(|s| s.bg(rgba(0x00000066))))
        .when(!enabled, |el| el.cursor_default())
        .child(icon(glyph, 18., color))
}

#[cfg(test)]
mod tests {
    #[test]
    fn keys_become_words() {
        assert_eq!(super::control_label("PlayDefaultAudioTrack"), "play default audio track");
        assert_eq!(super::control_label("remember_audio"), "remember audio");
        assert_eq!(super::control_label("debug-channel"), "debug channel");
        assert_eq!(super::control_label("upnext"), "upnext");
    }
}
