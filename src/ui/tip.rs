// SPDX-License-Identifier: AGPL-3.0-or-later
//! Small text label that comes up when the pointer rests on a button.
//! Use it with gpui's own tooltip, which waits half a second and builds the
//! view only when it shows: `.tooltip(tip("Mute (M)"))`.

use gpui_kit::{
    AnyView, App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString,
    Styled, Window, div, px, rgba,
};

struct Tip {
    label: SharedString,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // gpui puts the view at the pointer; the space keeps it clear of it.
        div().pt(px(6.)).child(
            div()
                .px(px(9.))
                .py(px(5.))
                .rounded(px(9.))
                .border_1()
                .border_color(rgba(0xf5f5f733))
                .bg(rgba(0x1c1c1cf2))
                .shadow_md()
                .font_family("Google Sans")
                .text_size(px(12.5))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(rgba(0xf5f5f7f2))
                .whitespace_nowrap()
                .child(self.label.clone()),
        )
    }
}

/// Tooltip with a text. A key the action has goes in the text: "Mute (M)".
pub fn tip(label: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let label = label.into();
    move |_, cx| {
        let label = label.clone();
        cx.new(|_| Tip { label }).into()
    }
}
