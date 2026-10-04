// SPDX-License-Identifier: AGPL-3.0-or-later
//! A large dialog for text of many lines, such as the custom CSS of the
//! server. [`super::dialogs::Prompt`] has single-line fields only. The editing
//! state lives in the admin page state, because a page renders with
//! `&Bloom` and cannot make an entity.

use std::{cell::Cell, rc::Rc};

use gpui_kit::{
    AppContext as _, ClickEvent, Context, Div, InteractiveElement as _, MouseButton, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
    rgba,
    base::input::{InputEditorStyle, InputEvent, Textarea, TextareaState},
};

use super::{ButtonKind, button};
use crate::{
    app::{Bloom, Page},
    ui::{glass::glass, theme::UiTheme},
};

/// What a caller asks for.
#[derive(Clone)]
pub struct TextEdit {
    pub title: String,
    pub message: String,
    /// The text of the button that applies the edit.
    pub action: String,
    /// A fixed-width font, for code.
    pub mono: bool,
    pub value: String,
    /// Gets the new text when the user applies the edit.
    pub apply: Rc<dyn Fn(&mut Bloom, String, &mut Context<Bloom>)>,
}

/// The open dialog.
pub struct TextEditor {
    spec: TextEdit,
    state: gpui_kit::Entity<TextareaState>,
    /// Rows the field has now, so a resize sets them only when they change.
    rows: Cell<usize>,
    _change: Subscription,
}

/// Height of one line of text in the field.
const LINE_H: f32 = 20.;
/// Height of the dialog besides the field: padding, title, message, footer.
const CHROME_H: f32 = 176.;

impl Bloom {
    pub fn text_editor_open(&self) -> bool {
        matches!(&self.page, Page::Admin(admin) if admin.text_editor.is_some())
    }

    /// Opens the dialog with the text in the field and the focus in it.
    pub fn open_text_editor(&mut self, spec: TextEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Admin(admin) = &mut self.page else {
            return;
        };
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(12)
                .soft_wrap(true)
                .placeholder("Empty")
        });
        state.update(cx, |state, cx| state.set_value(spec.value.clone(), window, cx));
        // The line count in the footer follows each key.
        let change = cx.subscribe(&state, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        state.update(cx, |state, cx| state.focus(window, cx));
        admin.text_editor = Some(TextEditor { spec, state, rows: Cell::new(0), _change: change });
        cx.notify();
    }

    /// Closes the dialog without an edit.
    pub fn close_text_editor(&mut self) {
        if let Page::Admin(admin) = &mut self.page {
            admin.text_editor = None;
        }
    }

    /// The text in the field of the dialog.
    pub fn text_editor_value(&self, cx: &gpui_kit::App) -> Option<String> {
        let Page::Admin(admin) = &self.page else {
            return None;
        };
        admin.text_editor.as_ref().map(|e| e.state.read(cx).value().to_string())
    }

    /// Puts text in the field, for the debug channel.
    pub fn text_editor_set(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Page::Admin(admin) = &self.page else {
            return false;
        };
        let Some(editor) = &admin.text_editor else {
            return false;
        };
        let text = text.to_string();
        editor.state.clone().update(cx, |state, cx| state.set_value(text, window, cx));
        cx.notify();
        true
    }

    /// Scrolls the field by lines, for the debug channel.
    pub fn text_editor_scroll(&mut self, lines: f32, cx: &mut Context<Self>) -> bool {
        let Page::Admin(admin) = &self.page else {
            return false;
        };
        let Some(editor) = &admin.text_editor else {
            return false;
        };
        editor.state.clone().update(cx, |state, cx| {
            state.set_scroll_offset(gpui_kit::point(px(0.), px(-lines * LINE_H)), cx)
        });
        true
    }

    /// Hands the text to the caller and closes the dialog.
    pub fn submit_text_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(value) = self.text_editor_value(cx) else {
            return;
        };
        let Page::Admin(admin) = &mut self.page else {
            return;
        };
        let Some(editor) = admin.text_editor.take() else {
            return;
        };
        window.focus(&self.app_focus, cx);
        (editor.spec.apply)(self, value, cx);
        cx.notify();
    }

    /// The dialog over the page.
    pub(crate) fn render_text_editor(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let Page::Admin(admin) = &self.page else {
            return None;
        };
        let editor = admin.text_editor.as_ref()?;
        let t = UiTheme::read(cx).clone();
        let colors = t.colors;
        let width = (self.viewport_w - 96.).clamp(420., 960.);
        let height = (self.viewport_h - 120.).clamp(320., 780.);
        let rows = (((height - CHROME_H - 24.) / LINE_H).floor() as usize).max(4);
        let state = editor.state.clone();
        if editor.rows.replace(rows) != rows {
            state.update(cx, |state, cx| state.set_rows(rows, cx));
        }
        state.update(cx, |state, _| {
            state.set_editor_style(InputEditorStyle {
                foreground: colors.foreground.into(),
                muted_foreground: colors.muted_foreground.into(),
                background: colors.background.into(),
                border: colors.input.into(),
                selection: colors.ring.opacity(0.3).into(),
                caret: colors.foreground.into(),
                ..Default::default()
            });
        });
        let text = state.read(cx).value();
        let lines = text.split('\n').count();
        let counts = format!(
            "{lines} {} · {} characters",
            if lines == 1 { "line" } else { "lines" },
            text.chars().count()
        );
        let click = state.clone();
        let field = div()
            .w_full()
            .h(px(rows as f32 * LINE_H + 24.))
            .flex_shrink_0()
            .px(px(14.))
            .py(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_color(rgba(0x282828cc))
            .bg(rgba(0x00000059))
            .overflow_hidden()
            .text_size(px(13.))
            .line_height(px(LINE_H))
            .text_color(colors.foreground)
            .when(editor.spec.mono, |el| el.font_family("Menlo"))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                click.update(cx, |state, cx| state.focus(window, cx));
            })
            .child(Textarea::new(&state));
        let card = div()
            .id("admin.text.card")
            .relative()
            .w(px(width))
            .h(px(height))
            .rounded(px(24.))
            .border_1()
            .border_color(rgba(0xf5f5f733))
            .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(colors.foreground)
                    .child(editor.spec.title.clone()),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(colors.foreground.opacity(0.7))
                    .child(editor.spec.message.clone()),
            )
            .child(field)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .text_color(colors.foreground.opacity(0.7))
                            .child(counts),
                    )
                    .child(
                        button("admin.text.cancel", "Cancel", ButtonKind::Plain, cx).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.close_admin_dialog(window, cx)
                            }),
                        ),
                    )
                    .child(
                        button(
                            "admin.text.apply",
                            editor.spec.action.clone(),
                            ButtonKind::Primary,
                            cx,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.submit_text_editor(window, cx)
                        })),
                    ),
            );
        Some(
            div()
                .id("admin.text")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.close_admin_dialog(window, cx)
                }))
                .child(card),
        )
    }
}
