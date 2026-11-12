// SPDX-License-Identifier: AGPL-3.0-or-later
//! Picture in picture: the player window made small, kept above other
//! windows, and placed at a screen corner. gpui has no calls for the window
//! level or frame, so this talks to the `NSWindow` through the Objective-C
//! runtime.

use gpui_kit::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::macos::{CGRect, CGSize, Id, send};

/// Size of the small window when no place is saved.
const DEFAULT_SIZE: (f64, f64) = (480., 270.);
/// Smallest size the small window may be dragged to.
const MIN_SIZE: (f64, f64) = (280., 158.);
/// Space between the small window and the screen edges.
const MARGIN: f64 = 24.;
/// Smallest size of the normal window: the window is created with it
/// (`open_main_window`), and gets it back when the small one ends.
pub const NORMAL_MIN_SIZE: (f32, f32) = (900., 600.);

/// A window frame in Cocoa screen coordinates (origin at the bottom left):
/// x, y, width, height.
pub type Frame = [f64; 4];

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
    let rect = send!(CGRect, window, c"frame");
    [rect.x, rect.y, rect.width, rect.height]
}

fn set_frame(window: Id, frame: Frame) {
    let rect = CGRect { x: frame[0], y: frame[1], width: frame[2], height: frame[3] };
    send!((), window, c"setFrame:display:animate:", rect => CGRect, true => bool, true => bool);
}

/// The close, minimise and zoom buttons of the window, those it has.
fn buttons(window: Id) -> Vec<Id> {
    (0..3_usize)
        .map(|kind| send!(Id, window, c"standardWindowButton:", kind => usize))
        .filter(|button| !button.is_null())
        .collect()
}

/// Shows or hides the close, minimise and zoom buttons.
fn set_buttons_hidden(window: Id, hidden: bool) {
    for button in buttons(window) {
        send!((), button, c"setHidden:", hidden => bool);
    }
}

/// Runs `work` with the window after the current event (see
/// `macos::after_event`). The window is kept until then: it can close
/// before the work runs.
fn after_event_with(window: Id, work: impl FnOnce(Id) + 'static) {
    let kept = send!(Id, window, c"retain");
    crate::macos::after_event(move || {
        work(kept);
        send!((), kept, c"release");
    });
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
    for button in buttons(window) {
        send!((), button, c"setAlphaValue:", alpha => f64);
    }
    after_event_with(window, |window| place_buttons(window, &buttons(window)));
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
    let close_frame = send!(CGRect, *close, c"frame");
    let step = send!(CGRect, *minimize, c"frame").x - close_frame.x;
    let height = close_frame.height + y + y;
    let mut frame = send!(CGRect, container, c"frame");
    frame.y = send!(CGRect, window, c"frame").height - height;
    frame.height = height;
    send!((), container, c"setFrame:", frame => CGRect);
    for (i, button) in buttons.iter().enumerate() {
        let mut frame = send!(CGRect, *button, c"frame");
        frame.x = x + step * i as f64;
        frame.y = y;
        send!((), *button, c"setFrame:", frame => CGRect);
    }
}

/// True when the frame lies on the screen the window is on.
fn on_screen(window: Id, frame: Frame) -> bool {
    let screen = send!(Id, window, c"screen");
    if screen.is_null() {
        return false;
    }
    let visible = send!(CGRect, screen, c"visibleFrame");
    frame[0] >= visible.x - 1.
        && frame[1] >= visible.y - 1.
        && frame[0] + frame[2] <= visible.x + visible.width + 1.
        && frame[1] + frame[3] <= visible.y + visible.height + 1.
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
        let visible = send!(CGRect, screen, c"visibleFrame");
        Some([
            visible.x + visible.width - DEFAULT_SIZE.0 - MARGIN,
            visible.y + MARGIN,
            DEFAULT_SIZE.0,
            DEFAULT_SIZE.1,
        ])
    })?;
    let min = CGSize { width: MIN_SIZE.0, height: MIN_SIZE.1 };
    // NSFloatingWindowLevel
    send!((), ns, c"setLevel:", 3_isize => isize);
    set_buttons_hidden(ns, true);
    // The caller is inside a gpui update, where gpui cannot take the news
    // of the new size; the resize waits until the update is over.
    after_event_with(ns, move |ns| {
        send!((), ns, c"setContentMinSize:", min => CGSize);
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
    let min = CGSize { width: f64::from(NORMAL_MIN_SIZE.0), height: f64::from(NORMAL_MIN_SIZE.1) };
    after_event_with(ns, move |ns| {
        set_frame(ns, normal);
        send!((), ns, c"setContentMinSize:", min => CGSize);
    });
}
