// SPDX-License-Identifier: AGPL-3.0-or-later
//! Updates of the app, through the Sparkle framework.
//!
//! Sparkle is not linked: it is loaded from the Frameworks folder of the
//! app bundle at the start, and only when the bundle names a feed and a
//! public key (`SUFeedURL`, `SUPublicEDKey`; `dev/bundle` writes them from
//! `src/brand.rs`). A bare binary, a development bundle, or a build with no
//! key has no updater, and the app says so.
//!
//! The app behaves as the Zed editor does: Sparkle asks the feed once a
//! day, downloads a newer version with no question, and checks its
//! signature against the public key. Then the top bar shows "Restart to
//! update". A click installs the update and starts the app again; with no
//! click the update is installed when the app quits. Nothing opens over a
//! video. "Check for Updates…" in the app menu shows Sparkle's own window.
//!
//! Sparkle tells the app what it does through a delegate object. Its class
//! is made here at run time ([`delegate`]), because the app has no
//! Objective-C code of its own.

use std::{
    ffi::{c_char, c_void},
    sync::{Mutex, OnceLock},
};

use gpui_kit::{App, ClickEvent, Context, Div, ParentElement, StatefulInteractiveElement};

use crate::{
    airplay::av::{nsstring, string_of},
    app::Bloom,
    macos::{Id, Sel, class, sel, send},
    settings::{checkbox, field, group, raised},
};

/// What the updater does now.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Idle,
    /// A newer version was found and is on its way.
    Downloading(String),
    /// The version is downloaded and checked; a restart installs it.
    Ready(String),
}

static STATUS: Mutex<Status> = Mutex::new(Status::Idle);
/// The block of Sparkle that installs the update and starts the app again,
/// as a number.
static INSTALL: Mutex<Option<usize>> = Mutex::new(None);
static CHANGES: OnceLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> = OnceLock::new();

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

/// A signal for each change of [`status`].
pub fn changes() -> async_channel::Receiver<()> {
    CHANGES.get_or_init(|| async_channel::bounded(4)).1.clone()
}

fn set_status(status: Status) {
    *STATUS.lock().unwrap() = status;
    let _ = CHANGES.get_or_init(|| async_channel::bounded(4)).0.try_send(());
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
    fn class_addMethod(class: Id, name: Sel, method: *const c_void, types: *const c_char) -> bool;
    fn objc_registerClassPair(class: Id);
}

unsafe extern "C" {
    fn _Block_copy(block: *const c_void) -> *mut c_void;
}

/// The start of an Objective-C block: enough to call it.
#[repr(C)]
struct Block {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut Block),
}

fn version_of(item: Id) -> String {
    string_of(send!(Id, item, c"displayVersionString")).unwrap_or_default()
}

/// `updater:didFindValidUpdate:`
extern "C" fn did_find(_this: Id, _sel: Sel, _updater: Id, item: Id) {
    let version = version_of(item);
    log::info!("updates: {version} is available");
    set_status(Status::Downloading(version));
}

/// `updater:willInstallUpdateOnQuit:immediateInstallationBlock:`. Sparkle
/// has the update ready; it keeps the block valid until the app quits.
extern "C" fn will_install_on_quit(_this: Id, _sel: Sel, _updater: Id, item: Id, block: *const c_void) -> bool {
    let version = version_of(item);
    log::info!("updates: {version} is downloaded; a restart installs it");
    *INSTALL.lock().unwrap() = Some(unsafe { _Block_copy(block) } as usize);
    set_status(Status::Ready(version));
    true
}

/// `updater:didAbortWithError:`. Also the answer "no update" ends here.
extern "C" fn did_abort(_this: Id, _sel: Sel, _updater: Id, error: Id) {
    let text = string_of(send!(Id, error, c"localizedDescription")).unwrap_or_default();
    log::debug!("updates: check ended: {text}");
    if matches!(status(), Status::Downloading(_)) {
        set_status(Status::Idle);
    }
}

/// The delegate object of the updater, of a class made at run time.
fn delegate() -> Id {
    let class = unsafe { objc_allocateClassPair(class(c"NSObject"), c"BloomUpdaterDelegate".as_ptr(), 0) };
    if class.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        class_addMethod(
            class,
            sel(c"updater:didFindValidUpdate:"),
            did_find as *const c_void,
            c"v@:@@".as_ptr(),
        );
        class_addMethod(
            class,
            sel(c"updater:willInstallUpdateOnQuit:immediateInstallationBlock:"),
            will_install_on_quit as *const c_void,
            c"B@:@@@?".as_ptr(),
        );
        class_addMethod(
            class,
            sel(c"updater:didAbortWithError:"),
            did_abort as *const c_void,
            c"v@:@@".as_ptr(),
        );
        objc_registerClassPair(class);
    }
    let object = send!(Id, class, c"alloc");
    send!(Id, object, c"init")
}

