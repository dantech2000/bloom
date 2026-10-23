//! Nova presentation for GPUI Kit's single-line editing state.

use super::theme::{ThemeMode, UiTheme};
pub use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::base::{InputBase, input::InputEditorStyle};
use gpui_kit::{
    AccessibleAction, App, ElementId, Entity, EntityInputHandler as _, Focusable as _,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, prelude::FluentBuilder as _,
};

/// What the keyboard of a phone offers for a field (the traits of UIKit;
/// the Mac has no use for them): the layout, the label of the return key,
/// the kind of content (the system fills a saved username, password or
/// code from it), autocorrection with the predictive bar, capitalization
/// and secure entry. A field names its kind, as `Keyboard::url()` for a
/// server address; a field that names none gets `Keyboard::text()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keyboard {
    pub kind: KeyboardKind,
    pub return_key: ReturnKey,
    pub content: Content,
    pub autocorrect: bool,
    pub capitalize: Capitalize,
    pub secure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KeyboardKind {
    #[default]
    Default,
    Url,
    Email,
    Numbers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReturnKey {
    #[default]
    Default,
    Go,
    Next,
    Search,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Content {
    #[default]
    None,
    Username,
    Password,
    Url,
    OneTimeCode,
    Name,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Capitalize {
    #[default]
    None,
    Words,
    Sentences,
}

impl Keyboard {
    /// Prose: correction, predictions and capitals at a sentence's start.
    pub const fn text() -> Self {
        Self {
            kind: KeyboardKind::Default,
            return_key: ReturnKey::Default,
            content: Content::None,
            autocorrect: true,
            capitalize: Capitalize::Sentences,
            secure: false,
        }
    }
    /// A name of something (a playlist, a user): a capital on each word,
    /// Done on the return key.
    pub const fn name() -> Self {
        Self { capitalize: Capitalize::Words, return_key: ReturnKey::Done, content: Content::Name, ..Self::text() }
    }
    /// A search: predictions, no capitals, Search on the return key.
    pub const fn search() -> Self {
        Self { capitalize: Capitalize::None, return_key: ReturnKey::Search, ..Self::text() }
    }
    /// A server address: the URL keyboard, nothing corrected, Go.
    pub const fn url() -> Self {
        Self {
            kind: KeyboardKind::Url,
            return_key: ReturnKey::Go,
            content: Content::Url,
            autocorrect: false,
            capitalize: Capitalize::None,
            secure: false,
        }
    }
    /// A username: nothing corrected and no capitals, Next. Autocorrection
    /// (which the predictive bar comes with) turned "dro" into "drop" in
    /// the simulator; a name typed that way reaches the server wrong
    /// (2026-10-22). Not typed as a user name either: iOS then puts the
    /// Passwords key over it (Apple's "Enabling Password AutoFill on a
    /// text input view": the username type lets "the user tap and fill"
    /// it), which Daniel does not want there. The password field offers
    /// the passwords.
    pub const fn username() -> Self {
        Self {
            kind: KeyboardKind::Default,
            return_key: ReturnKey::Next,
            content: Content::None,
            autocorrect: false,
            capitalize: Capitalize::None,
            secure: false,
        }
    }
    /// A password: secure entry, the saved passwords offered, Go.
    pub const fn password() -> Self {
        Self { kind: KeyboardKind::Default, content: Content::Password, secure: true, ..Self::url() }
    }
    /// A code of digits, as Quick Connect's: the number pad, and a code
    /// that arrived in a message is offered.
    pub const fn code() -> Self {
        Self { kind: KeyboardKind::Numbers, content: Content::OneTimeCode, ..Self::url() }
    }
    /// One line of plain text with Done on the return key, no capitals.
    pub const fn plain() -> Self {
        Self { capitalize: Capitalize::None, return_key: ReturnKey::Done, ..Self::text() }
    }
}

/// The traits of the field that has the focus go to the platform, which
/// gives them to the keyboard before it shows (iOS; nothing elsewhere).
fn apply_keyboard(keyboard: Keyboard) {
    #[cfg(target_os = "ios")]
    {
        use gpui_ios as ios;
        ios::set_text_traits(ios::TextTraits {
            keyboard: match keyboard.kind {
                KeyboardKind::Default => ios::KeyboardType::Default,
                KeyboardKind::Url => ios::KeyboardType::URL,
                KeyboardKind::Email => ios::KeyboardType::EmailAddress,
                KeyboardKind::Numbers => ios::KeyboardType::NumberPad,
            },
            return_key: match keyboard.return_key {
                ReturnKey::Default => ios::ReturnKey::Default,
                ReturnKey::Go => ios::ReturnKey::Go,
                ReturnKey::Next => ios::ReturnKey::Next,
                ReturnKey::Search => ios::ReturnKey::Search,
                ReturnKey::Done => ios::ReturnKey::Done,
            },
            content: match keyboard.content {
                Content::None => ios::TextContent::None,
                Content::Username => ios::TextContent::Username,
                Content::Password => ios::TextContent::Password,
                Content::Url => ios::TextContent::Url,
                Content::OneTimeCode => ios::TextContent::OneTimeCode,
                Content::Name => ios::TextContent::Name,
            },
            autocorrect: keyboard.autocorrect,
            capitalize: match keyboard.capitalize {
                Capitalize::None => ios::Capitalize::None,
                Capitalize::Words => ios::Capitalize::Words,
                Capitalize::Sentences => ios::Capitalize::Sentences,
            },
            secure: keyboard.secure,
        });
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = keyboard;
    }
}

/// The caller owns the editing state, focus and event subscriptions.
#[derive(IntoElement)]
pub struct Input {
    state: Entity<InputState>,
    style: gpui_kit::StyleRefinement,
    label: Option<SharedString>,
    disabled: bool,
    read_only: bool,
    invalid: bool,
    bordered: bool,
    keyboard: Keyboard,
}
impl Input {
    /// Creates a Nova input using the caller's retained Kit editing state.
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            style: Default::default(),
            label: None,
            disabled: false,
            read_only: false,
            invalid: false,
            bordered: true,
            keyboard: Keyboard::text(),
        }
    }
    /// What the keyboard of a phone offers for this field.
    pub fn keyboard(mut self, keyboard: Keyboard) -> Self {
        self.keyboard = keyboard;
        self
    }
    /// Sets the accessible name of the control.
    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
    /// Disables interaction and applies the disabled appearance.
    pub fn disabled(mut self, value: bool) -> Self {
        self.disabled = value;
        self
    }
    /// Prevents user changes while preserving focus and reading.
    pub fn read_only(mut self, value: bool) -> Self {
        self.read_only = value;
        self
    }
    /// Applies validation error colors and an invalid accessibility description.
    pub fn invalid(mut self, value: bool) -> Self {
        self.invalid = value;
        self
    }
    /// Controls the Nova input border and background.
    pub fn bordered(mut self, value: bool) -> Self {
        self.bordered = value;
        self
    }
}
impl Styled for Input {
    fn style(&mut self) -> &mut gpui_kit::StyleRefinement {
        &mut self.style
    }
}
impl RenderOnce for Input {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let colors = theme.colors;
        self.state.update(cx, |state, cx| {
            state.set_disabled(self.disabled, cx);
            state.set_readonly(self.read_only, cx);
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
        let state = self.state.read(cx);
        let focus = state.focus_handle(cx);
        let focused = focus.is_focused(window) && !self.disabled;
        if focused {
            apply_keyboard(self.keyboard);
        }
        let value = state.value();
        let placeholder = state.presentation().placeholder().clone();
        let action_state = self.state.clone();
        let click_state = self.state.clone();
        let a11y_focus = self.state.clone();
        let root = InputBase::new(ElementId::from(("input", self.state.entity_id())))
            .focused(focused)
            .disabled(self.disabled)
            .when_some(self.label, |root, label| root.accessibility_label(label))
            .aria_value(value)
            .aria_placeholder(placeholder)
            .when(self.read_only, |root| root.aria_description("Read only"))
            .when(self.invalid, |root| root.aria_description("Invalid value"))
            .when(!self.disabled, |root| {
                root.on_a11y_action(AccessibleAction::Focus, move |_, window, cx| {
                    a11y_focus.update(cx, |state, cx| state.focus(window, cx));
                })
            })
            .when(!self.disabled, |root| {
                root.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    click_state.update(cx, |state, cx| state.focus(window, cx));
                })
            })
            .when(!self.disabled && !self.read_only, |root| {
                root.on_a11y_action(AccessibleAction::SetValue, move |data, window, cx| {
                    if let Some(gpui_kit::accesskit::ActionData::Value(value)) = data {
                        action_state.update(cx, |state, cx| {
                            let end = state.value().encode_utf16().count();
                            state.replace_text_in_range(Some(0..end), value, window, cx);
                        });
                    }
                })
            })
            .flex()
            .w_full()
            .min_w_0()
            .h(theme.space(8.))
            .items_center()
            .px(theme.space(2.5))
            .font_family(theme.fonts.body.clone())
            .text_size(theme.text(14.))
            .line_height(theme.text(20.))
            .text_color(colors.foreground)
            .when(self.bordered, |root| {
                root.rounded(theme.radius.lg)
                    .border_1()
                    .border_color(if self.invalid {
                        colors.destructive
                    } else {
                        colors.input
                    })
                    .bg(match theme.mode {
                        ThemeMode::Light => colors.background.opacity(0.),
                        ThemeMode::Dark => colors.input.opacity(0.3),
                    })
            })
            .when(focused && self.bordered, |root| {
                root.border_color(if self.invalid {
                    colors.destructive
                } else {
                    colors.ring
                })
            })
            .when(self.disabled, |root| root.opacity(0.5).cursor_not_allowed())
            // A phone: a long press on the field opens the system's edit
            // menu (Cut, Copy, Paste, Select All); the Mac has the context
            // menu and the keys.
            .when(cfg!(target_os = "ios") && !self.disabled, |root| {
                root.relative().child(edit_menu_overlay(
                    self.state.clone(),
                    self.keyboard,
                    self.read_only,
                    window,
                    cx,
                ))
            })
            .child(self.state);
        super::theme::apply_style(root, &self.style)
    }
}

