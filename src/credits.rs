// SPDX-License-Identifier: AGPL-3.0-or-later
//! The projects Bloom is made with, for the About page and `CREDITS.md`.
//! A test keeps the two in step: every entry here must be in the file.

use gpui_kit::{ClickEvent, Context, Div, ParentElement, StatefulInteractiveElement, Styled, div, px};

use crate::{
    app::Bloom,
    settings::{field, group, raised},
    ui::theme::UiTheme,
};

pub struct Credit {
    pub name: &'static str,
    /// What Bloom uses it for.
    pub role: &'static str,
    pub license: &'static str,
    pub url: &'static str,
}

pub const GROUPS: &[(&str, &[Credit])] = &[
    (
        "Built on",
        &[
            Credit { name: "jellyui", role: "The project Bloom started from, by iamd3vil", license: "AGPL-3.0-or-later", url: "https://github.com/iamd3vil/jellyui" },
            Credit { name: "Jellyfin", role: "The media server and its API", license: "GPL-2.0", url: "https://jellyfin.org" },
            Credit { name: "GPUI", role: "The UI framework, from the Zed editor", license: "Apache-2.0", url: "https://www.gpui.rs" },
            Credit { name: "gpui-kit", role: "Base components for GPUI", license: "Apache-2.0", url: "https://github.com/longbridge/gpui-kit" },
            Credit { name: "gpuicn", role: "The components in src/ui", license: "MIT", url: "https://github.com/devaryakjha/gpuicn" },
            Credit { name: "mpv", role: "Video playback, embedded as libmpv", license: "GPL-2.0-or-later", url: "https://mpv.io" },
            Credit { name: "FFmpeg", role: "Decoding and demuxing inside mpv", license: "GPL-3.0-or-later (as built here)", url: "https://ffmpeg.org" },
            Credit { name: "libmpv2", role: "Rust bindings for libmpv", license: "LGPL-2.1", url: "https://github.com/kohsine/libmpv2-rs" },
            Credit { name: "Sparkle", role: "Updates of the app in release builds", license: "MIT", url: "https://sparkle-project.org" },
            Credit { name: "Mozilla CA certificate list", role: "The certificate authorities the player trusts for HTTPS streams", license: "MPL-2.0", url: "https://curl.se/docs/caextract.html" },
        ],
    ),
    (
        "Look, icons and fonts",
        &[
            Credit { name: "Abyss theme", role: "The Jellyfin theme whose look Bloom follows", license: "MIT", url: "https://github.com/AumGupta/abyss-jellyfin" },
            Credit { name: "Jellyfin logo", role: "The mark in the top bar and on the sign-in page", license: "CC BY-SA 4.0", url: "https://github.com/jellyfin/jellyfin-ux" },
            Credit { name: "Lucide", role: "Line icons", license: "ISC", url: "https://lucide.dev" },
            Credit { name: "gpui-icons", role: "Lucide icons for GPUI", license: "MIT and ISC", url: "https://github.com/devaryakjha/gpui-icons" },
            Credit { name: "Material Design Icons", role: "Filled icons", license: "Apache-2.0", url: "https://github.com/google/material-design-icons" },
            Credit { name: "Geist", role: "Typeface", license: "OFL-1.1", url: "https://github.com/vercel/geist-font" },
            Credit { name: "Google Sans", role: "Typeface", license: "OFL-1.1", url: "https://github.com/googlefonts/googlesans" },
        ],
    ),
    (
        "Server plugins Bloom works with",
        &[
            Credit { name: "Jellyfin Enhanced", role: "Quality tags, bookmarks, hidden content, requests; Bloom follows its rules and storage", license: "GPL-3.0", url: "https://github.com/n00bcodr/Jellyfin-Enhanced" },
            Credit { name: "Intro Skipper", role: "The intro and credit segments Bloom can skip", license: "GPL-3.0", url: "https://github.com/intro-skipper/intro-skipper" },
        ],
    ),
    (
        "Studied for protocols and behaviour",
        &[
            Credit { name: "jellyfin-web", role: "The web client: SyncPlay, playback negotiation, remote control", license: "GPL-2.0", url: "https://github.com/jellyfin/jellyfin-web" },
            Credit { name: "jellyfin-chromecast", role: "The Cast receiver Bloom talks to", license: "GPL-2.0", url: "https://github.com/jellyfin/jellyfin-chromecast" },
            Credit { name: "IINA", role: "How a libmpv player drives rendering from the display", license: "GPL-3.0", url: "https://github.com/iina/iina" },
            Credit { name: "pyatv", role: "How AirPlay devices pair and play", license: "MIT", url: "https://github.com/postlund/pyatv" },
        ],
    ),
    (
        "Rust crates",
        &[
            Credit { name: "rustls and ring", role: "TLS", license: "Apache-2.0, ISC or MIT", url: "https://github.com/rustls/rustls" },
            Credit { name: "ureq", role: "HTTP client", license: "MIT or Apache-2.0", url: "https://github.com/algesten/ureq" },
            Credit { name: "tungstenite", role: "WebSocket client", license: "MIT or Apache-2.0", url: "https://github.com/snapview/tungstenite-rs" },
            Credit { name: "serde and serde_json", role: "JSON", license: "MIT or Apache-2.0", url: "https://github.com/serde-rs/serde" },
            Credit { name: "image and resvg", role: "Image and SVG decoding", license: "MIT or Apache-2.0", url: "https://github.com/image-rs/image" },
            Credit { name: "jiff", role: "Dates and times", license: "Unlicense or MIT", url: "https://github.com/BurntSushi/jiff" },
        ],
    ),
];

