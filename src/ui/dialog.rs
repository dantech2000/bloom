// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shell of a dialog: a dark layer over the page with a glass card in
//! its middle. One place has the rules every dialog of the app follows:
//! the page behind takes no click, the card settles into place over a
//! layer that fades in, and on a phone the card centres in the room above
//! the keyboard. A dialog adds what is its own: the click beside the card
//! that closes it (`.on_click`), its keys, its focus.

use gpui_kit::{
    AnyElement, App, ClickEvent, Div, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled, div, px,
};

use super::{
    glass::{self, glass},
    motion::{Enter, enter},
    theme::UiTheme,
    tone,
};

/// The dark layer with `card` in its middle. Add `.on_click(...)` for the
/// click beside the card, and show it with [`shown`].
pub fn layer(id: impl Into<SharedString>, card: impl IntoElement + Styled + 'static, cx: &App) -> Stateful<Div> {
    let id: SharedString = id.into();
    let settle = SharedString::from(format!("{id}.card.in"));
    div()
        .id(id)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        // A phone: a field of the dialog raises the keyboard, and the
        // dialog centres in the room above it.
        .pb(px(crate::views::shell::keyboard_inset()))
        .bg(tone::SCRIM)
        // The page behind takes no click while the dialog is open: a
        // hitbox does not block by itself, and the click that closes the
        // dialog also pressed the button under the pointer.
        .occlude()
        .child(enter(card, settle, Enter::Settle, cx))
}

/// A [`layer`] with its handlers, as the element of the page: it fades in.
pub fn shown(layer: Stateful<Div>, id: impl Into<SharedString>, cx: &App) -> AnyElement {
    let id: SharedString = id.into();
    enter(layer, SharedString::from(format!("{id}.in")), Enter::Fade, cx)
}

/// The glass card of a dialog, `width` wide and no wider than the window,
/// with no content yet. A click in the card does not reach the layer,
/// which would close the dialog.
pub fn card(id: impl Into<SharedString>, width: f32) -> Stateful<Div> {
    div()
        .id(id.into())
        .relative()
        .w(px(width))
        .max_w_full()
        .rounded(px(24.))
        .border_1()
        .border_color(tone::EDGE)
        .child(glass(px(24.), glass::POPUP_TINT))
        .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
}

/// A [`card`] with a title and a message, and room under them for the
/// fields and the buttons.
pub fn titled(id: impl Into<SharedString>, title: &str, message: &str, cx: &App) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    card(id, 440.)
        .p(px(24.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            div()
                .text_size(px(20.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(px(15.))
                .text_color(t.colors.foreground.opacity(0.8))
                .child(message.to_string()),
        )
}