/// Installs the downloaded update and starts the app again. False when no
/// update is ready. Call on the main thread.
pub fn restart_to_update() -> bool {
    let Some(block) = INSTALL.lock().unwrap().take() else {
        return false;
    };
    log::info!("updates: installing, then the app starts again");
    let block = block as *mut Block;
    unsafe { ((*block).invoke)(block) };
    true
}

/// Why there is no updater, or the controller of Sparkle (kept for the life
/// of the app, as a number so the value can sit in a static).
enum State {
    Ready(usize),
    Off(&'static str),
}

static STATE: OnceLock<State> = OnceLock::new();

fn info(key: &str) -> Option<String> {
    let bundle = send!(Id, class(c"NSBundle"), c"mainBundle");
    let value = send!(Id, bundle, c"objectForInfoDictionaryKey:", nsstring(key) => Id);
    // The value could be a number or a list; only a string is of use.
    if value.is_null() || !send!(bool, value, c"isKindOfClass:", class(c"NSString") => Id) {
        return None;
    }
    string_of(value).filter(|text| !text.is_empty())
}

/// Loads Sparkle and starts its updater. Call once, on the main thread,
/// when the window is open. With automatic checks on, Sparkle looks at the
/// feed by itself from here on.
pub fn start() {
    STATE.get_or_init(|| {
        if info("SUFeedURL").is_none() || info("SUPublicEDKey").is_none() {
            return State::Off("This build has no update feed. Builds from a release update themselves.");
        }
        let bundle = send!(Id, class(c"NSBundle"), c"mainBundle");
        let frameworks = string_of(send!(Id, bundle, c"privateFrameworksPath")).unwrap_or_default();
        let path = format!("{frameworks}/Sparkle.framework");
        let sparkle = send!(Id, class(c"NSBundle"), c"bundleWithPath:", nsstring(&path) => Id);
        if sparkle.is_null() || !send!(bool, sparkle, c"load") {
            log::warn!("updates: no Sparkle framework at {path}");
            return State::Off("The update framework is not in this build.");
        }
        let controller = class(c"SPUStandardUpdaterController");
        if controller.is_null() {
            return State::Off("The update framework is not in this build.");
        }
        let controller = send!(Id, controller, c"alloc");
        // Sparkle does not keep the delegate alive; it lives as long as
        // the app.
        let controller = send!(
            Id, controller, c"initWithStartingUpdater:updaterDelegate:userDriverDelegate:",
            true => bool, delegate() => Id, std::ptr::null_mut() => Id
        );
        if controller.is_null() {
            return State::Off("The updater did not start.");
        }
        log::info!("updates: the updater runs, feed {}", info("SUFeedURL").unwrap_or_default());
        State::Ready(controller as usize)
    });
}

fn updater() -> Option<Id> {
    match STATE.get()? {
        State::Ready(controller) => {
            let updater = send!(Id, *controller as Id, c"updater");
            (!updater.is_null()).then_some(updater)
        }
        State::Off(_) => None,
    }
}

/// Why the app cannot update itself, when it cannot.
pub fn off_reason() -> Option<&'static str> {
    match STATE.get() {
        Some(State::Ready(_)) => None,
        Some(State::Off(why)) => Some(why),
        None => Some("The updater has not started."),
    }
}

/// Asks the feed now and shows Sparkle's window with the answer. False
/// when there is no updater.
pub fn check_now() -> bool {
    let Some(State::Ready(controller)) = STATE.get() else {
        return false;
    };
    send!((), *controller as Id, c"checkForUpdates:", std::ptr::null_mut() => Id);
    true
}

pub fn automatic() -> bool {
    updater().is_some_and(|updater| send!(bool, updater, c"automaticallyChecksForUpdates"))
}

pub fn set_automatic(on: bool) {
    if let Some(updater) = updater() {
        send!((), updater, c"setAutomaticallyChecksForUpdates:", on => bool);
    }
}

/// When the feed was last asked, as macOS writes a date.
fn last_check() -> Option<String> {
    let date = send!(Id, updater()?, c"lastUpdateCheckDate");
    if date.is_null() {
        return None;
    }
    string_of(send!(Id, date, c"description"))
}