/// The license of Bloom itself.
pub const LICENSE_NAME: &str = "GNU Affero General Public License v3.0 or later";
pub const LICENSE_URL: &str = "https://www.gnu.org/licenses/agpl-3.0.html";

impl Bloom {
    /// The About page of the settings: what Bloom is, its license, and the
    /// projects it is made with, each with a link.
    pub(crate) fn render_settings_about(&self, cx: &mut Context<Self>) -> Div {
        let t = UiTheme::read(cx).clone();
        let soft = t.colors.foreground.opacity(0.6);
        let header = div()
            .flex()
            .items_center()
            .gap(px(18.))
            .child(crate::icons::logo(44., t.colors.foreground))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(24.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(crate::brand::NAME),
                    )
                    .child(div().text_size(px(14.)).text_color(soft).child(format!(
                        "{} · version {}",
                        crate::brand::TAGLINE,
                        crate::updates::version_line()
                    ))),
            );
        let about = group("About", cx)
            .child(header.py(px(8.)))
            .child(field(
                "License",
                format!(
                    "{} is free software under the {LICENSE_NAME}. You may use, study, share and change it; \
                     a copy you pass on must come with its source under the same license.",
                    crate::brand::NAME
                ),
                raised("about.license", "Read", cx)
                    .on_click(|_: &ClickEvent, _, cx| cx.open_url(LICENSE_URL)),
                cx,
            ))
            .child(field(
                "Not an official Jellyfin app",
                "Bloom is an independent client. Jellyfin and its logo belong to the Jellyfin project.",
                raised("about.jellyfin", "jellyfin.org", cx)
                    .on_click(|_: &ClickEvent, _, cx| cx.open_url("https://jellyfin.org")),
                cx,
            ));
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(about)
            .child(self.render_updates(cx));
        for (title, credits) in GROUPS {
            let mut card = group(*title, cx);
            for credit in credits.iter() {
                let url = credit.url;
                card = card.child(field(
                    credit.name,
                    format!("{} · {}", credit.role, credit.license),
                    raised(format!("about.open.{}", credit.name), "Open", cx)
                        .on_click(move |_: &ClickEvent, _, cx| cx.open_url(url)),
                    cx,
                ));
            }
            page = page.child(card);
        }
        page.child(
            div()
                .px(px(6.))
                .text_size(px(13.))
                .text_color(soft)
                .child("Bloom also uses several hundred Rust crates under MIT, Apache-2.0 and similar licenses; Cargo.lock lists them."),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_credit_is_in_the_credits_file() {
        let file = include_str!("../CREDITS.md");
        for (title, credits) in GROUPS {
            assert!(file.contains(title), "CREDITS.md lacks the group {title}");
            for credit in credits.iter() {
                assert!(file.contains(credit.name), "CREDITS.md lacks {}", credit.name);
                assert!(file.contains(credit.url), "CREDITS.md lacks the link of {}", credit.name);
                assert!(file.contains(credit.license), "CREDITS.md lacks the license of {}", credit.name);
            }
        }
    }
}
