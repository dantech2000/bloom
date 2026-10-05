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
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

/// An autorelease pool for one turn of a loop on a thread of our own.
/// Without one, what the system autoreleases on the thread (AppKit,
/// CoreVideo, the GL driver) lives until the thread ends.
pub struct Pool(*mut c_void);

impl Pool {
    pub fn new() -> Self {
        Self(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
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

/// Opens a link that came from the server, in the browser. Only `http` and
/// `https` links go; anything else (`file:`, `javascript:`, a custom scheme
/// that starts an app) is logged and ignored.
pub fn open_web_url(cx: &mut gpui_kit::App, url: &str) {
    if is_web_url(url) {
        cx.open_url(url);
    } else {
        // The query can hold a token; keep it out of the log.
        log::warn!("link not opened, not http(s): {:?}", url.split('?').next().unwrap_or_default());
    }
}

/// An `http://` or `https://` link with something after the scheme. The
/// scheme is not case sensitive; a space before it is not allowed.
fn is_web_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && !rest.is_empty()
        && !url.contains(|c: char| c.is_control())
}

/// Does the locale of this Mac use a 12-hour clock? It asks for the pattern
/// of the "j" template (the hour in the way of the user): a pattern with an
/// "a" has an AM/PM mark. The answer is kept; a change of the setting shows
/// after a restart.
fn clock_is_12_hour() -> bool {
    static TWELVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TWELVE.get_or_init(|| uses_12_hour(send!(Id, class(c"NSLocale"), c"currentLocale")))
}

fn uses_12_hour(locale: Id) -> bool {
    {
        let template = send!(Id, class(c"NSString"), c"stringWithUTF8String:", c"j".as_ptr() => *const std::ffi::c_char);
        let pattern = send!(
            Id, class(c"NSDateFormatter"), c"dateFormatFromTemplate:options:locale:",
            template => Id, 0usize => usize, locale => Id
        );
        if pattern.is_null() {
            return true;
        }
        let utf8 = send!(*const std::ffi::c_char, pattern, c"UTF8String");
        !utf8.is_null() && unsafe { CStr::from_ptr(utf8) }.to_string_lossy().contains('a')
    }
}

fn format_clock(time: jiff::civil::Time, twelve_hour: bool) -> String {
    if twelve_hour { time.strftime("%-I:%M %p") } else { time.strftime("%H:%M") }.to_string()
}

/// Local clock time `secs` from now, as "3:16 AM" or "15:16" by the setting
/// of the Mac. The pages and the player show it as "Ends at ...".
pub fn ends_at(secs: i64) -> String {
    let end = jiff::Zoned::now().saturating_add(jiff::SignedDuration::from_secs(secs));
    format_clock(end.time(), clock_is_12_hour())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_links_open() {
        for ok in ["http://a.example/x", "https://youtu.be/abc?t=1", "HTTPS://A.EXAMPLE/", "Http://a"] {
            assert!(is_web_url(ok), "{ok}");
        }
        for bad in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "javascript://%0Aalert(1)",
            "vnc://host",
            "ssh://host",
            "x-apple.systempreferences:com.apple.preference",
            " https://a.example",
            "\thttps://a.example",
            "https:/a.example",
            "https://",
            "ftp://a.example",
            "httpx://a",
            "https://a.example/\nx",
            "data:text/html,hi",
        ] {
            assert!(!is_web_url(bad), "{bad:?}");
        }
    }

    #[test]
    fn clock_formats_for_both_modes() {
        let t = |h, m| jiff::civil::time(h, m, 0, 0);
        assert_eq!(format_clock(t(15, 7), false), "15:07");
        assert_eq!(format_clock(t(15, 7), true), "3:07 PM");
        assert_eq!(format_clock(t(0, 5), false), "00:05");
        assert_eq!(format_clock(t(0, 5), true), "12:05 AM");
        assert_eq!(format_clock(t(12, 0), true), "12:00 PM");
        assert_eq!(format_clock(t(9, 30), true), "9:30 AM");
    }

    #[test]
    fn the_setting_of_this_mac_is_read() {
        // Not a fixed answer: it depends on the Mac. It must not crash and
        // must be the same twice.
        assert_eq!(clock_is_12_hour(), clock_is_12_hour());
        // The same call for fixed locales, to see both answers.
        let locale = |name: &CStr| {
            let name = send!(Id, class(c"NSString"), c"stringWithUTF8String:", name.as_ptr() => *const std::ffi::c_char);
            let locale = send!(Id, class(c"NSLocale"), c"alloc");
            send!(Id, locale, c"initWithLocaleIdentifier:", name => Id)
        };
        assert!(uses_12_hour(locale(c"en_US")));
        assert!(!uses_12_hour(locale(c"en_GB")));
        assert!(!uses_12_hour(locale(c"de_DE")));
        let shown = ends_at(3600);
        assert!(shown.contains(':'), "{shown}");
        eprintln!("12-hour: {}, now+1h: {shown}", clock_is_12_hour());
    }
}