/// One line for the debug command `updates`.
pub fn describe() -> String {
    match off_reason() {
        Some(why) => format!("updater=off why={why:?} feed={:?}", info("SUFeedURL")),
        None => format!(
            "updater=on status={:?} feed={:?} automatic={} last_check={:?} can_check={}",
            status(),
            info("SUFeedURL").unwrap_or_default(),
            automatic(),
            last_check(),
            updater().is_some_and(|updater| send!(bool, updater, c"canCheckForUpdates")),
        ),
    }
}

/// The version with the commit of the build, as the About page shows it.
pub fn version_line() -> String {
    match option_env!("BLOOM_COMMIT") {
        Some(commit) if !commit.is_empty() => format!("{} ({commit})", crate::config::APP_VERSION),
        _ => crate::config::APP_VERSION.to_string(),
    }
}

impl Bloom {
    /// "Check for Updates…" of the app menu.
    pub fn check_for_updates(&mut self, cx: &mut App) {
        if !check_now() {
            self.toast("No updater in this build", off_reason().unwrap_or_default(), cx);
        }
    }

    /// "Restart to update" in the top bar, while a downloaded update waits.
    pub fn render_update_chip(&self, cx: &mut Context<Self>) -> Option<gpui_kit::Stateful<Div>> {
        use gpui_kit::{InteractiveElement, Styled, px, rgba};
        let Status::Ready(version) = status() else {
            return None;
        };
        let fg = crate::ui::theme::UiTheme::read(cx).colors.foreground;
        Some(
            gpui_kit::div()
                .id("top.update")
                .h(px(36.))
                .px(px(12.))
                .rounded_full()
                .bg(rgba(0xffffff1f))
                .hover(|s| s.bg(rgba(0xffffff29)))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(13.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(fg)
                .tooltip(crate::ui::tip::tip(format!(
                    "{} {version} is downloaded. Click to install it and start again.",
                    crate::brand::NAME
                )))
                .child(crate::views::cards::icon(gpui_icons::LucideIcon::RefreshCw, 15., fg))
                .child("Restart to update")
                .on_click(cx.listener(|_, _: &ClickEvent, _, _| {
                    restart_to_update();
                })),
        )
    }

    /// The status of the updater changed: the top bar and the About page
    /// show it, and a version that is ready is said once.
    pub fn update_status_changed(&mut self, cx: &mut Context<Self>) {
        if let Status::Ready(version) = status()
            && self.update_said.as_deref() != Some(version.as_str())
            && !self.player_open
        {
            self.update_said = Some(version.clone());
            self.toast(
                format!("{} {version} is ready", crate::brand::NAME),
                "Restart to update, in the top bar. It also installs when you quit.",
                cx,
            );
        }
        cx.notify();
    }

    /// The group of the About page: the version, and the updater or the
    /// reason there is none.
    pub(crate) fn render_updates(&self, cx: &mut Context<Self>) -> Div {
        let card = group("Updates", cx);
        match off_reason() {
            Some(why) => card.child(field(
                format!("Version {}", version_line()),
                why,
                gpui_kit::div(),
                cx,
            )),
            None => card
                .child(field(
                    format!("Version {}", version_line()),
                    match (status(), last_check()) {
                        (Status::Ready(version), _) => {
                            format!("{} {version} is downloaded. Restart to install it; it also installs when you quit.", crate::brand::NAME)
                        }
                        (Status::Downloading(version), _) => format!("{} {version} is downloading.", crate::brand::NAME),
                        (Status::Idle, Some(when)) => format!("Last check: {when}"),
                        (Status::Idle, None) => "No check yet.".to_string(),
                    },
                    raised("about.updates.check", "Check now", cx).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| this.check_for_updates(cx),
                    )),
                    cx,
                ))
                .child(field(
                    "Update by itself",
                    "Looks for a new version once a day and downloads it. It is installed at the next restart.",
                    checkbox("about.updates.automatic", automatic(), cx).on_click(cx.listener(
                        |_, _: &ClickEvent, _, cx| {
                            set_automatic(!automatic());
                            cx.notify();
                        },
                    )),
                    cx,
                )),
        }
    }

    pub fn debug_updates(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        match rest {
            "" | "state" => describe(),
            "check" => {
                self.check_for_updates(cx);
                describe()
            }
            // The check Sparkle makes by itself once a day, now: no window
            // unless there is an update.
            "background" => {
                if let Some(updater) = updater() {
                    send!((), updater, c"checkForUpdatesInBackground");
                }
                describe()
            }
            // What a click on "Restart to update" does.
            "restart" => {
                if restart_to_update() {
                    "restarting".into()
                } else {
                    "error: no update is ready".into()
                }
            }
            "auto on" | "auto off" => {
                set_automatic(rest == "auto on");
                describe()
            }
            _ => "error: updates [state | check | background | restart | auto on|off]".into(),
        }
    }
}