/// A layer over the field that takes a long press and shows the edit
/// menu for the field (iOS). The press must be claimed at its start
/// (`prevent_default`), or the release becomes a tap.
fn edit_menu_overlay(
    state: Entity<InputState>,
    keyboard: Keyboard,
    read_only: bool,
    window: &mut Window,
    cx: &mut App,
) -> gpui_kit::Canvas<()> {
    use gpui_kit::{Bounds, DispatchPhase, LongPressEvent, Pixels, TouchPhase, canvas};
    let async_window = window.to_async(cx);
    canvas(|_, _, _| {}, move |bounds: Bounds<Pixels>, _, window, _| {
        let state = state.clone();
        let async_window = async_window.clone();
        window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.phase != TouchPhase::Started
                || !bounds.contains(&event.start_position)
            {
                return;
            }
            window.prevent_default();
            state.update(cx, |state, cx| state.focus(window, cx));
            let (empty, selection) = {
                let state = state.read(cx);
                (state.value().is_empty(), !state.selected_value().is_empty())
            };
            // Asked without a read: a read of the pasteboard makes iOS ask
            // the user's leave, which belongs to the Paste itself.
            let clipboard = clipboard_has_text(cx);
            // A password is never copied out of its field.
            let menu = EditMenu {
                bounds,
                cut: selection && !keyboard.secure && !read_only,
                copy: selection && !keyboard.secure,
                paste: clipboard && !read_only,
                select_all: !empty,
            };
            present_edit_menu(menu, state.clone(), async_window.clone());
        });
    })
    .absolute()
    .inset_0()
}

