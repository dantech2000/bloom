// SPDX-License-Identifier: AGPL-3.0-or-later
//! The system route picker (`AVRoutePickerView` of AVKit) in the window.
//! macOS has no public call that chooses an AirPlay route; the user picks
//! one from this button's menu, and macOS pairs with the receiver and
//! moves the player's item there. The view sits above the gpui surface at
//! a rectangle the panel gives, and goes away with [`hide`].

use std::sync::Mutex;

use gpui_kit::Window;

use crate::macos::{Id, class, send};

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

/// The view that is in the window now, as an address.
static VIEW: Mutex<usize> = Mutex::new(0);

/// A rectangle in the window, from its top left: x, y, width, height.
pub type Place = [f32; 4];

/// Puts the picker at `place` in the window, for the player. Replaces the
/// one shown before. Main thread only, as every view call.
pub fn show(window: &Window, player: Id, place: Place) -> bool {
    hide();
    let Some(ns_window) = crate::pip::ns_window(window) else {
        return false;
    };
    let content = send!(Id, ns_window, c"contentView");
    if content.is_null() {
        return false;
    }
    // AppKit counts from the bottom left.
    let height = send!(Rect, content, c"frame").size.height;
    let [x, y, w, h] = place.map(f64::from);
    let frame = Rect { x, y: height - y - h, size: Size { width: w, height: h } };
    let view = send!(Id, class(c"AVRoutePickerView"), c"alloc");
    let view = send!(Id, view, c"initWithFrame:", frame => Rect);
    if view.is_null() {
        return false;
    }
    send!((), view, c"setRoutePickerButtonBordered:", false => bool);
    // The button takes the ink of the app in its normal states
    // (AVRoutePickerViewButtonStateNormal, NormalHighlighted).
    let ink = send!(Id, class(c"NSColor"), c"whiteColor");
    for state in 0..2_isize {
        send!((), view, c"setRoutePickerButtonColor:forState:", ink => Id, state => isize);
    }
    if !player.is_null() {
        send!((), view, c"setPlayer:", player => Id);
    }
    send!((), content, c"addSubview:", view => Id);
    *VIEW.lock().unwrap() = view as usize;
    true
}

/// Takes the picker out of the window.
pub fn hide() {
    let view = std::mem::replace(&mut *VIEW.lock().unwrap(), 0) as Id;
    if view.is_null() {
        return;
    }
    send!((), view, c"removeFromSuperview");
    send!((), view, c"release");
}

pub fn shown() -> bool {
    *VIEW.lock().unwrap() != 0
}
