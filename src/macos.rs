// SPDX-License-Identifier: AGPL-3.0-or-later
//! Calls into AppKit through the Objective-C runtime, for the few things gpui
//! has no call for: the window level and frame, and the Dock icon.

use std::ffi::{CStr, c_void};

pub type Id = *mut c_void;
pub type Sel = *const c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn sel_registerName(name: *const std::ffi::c_char) -> Sel;
    fn objc_getClass(name: *const std::ffi::c_char) -> Id;
    pub fn objc_msgSend();
}

pub fn sel(name: &CStr) -> Sel {
    unsafe { sel_registerName(name.as_ptr()) }
}

pub fn class(name: &CStr) -> Id {
    unsafe { objc_getClass(name.as_ptr()) }
}

// `objc_msgSend` takes the signature of the method it calls.
macro_rules! send {
    ($ret:ty, $obj:expr, $name:literal $(, $arg:expr => $ty:ty)*) => {{
        let function: unsafe extern "C" fn(
            $crate::macos::Id,
            $crate::macos::Sel
            $(, $ty)*
        ) -> $ret = unsafe {
            std::mem::transmute($crate::macos::objc_msgSend as unsafe extern "C" fn())
        };
        unsafe { function($obj, $crate::macos::sel($name) $(, $arg)*) }
    }};
}
pub(crate) use send;

/// Hides the pointer until the mouse moves, or gives it back.
pub fn hide_pointer_until_it_moves(hidden: bool) {
    send!((), class(c"NSCursor"), c"setHiddenUntilMouseMoves:", hidden => bool);
}

/// Shows an image as the app's icon in the Dock and the app switcher. An app
/// that runs outside a bundle has no icon file, so it is set at launch.
pub fn set_app_icon(png: &'static [u8]) {
    let data = send!(
        Id, class(c"NSData"), c"dataWithBytes:length:",
        png.as_ptr() => *const u8, png.len() => usize
    );
    let image = send!(Id, class(c"NSImage"), c"alloc");
    let image = send!(Id, image, c"initWithData:", data => Id);
    if image.is_null() {
        return;
    }
    let app = send!(Id, class(c"NSApplication"), c"sharedApplication");
    send!((), app, c"setApplicationIconImage:", image => Id);
}

#[allow(non_upper_case_globals)]
unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
}

/// Runs `work` on the main thread after the current event is handled, so
/// outside any gpui update. AppKit calls that make gpui call back into the
/// app (a window resize) need this: gpui drops such a callback while the app
/// state is in use.
pub fn after_event(work: impl FnOnce() + 'static) {
    extern "C" fn run(context: *mut c_void) {
        let work = unsafe { Box::from_raw(context as *mut Box<dyn FnOnce()>) };
        work();
    }
    let work: Box<Box<dyn FnOnce()>> = Box::new(Box::new(work));
    unsafe { dispatch_async_f(&raw const _dispatch_main_q, Box::into_raw(work) as *mut c_void, run) };
}