/// Whether there is text to paste.
#[allow(unused_variables)]
fn clipboard_has_text(cx: &App) -> bool {
    #[cfg(target_os = "ios")]
    {
        gpui_ios::clipboard_has_text()
    }
    #[cfg(not(target_os = "ios"))]
    {
        cx.read_from_clipboard().and_then(|item| item.text()).is_some_and(|text| !text.is_empty())
    }
}

/// The edit menu of a field: where it points, and what it offers.
struct EditMenu {
    bounds: gpui_kit::Bounds<gpui_kit::Pixels>,
    cut: bool,
    copy: bool,
    paste: bool,
    select_all: bool,
}

/// Shows the platform's edit menu; the chosen entry acts on the field.
#[allow(unused_variables)]
fn present_edit_menu(menu: EditMenu, state: Entity<InputState>, async_window: gpui_kit::AsyncWindowContext) {
    #[cfg(target_os = "ios")]
    {
        let mut async_window = async_window;
        gpui_ios::present_edit_menu(
            gpui_ios::EditMenu {
                x: f32::from(menu.bounds.origin.x),
                y: f32::from(menu.bounds.origin.y),
                w: f32::from(menu.bounds.size.width),
                h: f32::from(menu.bounds.size.height),
                cut: menu.cut,
                copy: menu.copy,
                paste: menu.paste,
                select_all: menu.select_all,
            },
            Box::new(move |action| {
                let state = state.clone();
                let _ = async_window.update(|window, cx| {
                    state.update(cx, |state, cx| {
                        use gpui_ios::EditAction;
                        match action {
                            EditAction::Paste => {
                                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                                    state.replace_text_in_range(None, &text, window, cx);
                                }
                            }
                            EditAction::Copy => {
                                let text = state.selected_value().to_string();
                                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
                            }
                            EditAction::Cut => {
                                let text = state.selected_value().to_string();
                                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
                                state.replace_text_in_range(None, "", window, cx);
                            }
                            EditAction::SelectAll => state.select_all(window, cx),
                        }
                    });
                });
            }),
        );
    }
}
