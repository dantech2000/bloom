// SPDX-License-Identifier: AGPL-3.0-or-later
//! Filled icons. The web client uses filled Material icons for play, star,
//! favourite and a few others; Lucide has only outlines. Paths are from the
//! Material Icons set (Apache-2.0).

use std::borrow::Cow;

use gpui_icons::LucideAssetSource;
use gpui_kit::{AssetSource, Rgba, SharedString, Styled, Svg, px, svg};

const PREFIX: &str = "icons/filled/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filled {
    Play,
    Pause,
    Star,
    Heart,
    More,
    Trailer,
    DropDown,
    Rewind,
    Forward,
    VolumeUp,
    VolumeOff,
    Settings,
    Fullscreen,
    FullscreenExit,
    Captions,
    Groups,
    Cast,
}

impl Filled {
    const ALL: [Filled; 17] = [
        Filled::Play,
        Filled::Pause,
        Filled::Star,
        Filled::Heart,
        Filled::More,
        Filled::Trailer,
        Filled::DropDown,
        Filled::Rewind,
        Filled::Forward,
        Filled::VolumeUp,
        Filled::VolumeOff,
        Filled::Settings,
        Filled::Fullscreen,
        Filled::FullscreenExit,
        Filled::Captions,
        Filled::Groups,
        Filled::Cast,
    ];

    fn name(self) -> &'static str {
        match self {
            Filled::Play => "play",
            Filled::Pause => "pause",
            Filled::Star => "star",
            Filled::Heart => "heart",
            Filled::More => "more",
            Filled::Trailer => "trailer",
            Filled::DropDown => "drop-down",
            Filled::Rewind => "rewind",
            Filled::Forward => "forward",
            Filled::VolumeUp => "volume-up",
            Filled::VolumeOff => "volume-off",
            Filled::Settings => "settings",
            Filled::Fullscreen => "fullscreen",
            Filled::FullscreenExit => "fullscreen-exit",
            Filled::Captions => "captions",
            Filled::Groups => "groups",
            Filled::Cast => "cast",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Filled::Play => "M8 5v14l11-7z",
            Filled::Pause => "M6 19h4V5H6v14zm8-14v14h4V5h-4z",
            Filled::Star => {
                "M12 17.27L18.18 21l-1.64-7.03L22 9.24l-7.19-.61L12 2 9.19 8.63 2 9.24l5.46 4.73L5.82 21z"
            }
            Filled::Heart => {
                "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z"
            }
            Filled::More => {
                "M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z"
            }
            Filled::Trailer => {
                "M18 3v2h-2V3H8v2H6V3H4v18h2v-2h2v2h8v-2h2v2h2V3h-2zM8 17H6v-2h2v2zm0-4H6v-2h2v2zm0-4H6V7h2v2zm10 8h-2v-2h2v2zm0-4h-2v-2h2v2zm0-4h-2V7h2v2z"
            }
            Filled::DropDown => "M7 10l5 5 5-5z",
            Filled::Rewind => "M11 18V6l-8.5 6 8.5 6zm.5-6l8.5 6V6l-8.5 6z",
            Filled::Forward => "M4 18l8.5-6L4 6v12zm9-12v12l8.5-6L13 6z",
            Filled::VolumeUp => {
                "M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z"
            }
            Filled::VolumeOff => {
                "M16.5 12c0-1.77-1.02-3.29-2.5-4.03v2.21l2.45 2.45c.03-.2.05-.41.05-.63zm2.5 0c0 .94-.2 1.82-.54 2.64l1.51 1.51C20.63 14.91 21 13.5 21 12c0-4.28-2.99-7.86-7-8.77v2.06c2.89.86 5 3.54 5 6.71zM4.27 3L3 4.27 7.73 9H3v6h4l5 5v-6.73l4.25 4.25c-.67.52-1.42.93-2.25 1.18v2.06c1.38-.31 2.63-.95 3.69-1.81L19.73 21 21 19.73l-9-9L4.27 3zM12 4L9.91 6.09 12 8.18V4z"
            }
            Filled::Settings => {
                "M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58c.18-.14.23-.41.12-.61l-1.92-3.32c-.12-.22-.37-.29-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54c-.04-.24-.24-.41-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58c-.18.14-.23.41-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z"
            }
            Filled::Fullscreen => {
                "M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"
            }
            Filled::FullscreenExit => {
                "M5 16h3v3h2v-5H5v2zm3-8H5v2h5V5H8v3zm6 11h2v-3h3v-2h-5v5zm2-11V5h-2v5h5V8h-3z"
            }
            Filled::Captions => {
                "M19 4H5c-1.11 0-2 .9-2 2v12c0 1.1.89 2 2 2h14c1.1 0 2-.9 2-2V6c0-1.1-.9-2-2-2zm-8 7H9.5v-.5h-2v3h2V13H11v1c0 .55-.45 1-1 1H7c-.55 0-1-.45-1-1v-4c0-.55.45-1 1-1h3c.55 0 1 .45 1 1v1zm7 0h-1.5v-.5h-2v3h2V13H18v1c0 .55-.45 1-1 1h-3c-.55 0-1-.45-1-1v-4c0-.55.45-1 1-1h3c.55 0 1 .45 1 1v1z"
            }
            // The SyncPlay icon of the web client ("groups").
            Filled::Groups => {
                "M12 12.75c1.63 0 3.07.39 4.24.9 1.08.48 1.76 1.56 1.76 2.73V18H6v-1.61c0-1.18.68-2.26 1.76-2.73 1.17-.52 2.61-.91 4.24-.91zM4 13c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm1.13 1.1c-.37-.06-.74-.1-1.13-.1-.99 0-1.93.21-2.78.58C.48 14.9 0 15.62 0 16.43V18h4.5v-1.61c0-.83.23-1.61.63-2.29zM20 13c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm4 3.43c0-.81-.48-1.53-1.22-1.85-.85-.37-1.79-.58-2.78-.58-.39 0-.76.04-1.13.1.4.68.63 1.46.63 2.29V18H24v-1.57zM12 6c1.66 0 3 1.34 3 3s-1.34 3-3 3-3-1.34-3-3 1.34-3 3-3z"
            }
            // The "play on another device" icon of the web client.
            Filled::Cast => {
                "M21 3H3c-1.1 0-2 .9-2 2v3h2V5h18v14h-7v2h7c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zM1 18v3h3c0-1.66-1.34-3-3-3zm0-4v2c2.76 0 5 2.24 5 5h2c0-3.87-3.13-7-7-7zm0-4v2c4.97 0 9 4.03 9 9h2c0-6.08-4.93-11-11-11z"
            }
        }
    }
}

