#![cfg(target_os = "ios")]
//! The iOS platform of GPUI: a UIKit window with a Metal layer, touch
//! input handed to GPUI's gesture recognizer, CoreText for text, Grand
//! Central Dispatch for tasks, and a C bridge for the app delegate.
//!
//! After `longbridge/gpui-mobile` (commit 73ebf86), with the Apple Metal
//! renderer of `gpui_apple` in place of wgpu. UIKit owns the run loop: the
//! app is started with [`ffi::run_app`], which keeps the application alive
//! for the life of the process, and a `CADisplayLink` of the window asks
//! GPUI for a frame on each tick of the display.

pub(crate) mod cg_types;
mod dispatcher;
mod display;
mod events;
pub mod ffi;
mod platform;
mod text_input;
mod text_system;
pub mod util;
mod window;

pub(crate) use dispatcher::*;
pub(crate) use display::*;
pub use platform::*;
pub(crate) use text_system::*;
pub use window::{set_landscape, set_status_bar_style, video_layer};
pub(crate) use window::*;

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The platform for `Application::with_platform`.
pub fn current_platform(_headless: bool) -> std::rc::Rc<dyn gpui::Platform> {
    std::rc::Rc::new(IosPlatform::new())
}

// ── Status bar ───────────────────────────────────────────────────────────────

/// The colour of the text and icons of the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusBarContentStyle {
    /// White text and icons, for a dark page.
    #[default]
    Light,
    /// Dark text and icons, for a light page.
    Dark,
}

// ── Text input callback ──────────────────────────────────────────────────────

type TextInputCallbackFn = Box<dyn FnMut(&str)>;

/// Set when the keyboard sent text or its height changed: the next frame
/// is drawn even if GPUI has nothing dirty, so the text reaches the page.
pub static TEXT_INPUT_DIRTY: AtomicBool = AtomicBool::new(false);

thread_local! {
    static TEXT_INPUT_CALLBACK: RefCell<Option<TextInputCallbackFn>> = const { RefCell::new(None) };
}

/// A callback for the text of the software keyboard, ahead of GPUI's
/// input handler. One at a time; `None` clears it.
pub fn set_text_input_callback(callback: Option<TextInputCallbackFn>) {
    TEXT_INPUT_CALLBACK.with(|cb| {
        *cb.borrow_mut() = callback;
    });
}

/// Hands text from the keyboard to the callback; true when one took it.
pub fn dispatch_text_input(text: &str) -> bool {
    TEXT_INPUT_CALLBACK.with(|cb| {
        if let Some(callback) = cb.borrow_mut().as_mut() {
            callback(text);
            TEXT_INPUT_DIRTY.store(true, Ordering::Release);
            true
        } else {
            false
        }
    })
}

// ── Software keyboard ────────────────────────────────────────────────────────

/// The keyboard to show (`UIKeyboardType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyboardType {
    #[default]
    Default,
    EmailAddress,
    Phone,
    NumberPad,
    URL,
    Decimal,
}

/// Shows the software keyboard with the default layout.
pub fn show_keyboard() {
    show_keyboard_with_type(KeyboardType::Default);
}

/// Shows the software keyboard with the given layout.
pub fn show_keyboard_with_type(keyboard_type: KeyboardType) {
    if let Some(window) = ffi::last_window() {
        window.show_keyboard_with_type(keyboard_type);
    }
}

/// Shows a web page in a sheet inside the app (`SFSafariViewController`:
/// Safari's engine with a Done button, so a trailer plays without leaving
/// the app). A page that is not http(s) is refused by the caller.
pub fn present_web(url: &str) {
    let Some(window) = ffi::last_window() else { return };
    window.present_web(url);
}

/// Hides the software keyboard.
pub fn hide_keyboard() {
    if let Some(window) = ffi::last_window() {
        window.hide_keyboard();
    }
}

// ── The traits of the focused field ─────────────────────────────────────────

/// What the keyboard offers for the field that has the focus
/// (`UITextInputTraits` of the composition view): the keyboard layout,
/// the label of the return key, the kind of content (which lets the
/// system fill a saved password or a one-time code), autocorrection with
/// the predictive bar, capitalization, and secure entry. The app sets them
/// (`set_text_traits`) before the keyboard shows, and again when the
/// focus moves to another field; the keyboard reloads then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextTraits {
    pub keyboard: KeyboardType,
    pub return_key: ReturnKey,
    pub content: TextContent,
    /// Autocorrection and the predictive bar (QuickType). Off for a name,
    /// an address or a password.
    pub autocorrect: bool,
    pub capitalize: Capitalize,
    /// A password: no predictions, and the keyboard offers the passwords
    /// the system keeps.
    pub secure: bool,
}

impl TextTraits {
    pub const DEFAULT: TextTraits = TextTraits {
        keyboard: KeyboardType::Default,
        return_key: ReturnKey::Default,
        content: TextContent::None,
        autocorrect: true,
        capitalize: Capitalize::Sentences,
        secure: false,
    };
}

