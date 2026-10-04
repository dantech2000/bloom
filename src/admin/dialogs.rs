// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dialogs of the dashboard that need more than a yes or a no: a form with
//! text fields ([`Prompt`]) and a value shown once with a Copy button
//! ([`Secret`]). The app holds the editing state of the fields, because a
//! page renders with `&Bloom` and cannot make an entity.

use std::rc::Rc;

use gpui_kit::{
    ClickEvent, ClipboardItem, Context, Div, Focusable as _, InteractiveElement as _,
    ParentElement as _, Stateful, StatefulInteractiveElement as _, Styled, Window, div, px, rgba,
};

use super::{ButtonKind, button};
use crate::{
    app::{Bloom, Page},
    ui::{glass::glass, input::Input, theme::UiTheme},
};

/// One text field of a [`Prompt`].
#[derive(Clone)]
pub struct Field {
    pub label: String,
    pub placeholder: String,
    /// The text the field starts with; empty for a new value.
    pub value: String,
    /// Shows dots for the text. A prompt can hold one plain field and two
    /// of these.
    pub masked: bool,
    /// The form does not submit while this field is empty.
    pub required: bool,
}

/// A form the user fills before an action runs.
#[derive(Clone)]
pub struct Prompt {
    pub title: String,
    pub message: String,
    /// Text of the button that runs the action, such as "Create key".
    pub action: String,
    pub fields: Vec<Field>,
    /// Gets the text of each field, in the order of `fields`.
    pub run: Rc<dyn Fn(&mut Bloom, Vec<String>, &mut Context<Bloom>)>,
}

/// A value the user sees once, such as a new API key.
#[derive(Clone)]
pub struct Secret {
    pub title: String,
    pub message: String,
    pub value: String,
}

impl Bloom {
    /// Shows a form; the action runs when the user submits it.
    pub fn ask_prompt(&mut self, prompt: Prompt, cx: &mut Context<Self>) {
        self.admin_prompt = Some(prompt);
        // The fields are cleared and focused in the next render, which has
        // the window.
        self.admin_prompt_fresh = true;
        cx.notify();
    }

    /// True while a dialog of the dashboard is open.
    pub fn admin_dialog_open(&self) -> bool {
        matches!(self.page, Page::Admin(_) | Page::Settings(_))
            && (self.admin_confirm.is_some()
                || self.admin_prompt.is_some()
                || self.admin_secret.is_some()
                || self.text_editor_open())
    }

