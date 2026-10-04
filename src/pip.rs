// SPDX-License-Identifier: AGPL-3.0-or-later
//! Picture in picture: the player window made small, kept above other
//! windows, and placed at a screen corner. gpui has no calls for the window
//! level or frame, so this talks to the `NSWindow` through the Objective-C
//! runtime.

use gpui_kit::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::macos::{Id, send};

/// Size of the small window when no place is saved.
const DEFAULT_SIZE: (f64, f64) = (480., 270.);
/// Smallest size the small window may be dragged to.
const MIN_SIZE: (f64, f64) = (280., 158.);
/// Space between the small window and the screen edges.
const MARGIN: f64 = 24.;
/// Smallest size of the normal window, as set when it is created.
const NORMAL_MIN_SIZE: (f64, f64) = (900., 600.);

/// A window frame in Cocoa screen coordinates (origin at the bottom left):
/// x, y, width, height.
pub type Frame = [f64; 4];

#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    size: Size,
}

/// Place of the close button in the window, from the top left. The window
/// is made with it, and the player puts the buttons back at it.
pub const TRAFFIC_LIGHTS: (f32, f32) = (18., 25.);

/// The `NSWindow` behind a gpui window.
pub(crate) fn ns_window(window: &Window) -> Option<Id> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    let view = handle.ns_view.as_ptr();
    let window = send!(Id, view, c"window");
    (!window.is_null()).then_some(window)
}

fn frame_of(window: Id) -> Frame {
    let rect = send!(Rect, window, c"frame");
    [rect.x, rect.y, rect.size.width, rect.size.height]
}

fn set_frame(window: Id, frame: Frame) {
    let rect = Rect {
        x: frame[0],
        y: frame[1],
        size: Size {
            width: frame[2],
            height: frame[3],
        },
    };
    send!((), window, c"setFrame:display:animate:", rect => Rect, true => bool, true => bool);
}

/// Shows or hides the close, minimise and zoom buttons.
fn set_buttons_hidden(window: Id, hidden: bool) {
    for kind in 0..3_usize {
        let button = send!(Id, window, c"standardWindowButton:", kind => usize);
        if !button.is_null() {
            send!((), button, c"setHidden:", hidden => bool);
        }
    }
}

/// Shows or hides the close, minimise and zoom buttons of the window. The
/// player hides them together with its controls.
/// They become transparent and are not hidden. AppKit lays the title bar
/// out again on this change and puts the buttons at its default place, so
/// they go back to [`TRAFFIC_LIGHTS`] after it, the way gpui places them.
pub fn set_window_buttons_hidden(window: &Window, hidden: bool) {
    let Some(window) = ns_window(window) else {
        return;
    };
    let alpha: f64 = if hidden { 0. } else { 1. };
    let buttons: Vec<Id> = (0..3_usize)
        .map(|kind| send!(Id, window, c"standardWindowButton:", kind => usize))
        .filter(|button| !button.is_null())
        .collect();
    for button in &buttons {
        send!((), *button, c"setAlphaValue:", alpha => f64);
    }
    crate::macos::after_event(move || place_buttons(window, &buttons));
}

/// Puts the window buttons at [`TRAFFIC_LIGHTS`].
fn place_buttons(window: Id, buttons: &[Id]) {
    // NSWindowStyleMaskFullScreen: a full-screen window keeps AppKit's layout.
    let full_screen = send!(usize, window, c"styleMask") & (1 << 14) != 0;
    let [close, minimize, _] = buttons else {
        return;
    };
    let holder = send!(Id, *close, c"superview");
    let container = send!(Id, holder, c"superview");
    if full_screen || container.is_null() {
        return;
    }
    let (x, y) = (TRAFFIC_LIGHTS.0 as f64, TRAFFIC_LIGHTS.1 as f64);
    let close_frame = send!(Rect, *close, c"frame");
    let step = send!(Rect, *minimize, c"frame").x - close_frame.x;
    let height = close_frame.size.height + y + y;
    let mut frame = send!(Rect, container, c"frame");
    frame.y = send!(Rect, window, c"frame").size.height - height;
    frame.size.height = height;
    send!((), container, c"setFrame:", frame => Rect);
    for (i, button) in buttons.iter().enumerate() {
        let mut frame = send!(Rect, *button, c"frame");
        frame.x = x + step * i as f64;
        frame.y = y;
        send!((), *button, c"setFrame:", frame => Rect);
    }
}

/// True when the frame lies on the screen the window is on.
fn on_screen(window: Id, frame: Frame) -> bool {
    let screen = send!(Id, window, c"screen");
    if screen.is_null() {
        return false;
    }
    let visible = send!(Rect, screen, c"visibleFrame");
    frame[0] >= visible.x - 1.
        && frame[1] >= visible.y - 1.
        && frame[0] + frame[2] <= visible.x + visible.size.width + 1.
        && frame[1] + frame[3] <= visible.y + visible.size.height + 1.
}

/// Current frame of the window.
pub fn frame(window: &Window) -> Option<Frame> {
    ns_window(window).map(frame_of)
}

/// Makes the window small and keeps it above other windows. `saved` is the
/// place the user left the small window at; without one it goes to the
/// bottom right of the screen. Returns the frame the window had, for
/// [`leave`].
pub fn enter(window: &Window, saved: Option<Frame>) -> Option<Frame> {
    let ns = ns_window(window)?;
    let normal = frame_of(ns);
    let target = saved.filter(|frame| on_screen(ns, *frame)).or_else(|| {
        let screen = send!(Id, ns, c"screen");
        if screen.is_null() {
            return None;
        }
        let visible = send!(Rect, screen, c"visibleFrame");
        Some([
            visible.x + visible.size.width - DEFAULT_SIZE.0 - MARGIN,
            visible.y + MARGIN,
            DEFAULT_SIZE.0,
            DEFAULT_SIZE.1,
        ])
    })?;
    let min = Size {
        width: MIN_SIZE.0,
        height: MIN_SIZE.1,
    };
    // NSFloatingWindowLevel
    send!((), ns, c"setLevel:", 3_isize => isize);
    set_buttons_hidden(ns, true);
    // The caller is inside a gpui update, where gpui cannot take the news
    // of the new size; the resize waits until the update is over.
    crate::macos::after_event(move || {
        send!((), ns, c"setContentMinSize:", min => Size);
        set_frame(ns, target);
    });
    Some(normal)
}

/// Returns the window to its normal level, size and place.
pub fn leave(window: &Window, normal: Frame) {
    let Some(ns) = ns_window(window) else {
        return;
    };
    // NSNormalWindowLevel
    send!((), ns, c"setLevel:", 0_isize => isize);
    set_buttons_hidden(ns, false);
    let min = Size {
        width: NORMAL_MIN_SIZE.0,
        height: NORMAL_MIN_SIZE.1,
    };
    crate::macos::after_event(move || {
        set_frame(ns, normal);
        send!((), ns, c"setContentMinSize:", min => Size);
    });
}