impl Default for TextTraits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The label of the return key (`UIReturnKeyType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReturnKey {
    #[default]
    Default,
    Go,
    Next,
    Search,
    Done,
    Send,
}

/// The kind of content of a field (`UITextContentType`): the system fills
/// a username, a password or a code from what it keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextContent {
    #[default]
    None,
    Username,
    Password,
    NewPassword,
    Url,
    Email,
    OneTimeCode,
    Name,
}

/// Automatic capitalization (`UITextAutocapitalizationType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Capitalize {
    #[default]
    None,
    Words,
    Sentences,
}

static TEXT_TRAITS: Mutex<TextTraits> = Mutex::new(TextTraits::DEFAULT);

/// Sets the traits of the field that has the focus; the keyboard takes
/// them when it shows, or at once while it is up.
pub fn set_text_traits(traits: TextTraits) {
    let changed = {
        let mut current = TEXT_TRAITS.lock().unwrap_or_else(|e| e.into_inner());
        if *current == traits {
            false
        } else {
            *current = traits;
            true
        }
    };
    if changed {
        if let Some(window) = ffi::last_window() {
            window.apply_text_traits();
        }
    }
}

pub(crate) fn text_traits() -> TextTraits {
    *TEXT_TRAITS.lock().unwrap_or_else(|e| e.into_inner())
}

// ── The edit menu of a field ────────────────────────────────────────────────

/// An entry of the edit menu of a text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// The edit menu to show over a field: its bounds in points of the
/// window (the menu points at them), and which entries it offers.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditMenu {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub cut: bool,
    pub copy: bool,
    pub paste: bool,
    pub select_all: bool,
}

/// Whether the pasteboard holds text, without reading it: a read asks the
/// user ("Bloom would like to paste from…"), and the menu must not; the
/// question belongs to the moment Paste is chosen.
pub fn clipboard_has_text() -> bool {
    unsafe {
        let pasteboard: *mut objc2::runtime::AnyObject =
            objc2::msg_send![objc2::class!(UIPasteboard), generalPasteboard];
        let has: objc2::runtime::Bool = objc2::msg_send![pasteboard, hasStrings];
        has.as_bool()
    }
}

/// Shows the system's edit menu (`UIEditMenuInteraction`) over a field;
/// `on_action` gets the entry the user chose. The app draws the field, so
/// UIKit's own menu of the composition view never shows: this one does.
pub fn present_edit_menu(menu: EditMenu, on_action: Box<dyn FnMut(EditAction)>) {
    if let Some(window) = ffi::last_window() {
        window.present_edit_menu(menu, on_action);
    }
}

/// The height of the software keyboard in points, as `f32` bits; 0 when
/// it is hidden. The height at the end of its slide: UIKit announces the
/// end frame before the keyboard moves.
pub static KEYBOARD_HEIGHT_BITS: AtomicU32 = AtomicU32::new(0);

/// The slide of the keyboard: the height it left, when, and how long UIKit
/// said the slide takes, so a page can move with the keyboard instead of
/// jumping ahead of it.
struct KeyboardSlide {
    from: f32,
    start: Instant,
    duration: Duration,
}
static KEYBOARD_SLIDE: Mutex<Option<KeyboardSlide>> = Mutex::new(None);

/// The height of the software keyboard in points at the end of its slide,
/// 0 when it is hidden.
pub fn keyboard_height() -> f32 {
    f32::from_bits(KEYBOARD_HEIGHT_BITS.load(Ordering::Relaxed))
}

/// The height the keyboard covers at this instant, eased along its slide,
/// and whether the slide goes on (a page that moves with it then asks for
/// another frame).
pub fn keyboard_height_now() -> (f32, bool) {
    let to = keyboard_height();
    let slide = KEYBOARD_SLIDE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(slide) = slide.as_ref() else { return (to, false) };
    let t = slide.start.elapsed().as_secs_f32() / slide.duration.as_secs_f32().max(0.001);
    if t >= 1. {
        return (to, false);
    }
    // Ease out, like UIKit's keyboard curve: quick at first, settling.
    let eased = 1. - (1. - t).powi(3);
    (slide.from + (to - slide.from) * eased, true)
}

/// The end height of the keyboard and the seconds its slide takes, from
/// a `UIKeyboardWillShow` or `WillHide` notification.
pub(crate) fn set_keyboard_height(height: f32, duration: f64) {
    let prev = f32::from_bits(KEYBOARD_HEIGHT_BITS.load(Ordering::Relaxed));
    if (prev - height).abs() > 0.5 {
        let (from, _) = keyboard_height_now();
        let slide = KeyboardSlide {
            from,
            start: Instant::now(),
            duration: Duration::from_secs_f64(duration.clamp(0., 1.)),
        };
        *KEYBOARD_SLIDE.lock().unwrap_or_else(|e| e.into_inner()) = Some(slide);
        KEYBOARD_HEIGHT_BITS.store(height.to_bits(), Ordering::Release);
        TEXT_INPUT_DIRTY.store(true, Ordering::Release);
    }
}

// ── Safe area ────────────────────────────────────────────────────────────────

/// The accessibility tree of the window as JSON (see `IosWindow::a11y_json`);
/// empty without a window or without `BLOOM_A11Y_TREE`.
pub fn a11y_json() -> String {
    ffi::last_window()
        .map(|window| window.a11y_json())
        .unwrap_or_else(|| "{}".to_string())
}

/// The safe area insets of the window in points: top, bottom, left, right.
pub fn safe_area_insets() -> (f32, f32, f32, f32) {
    ffi::last_window()
        .map(|window| window.safe_area_insets())
        .unwrap_or((0.0, 0.0, 0.0, 0.0))
}