/// A filled icon of a size and colour.
pub fn filled(icon: Filled, size: f32, color: Rgba) -> Svg {
    svg()
        .path(SharedString::from(format!("{PREFIX}{}.svg", icon.name())))
        .size(px(size))
        .text_color(color)
}

const LOGO_PATH: &str = "icons/jellyfin.svg";
/// The Jellyfin mark with no background, from the Jellyfin branding files
/// (CC BY-SA 4.0).
const LOGO_SVG: &[u8] = include_bytes!("../assets/icon/jellyfin-transparent.svg");

/// The Jellyfin mark in one colour, as the web theme shows it in the top bar.
pub fn logo(size: f32, color: Rgba) -> Svg {
    svg().path(LOGO_PATH).size(px(size)).text_color(color)
}

/// Serves the filled icons and passes every other path on to Lucide.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO_PATH {
            return Ok(Some(Cow::Borrowed(LOGO_SVG)));
        }
        let Some(name) = path
            .strip_prefix(PREFIX)
            .and_then(|n| n.strip_suffix(".svg"))
        else {
            return LucideAssetSource.load(path);
        };
        Ok(Filled::ALL.iter().find(|i| i.name() == name).map(|icon| {
            Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="black" d="{}"/></svg>"#,
                    icon.path()
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        LucideAssetSource.list(path)
    }
}
