// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bloom, the app: its modules, and the start of it for the Mac binary
//! (`src/main.rs`) and the iOS library (`ios/`).
// The parts with no window live in `bloom-core`; they keep their names here.
pub(crate) use bloom_core::{brand, config, jellyfin, news, options, realtime, search_index};

mod adaptive;
mod admin;
mod airplay;
mod app;
mod awake;
mod bookmarks;
mod chromecast;
mod cast;
mod connection;
mod credits;
mod debug;
mod downloads;
mod enhanced;
mod events;
mod hidden;
mod icons;
mod images;
mod lists;
mod macos;
mod menus;
mod metadata;
mod nowplaying;
mod pacing;
mod perf;
mod pip;
mod playback;
mod player;
mod quality_tags;
mod queue;
mod seasons;
mod settings;
mod socket;
mod stream;
mod subtitles;
mod swipe;
mod syncplay;
mod trailer;
mod ui;
mod updates;
#[cfg(target_os = "macos")]
mod video_surface;
/// iOS has no video path yet: the frame type alone, so the pages that
/// hold a frame compile.
#[cfg(not(target_os = "macos"))]
#[path = "video_surface_stub.rs"]
mod video_surface;
mod views;

use std::borrow::Cow;

use gpui_kit::{App, AppContext as _, WindowOptions};
#[cfg(target_os = "macos")]
use gpui_kit::{
    Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, point, px, size,
};

use crate::{app::Bloom, config::Config, ui::theme::UiTheme};

/// Turns the phone to landscape for the player, and back when it closes.
/// Nothing on the Mac.
pub(crate) fn set_landscape(on: bool) {
    #[cfg(target_os = "ios")]
    gpui_ios::set_landscape(on);
    #[cfg(not(target_os = "ios"))]
    let _ = on;
}

/// The icons of the app, for `Application::with_assets`.
pub fn assets() -> icons::Assets {
    icons::Assets
}

/// Runs the Mac app: the GPUI application with the one window.
#[cfg(target_os = "macos")]
pub fn run() {
    perf::mark_start();
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("bloom=info"));

    let application = gpui_kit::platform::application().with_assets(icons::Assets);
    // The red button and cmd-w close the window and the app stays, as in
    // Music and TV: a click on the Dock icon opens it again.
    application.on_reopen(reopen_window);
    application.run(|cx: &mut App| {
        let config = init(cx);
        open_main_window(cx, config);
    });
}

/// Sets the app up in a fresh GPUI context: fonts, theme, menus, the
/// config. Returns the config the window opens with.
pub fn init(cx: &mut App) -> Config {
    ui::theme::init(cx);
    cx.text_system()
        .add_fonts(vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Medium.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/GeistMono-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/GoogleSans-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/GoogleSans-Medium.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/GoogleSans-SemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/GoogleSans-Bold.ttf")),
        ])
        .expect("load bundled fonts");
    log::debug!("fonts loaded {} ms after start", perf::since_start_ms());

    #[cfg(target_os = "macos")]
    {
        menus::init(cx);
        // Inside an app bundle macOS draws the icon of the bundle, in the
        // look the user chose (default, dark, clear, tinted). An icon set
        // here would replace it with one fixed picture, so only the bare
        // binary, which has no icon, gets one.
        let bundled = std::env::current_exe()
            .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
        if !bundled {
            macos::set_app_icon(include_bytes!("../assets/icon/bloom.png"));
        }
    }

    // The folders of the name the app had before come along.
    brand::migrate_folders();
    let mut config = Config::load();
    // Bloom is dark only: the light theme has no switch any more,
    // and a choice of it that an older build saved is dropped.
    config.dark = None;
    UiTheme::set(cx, app::theme(true));
    ui::glass::set_liquid(config.liquid_glass.unwrap_or(true));
    // The parts AppKit draws follow the theme of the app, not the
    // appearance of the system, so they match the window in both
    // settings of the Mac. `BLOOM_APPEARANCE=light|dark|system`
    // overrides it for a test.
    #[cfg(target_os = "macos")]
    macos::set_app_appearance(match options::options().appearance.as_deref() {
        Some("light") => Some(false),
        Some("dark") => Some(true),
        Some(_) => None,
        None => Some(true),
    });

    // Nothing may play from a window that is gone: the cores close
    // with the last window (`player::shut_down_all`).
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            player::shut_down_all();
        }
    })
    .detach();
    // mpv is closed before the process exits: a core that runs at
    // exit can crash (see `player::shut_down_all`).
    cx.on_app_quit(|_| {
        player::shut_down_all();
        async {}
    })
    .detach();
    config
}

/// Opens the one window of the app. On the Mac the window opens where the
/// user left it; a saved place on a display that is gone falls back to
/// the centred default. On iOS the window is the screen.
pub fn open_main_window(cx: &mut App, config: Config) {
    #[cfg(target_os = "macos")]
    let test_instance = options::options().test_name.is_some();
    #[cfg(target_os = "macos")]
    let options = {
        let saved = config.window.map(|[x, y, w, h]| {
            Bounds::new(point(px(x), px(y)), size(px(w.max(900.)), px(h.max(600.))))
        });
        let bounds = saved
            .filter(|bounds| {
                cx.displays()
                    .iter()
                    .any(|display| display.bounds().intersects(bounds))
            })
            .unwrap_or_else(|| Bounds::centered(None, size(px(1280.), px(820.)), cx));
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(brand::NAME.into()),
                appears_transparent: true,
                traffic_light_position: Some(point(
                    px(pip::TRAFFIC_LIGHTS.0),
                    px(pip::TRAFFIC_LIGHTS.1),
                )),
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(900.), px(600.))),
            window_background: WindowBackgroundAppearance::Opaque,
            // A test instance must not take the keyboard from the
            // person at the Mac: the keys he types would go to its
            // window, and macOS beeps for each one it cannot use.
            focus: !test_instance,
            ..Default::default()
        }
    };
    #[cfg(not(target_os = "macos"))]
    let options = WindowOptions::default();
    cx.open_window(options, move |window, cx| {
        window.set_window_title(brand::NAME);
        #[cfg(target_os = "macos")]
        if test_instance {
            // On screen, so it draws and can be captured, but
            // the app does not become the active one.
            if let Some(ns_window) = pip::ns_window(window) {
                macos::send!((), ns_window, c"orderFrontRegardless");
            }
        }
        cx.new(|cx| Bloom::new(config, window, cx))
    })
    .expect("open application window");
    log::debug!("window open {} ms after start", perf::since_start_ms());
    #[cfg(target_os = "macos")]
    if !test_instance {
        cx.activate(true);
    }
    // The updater, when this is a release build in its bundle.
    #[cfg(target_os = "macos")]
    updates::start();
}

/// What a click on the Dock icon does while no window is open: the window
/// comes back, with the session of the config as at a start.
pub fn reopen_window(cx: &mut App) {
    if cx.windows().is_empty() {
        log::info!("window reopened");
        open_main_window(cx, Config::load());
    }
}