    /// Closes the open dialog without an action.
    pub fn close_admin_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.admin_confirm = None;
        self.admin_prompt = None;
        self.admin_secret = None;
        self.close_text_editor();
        self.clear_admin_inputs(window, cx);
        window.focus(&self.app_focus, cx);
        cx.notify();
    }

    /// A password must not stay in a field that is out of view.
    fn clear_admin_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for input in &self.admin_inputs {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// The editing state of a field of the prompt: one plain and two masked
    /// exist, and a masked field takes them in order.
    fn admin_input(
        &self,
        prompt: &Prompt,
        index: usize,
    ) -> &gpui_kit::Entity<crate::ui::input::InputState> {
        let masked_before = prompt.fields[..index].iter().filter(|f| f.masked).count();
        match prompt.fields[index].masked {
            false => &self.admin_inputs[0],
            true => &self.admin_inputs[1 + masked_before.min(1)],
        }
    }

    /// Makes a prompt that just opened ready: the fields with their start
    /// text and placeholders, and the focus in the first one. The root render calls
    /// this.
    pub fn prepare_admin_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.admin_prompt_fresh) {
            return;
        }
        let Some(prompt) = self.admin_prompt.clone() else {
            return;
        };
        for (index, field) in prompt.fields.iter().enumerate() {
            self.admin_input(&prompt, index).clone().update(cx, |input, cx| {
                input.set_value(field.value.clone(), window, cx);
                input.set_placeholder(field.placeholder.clone(), window, cx);
            });
        }
        if !prompt.fields.is_empty() {
            let focus = self.admin_input(&prompt, 0).read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    /// Moves the focus to the next field of the prompt (Tab).
    pub fn next_admin_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = &self.admin_prompt else {
            return;
        };
        let current = (0..prompt.fields.len()).position(|index| {
            self.admin_input(prompt, index)
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        });
        let next = current.map_or(0, |i| (i + 1) % prompt.fields.len().max(1));
        if next < prompt.fields.len() {
            let focus = self.admin_input(prompt, next).read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    /// Runs the action of the prompt with the text of its fields. Does
    /// nothing while a required field is empty.
    pub fn submit_admin_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.admin_prompt.clone() else {
            return;
        };
        let values: Vec<String> = prompt
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let value = self.admin_input(&prompt, index).read(cx).value().to_string();
                if field.masked { value } else { value.trim().to_string() }
            })
            .collect();
        let missing = prompt
            .fields
            .iter()
            .zip(&values)
            .position(|(field, value)| field.required && value.is_empty());
        if let Some(index) = missing {
            let focus = self.admin_input(&prompt, index).read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            return;
        }
        self.close_admin_dialog(window, cx);
        (prompt.run)(self, values, cx);
    }

    /// The form dialog over the page.
    pub(crate) fn render_prompt(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let prompt = self.admin_prompt.clone()?;
        let t = UiTheme::read(cx).clone();
        let mut fields = div().flex().flex_col().gap(px(12.));
        for (index, field) in prompt.fields.iter().enumerate() {
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(t.colors.foreground.opacity(0.7))
                            .child(field.label.clone()),
                    )
                    .child(
                        Input::new(self.admin_input(&prompt, index))
                            .aria_label(field.label.clone())
                            .w_full(),
                    ),
            );
        }
        Some(
            dialog("admin.prompt", &prompt.title, &prompt.message, cx)
                .child(fields)
                .child(
                    div()
                        .mt(px(8.))
                        .flex()
                        .justify_end()
                        .gap(px(10.))
                        .child(
                            button("admin.prompt.cancel", "Cancel", ButtonKind::Plain, cx).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.close_admin_dialog(window, cx)
                                }),
                            ),
                        )
                        .child(
                            button(
                                "admin.prompt.run",
                                prompt.action.clone(),
                                ButtonKind::Primary,
                                cx,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit_admin_prompt(window, cx)
                            })),
                        ),
                ),
        )
        .map(|card| backdrop("admin.prompt.back", card, cx))
    }

    /// The dialog that shows a value once.
    pub(super) fn render_secret(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let secret = self.admin_secret.clone()?;
        let t = UiTheme::read(cx).clone();
        let value = secret.value.clone();
        Some(
            dialog("admin.secret", &secret.title, &secret.message, cx)
                .child(
                    div()
                        .px(px(12.))
                        .py(px(10.))
                        .rounded(px(10.))
                        .bg(rgba(0x00000052))
                        .font_family("Menlo")
                        .text_size(px(13.))
                        .text_color(t.colors.foreground)
                        .child(secret.value.clone()),
                )
                .child(
                    div()
                        .mt(px(8.))
                        .flex()
                        .justify_end()
                        .gap(px(10.))
                        .child(
                            button("admin.secret.copy", "Copy", ButtonKind::Plain, cx).on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        value.clone(),
                                    ));
                                    this.toast("Copied", "", cx);
                                }),
                            ),
                        )
                        .child(
                            button("admin.secret.done", "Done", ButtonKind::Primary, cx).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.close_admin_dialog(window, cx)
                                }),
                            ),
                        ),
                ),
        )
        .map(|card| backdrop("admin.secret.back", card, cx))
    }
}

/// Dark layer over the page with a dialog in its middle. A click beside the
/// dialog closes it.
fn backdrop(id: &'static str, card: Stateful<Div>, cx: &mut Context<Bloom>) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x00000099))
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.close_admin_dialog(window, cx)
        }))
        .child(card)
}

/// The glass card of a dialog with its title and message.
fn dialog(id: &'static str, title: &str, message: &str, cx: &Context<Bloom>) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    div()
        .id(id)
        .relative()
        .w(px(440.))
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
