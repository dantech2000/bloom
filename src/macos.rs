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

fn ns_string(text: &CStr) -> Id {
    send!(Id, class(c"NSString"), c"stringWithUTF8String:", text.as_ptr() => *const std::ffi::c_char)
}

fn string_of(ns_string: Id) -> String {
    if ns_string.is_null() {
        return String::new();
    }
    let utf8 = send!(*const std::ffi::c_char, ns_string, c"UTF8String");
    if utf8.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(utf8) }.to_string_lossy().into_owned()
}

/// Sets the appearance of the app's own windows and panels: dark, light,
/// or the system's (`None`). The parts AppKit draws (the title bar and its
/// buttons, the open panel, the windows of the updater) follow it.
pub fn set_app_appearance(dark: Option<bool>) {
    let app = send!(Id, class(c"NSApplication"), c"sharedApplication");
    let appearance = match dark {
        Some(true) => send!(Id, class(c"NSAppearance"), c"appearanceNamed:", ns_string(c"NSAppearanceNameDarkAqua") => Id),
        Some(false) => send!(Id, class(c"NSAppearance"), c"appearanceNamed:", ns_string(c"NSAppearanceNameAqua") => Id),
        None => std::ptr::null_mut(),
    };
    send!((), app, c"setAppearance:", appearance => Id);
}

/// The name of the appearance the app runs with, such as
/// "NSAppearanceNameDarkAqua"; for the report of a test.
pub fn app_appearance() -> String {
    let app = send!(Id, class(c"NSApplication"), c"sharedApplication");
    let appearance = send!(Id, app, c"effectiveAppearance");
    if appearance.is_null() {
        return "none".into();
    }
    string_of(send!(Id, appearance, c"name"))
}

/// Whether "Swipe between pages" of the trackpad settings turns pages with
/// a two-finger scroll (the default), as opposed to three fingers only or
/// off: then a sideways scroll is a swipe between pages (`swipe.rs`).
pub fn swipe_between_pages_with_scroll() -> bool {
    send!(bool, class(c"NSEvent"), c"isSwipeTrackingFromScrollEventsEnabled")
}

/// "Bring All to Front" of the Window menu.
pub fn arrange_in_front() {
    let app = send!(Id, class(c"NSApplication"), c"sharedApplication");
    send!((), app, c"arrangeInFront:", std::ptr::null_mut::<c_void>() => Id);
}

/// One entry of the menu bar as AppKit holds it (`menu_bar`).
pub struct MenuEntry {
    /// 0 for a menu of the bar, 1 for its entries, 2 for a submenu's.
    pub depth: usize,
    /// The title of an entry that runs something; `None` for a menu, a
    /// separator or a submenu.
    pub title: Option<String>,
    /// The line to print: title, key equivalent, enabled and checked state.
    pub text: String,
}

/// The menu bar as AppKit holds it, one entry per line: the menu, then
/// its entries with their key equivalents, or `---` for a separator. The
/// Window and Help menus say when AppKit knows them as such (it adds the
/// window list and the search field itself).
pub fn menu_bar() -> Vec<MenuEntry> {
    let app = send!(Id, class(c"NSApplication"), c"sharedApplication");
    let main = send!(Id, app, c"mainMenu");
    let mut out = Vec::new();
    if main.is_null() {
        return out;
    }
    let windows_menu = send!(Id, app, c"windowsMenu");
    let help_menu = send!(Id, app, c"helpMenu");
    let count = send!(isize, main, c"numberOfItems");
    for index in 0..count {
        let item = send!(Id, main, c"itemAtIndex:", index => isize);
        let menu = send!(Id, item, c"submenu");
        let mut text = string_of(send!(Id, item, c"title"));
        if !menu.is_null() && menu == windows_menu {
            text.push_str("  [windowsMenu]");
        }
        if !menu.is_null() && menu == help_menu {
            text.push_str("  [helpMenu]");
        }
        out.push(MenuEntry { depth: 0, title: None, text });
        if !menu.is_null() {
            menu_entries(menu, 1, &mut out);
        }
    }
    out
}

fn menu_entries(menu: Id, depth: usize, out: &mut Vec<MenuEntry>) {
    let count = send!(isize, menu, c"numberOfItems");
    for index in 0..count {
        let item = send!(Id, menu, c"itemAtIndex:", index => isize);
        if send!(bool, item, c"isSeparatorItem") {
            out.push(MenuEntry { depth, title: None, text: "---".into() });
            continue;
        }
        let title = string_of(send!(Id, item, c"title"));
        let mut text = title.clone();
        let key = string_of(send!(Id, item, c"keyEquivalent"));
        if !key.is_empty() {
            let mask = send!(usize, item, c"keyEquivalentModifierMask");
            text.push_str("  ");
            text.push_str(&key_equivalent_text(&key, mask));
        }
        if !send!(bool, item, c"isEnabled") {
            text.push_str("  (disabled)");
        }
        if send!(isize, item, c"state") != 0 {
            text.push_str("  (checked)");
        }
        let submenu = send!(Id, item, c"submenu");
        out.push(MenuEntry { depth, title: submenu.is_null().then_some(title), text });
        if !submenu.is_null() {
            menu_entries(submenu, depth + 1, out);
        }
    }
}

/// "shift-cmd-z" for a key equivalent and its modifier mask, in the order
/// the menu shows the symbols.
fn key_equivalent_text(key: &str, mask: usize) -> String {
    const SHIFT: usize = 1 << 17;
    const CONTROL: usize = 1 << 18;
    const OPTION: usize = 1 << 19;
    const COMMAND: usize = 1 << 20;
    let mut text = String::new();
    for (bit, name) in [(CONTROL, "ctrl-"), (OPTION, "alt-"), (SHIFT, "shift-"), (COMMAND, "cmd-")] {
        if mask & bit != 0 {
            text.push_str(name);
        }
    }
    text.push_str(key);
    text
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
    fn key_equivalents_read_as_gpui_writes_them() {
        assert_eq!(key_equivalent_text(",", 1 << 20), "cmd-,");
        assert_eq!(key_equivalent_text("z", (1 << 20) | (1 << 17)), "shift-cmd-z");
        assert_eq!(key_equivalent_text("f", (1 << 20) | (1 << 18)), "ctrl-cmd-f");
        assert_eq!(key_equivalent_text("h", (1 << 20) | (1 << 19)), "alt-cmd-h");
        assert_eq!(key_equivalent_text("w", 0), "w");
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
