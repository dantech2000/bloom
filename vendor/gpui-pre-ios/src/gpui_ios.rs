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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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

/// Hides the software keyboard.
pub fn hide_keyboard() {
    if let Some(window) = ffi::last_window() {
        window.hide_keyboard();
    }
}

/// The height of the software keyboard in points, as `f32` bits; 0 when
/// it is hidden.
pub static KEYBOARD_HEIGHT_BITS: AtomicU32 = AtomicU32::new(0);

/// The height of the software keyboard in points, 0 when it is hidden.
pub fn keyboard_height() -> f32 {
    f32::from_bits(KEYBOARD_HEIGHT_BITS.load(Ordering::Relaxed))
}

pub(crate) fn set_keyboard_height(height: f32) {
    let prev = f32::from_bits(KEYBOARD_HEIGHT_BITS.load(Ordering::Relaxed));
    if (prev - height).abs() > 0.5 {
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
