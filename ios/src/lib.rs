// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Rust side of the iOS app. The host (`host/App.swift`) calls
//! `bloom_ios_start` once its window scene connects; that starts GPUI with
//! the iOS platform and opens the one window, whose view controller the
//! host installs in its `UIWindow`.
#![cfg(target_os = "ios")]

use gpui::App;

/// Routes the `log` crate to NSLog, which the simulator and a device show.
struct NsLogLogger;

impl log::Log for NsLogLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        nslog(&format!("[{}] {}: {}", record.level(), record.target(), record.args()));
    }

    fn flush(&self) {}
}

fn nslog(message: &str) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    unsafe extern "C" {
        fn NSLog(format: *mut AnyObject, ...);
    }
    let Ok(message) = std::ffi::CString::new(message) else { return };
    unsafe {
        let message: *mut AnyObject =
            msg_send![class!(NSString), stringWithUTF8String: message.as_ptr()];
        let format: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c"%@".as_ptr()];
        NSLog(format, message);
    }
}

/// Starts GPUI and opens the window. Called by the host on the main thread,
/// after `gpui_ios_set_embedded` when the host supplies the `UIWindow`.
#[unsafe(no_mangle)]
pub extern "C" fn bloom_ios_start() {
    let _ = log::set_logger(&NsLogLogger).map(|()| log::set_max_level(log::LevelFilter::Info));
    std::panic::set_hook(Box::new(|info| nslog(&format!("PANIC: {info}"))));
    log::info!("bloom-ios: start");

    gpui_ios::ffi::set_app_callback(Box::new(|cx: &mut App| {
        let config = bloom::init(cx);
        bloom::open_main_window(cx, config);
    }));
    gpui_ios::ffi::run_app_with(|application| application.with_assets(bloom::assets()));
}
