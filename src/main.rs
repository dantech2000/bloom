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
mod syncplay;
mod trailer;
#[allow(dead_code, unused_imports)]
mod ui;
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

    gpui_kit::platform::application()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
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

            // The window opens where the user left it. A saved place on a
            // display that is gone falls back to the centred default.
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
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title(brand::NAME);
                    cx.new(|cx| Bloom::new(config, window, cx))
                },
            )
            .expect("open application window");
            log::debug!("window open {} ms after start", perf::since_start_ms());
            cx.activate(true);
        });
}
