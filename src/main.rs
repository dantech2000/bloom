// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
mod adaptive;
mod admin;
mod airplay;
mod app;
mod awake;
mod bookmarks;
mod brand;
mod chromecast;
mod cast;
mod config;
mod connection;
mod credits;
mod debug;
mod downloads;
mod enhanced;
mod hidden;
mod icons;
mod images;
mod jellyfin;
mod lists;
mod macos;
mod menus;
mod metadata;
mod news;
mod nowplaying;
mod pacing;
mod perf;
mod pip;
mod playback;
mod player;
mod quality;
mod queue;
mod realtime;
mod search_index;
mod seasons;
mod settings;
mod stream;
mod subtitles;
mod swipe;
mod syncplay;
mod trailer;
#[allow(dead_code, unused_imports)]
mod ui;
mod updates;
mod video_surface;

/// The installed gpuicn component tests expect `crate::init`.
#[cfg(test)]
pub use ui::theme::init;
mod views;

use std::borrow::Cow;

use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, point, px, size,
};

use crate::{app::Bloom, config::Config, ui::theme::UiTheme};

fn main() {
    perf::mark_start();
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("bloom=info"));

    let application = gpui_kit::platform::application().with_assets(icons::Assets);
    // The red button and cmd-w close the window and the app stays, as in
    // Music and TV: a click on the Dock icon opens it again.
    application.on_reopen(reopen_window);
    application.run(|cx: &mut App| {
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

            // The folders of the name the app had before come along.
            brand::migrate_folders();
            let config = Config::load();
            UiTheme::set(cx, app::theme(config.dark.unwrap_or(true)));
            // The parts AppKit draws follow the theme of the app, not the
            // appearance of the system, so they match the window in both
            // settings of the Mac. `BLOOM_APPEARANCE=light|dark|system`
            // overrides it for a test.
            macos::set_app_appearance(match std::env::var("BLOOM_APPEARANCE").as_deref() {
                Ok("light") => Some(false),
                Ok("dark") => Some(true),
                Ok(_) => None,
                Err(_) => Some(config.dark.unwrap_or(true)),
            });

            let test_instance = std::env::var_os("BLOOM_TEST_NAME").is_some();
            open_main_window(cx, config);
            log::debug!("window open {} ms after start", perf::since_start_ms());
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
            if !test_instance {
                cx.activate(true);
            }
            // The updater, when this is a release build in its bundle.
            updates::start();
        });
}

/// Opens the one window of the app. The window opens where the user left
/// it; a saved place on a display that is gone falls back to the centred
/// default.
fn open_main_window(cx: &mut App, config: Config) {
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
    let test_instance = std::env::var_os("BLOOM_TEST_NAME").is_some();
    cx.open_window(
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
        },
        move |window, cx| {
            window.set_window_title(brand::NAME);
            if test_instance {
                // On screen, so it draws and can be captured, but
                // the app does not become the active one.
                if let Some(ns_window) = pip::ns_window(window) {
                    macos::send!((), ns_window, c"orderFrontRegardless");
                }
            }
            cx.new(|cx| Bloom::new(config, window, cx))
        },
    )
    .expect("open application window");
}

/// What a click on the Dock icon does while no window is open: the window
/// comes back, with the session of the config as at a start.
pub fn reopen_window(cx: &mut App) {
    if cx.windows().is_empty() {
        log::info!("window reopened");
        open_main_window(cx, Config::load());
    }
}
