// SPDX-License-Identifier: AGPL-3.0-or-later
//! The parts of the Branding page that are not a plain field: the splash
//! screen (the image clients show while they load) and the debug commands
//! of the page.
//!
//! The server keeps the splash screen as a file. `POST /Branding/Splashscreen`
//! takes the image as base64 text with the type of the image as content type,
//! saves it, and writes its place into `SplashscreenLocation` of the branding
//! configuration. `DELETE` removes the file and the place. `GET` gives the
//! custom image, or else the default image of the server, or 404.

use std::{
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::Result;
use gpui_kit::{
    ClickEvent, Context, Div, ObjectFit, ParentElement as _, StatefulInteractiveElement as _, Styled, Window, div, px, rgba,
};
use serde_json::Value;

use super::{ButtonKind, Confirm, Section, button, config};
use crate::{
    app::{Bloom, Page},
    jellyfin::Client,
    ui::theme::UiTheme,
};

/// Changes with each upload and removal, so the preview loads the image
/// again instead of showing the one the image cache holds.
static REV: AtomicU64 = AtomicU64::new(0);

/// Largest image the page sends.
const MAX_BYTES: usize = 10 * 1024 * 1024;

/// The type of an image file, from its name; none for a file that is not an
/// image the server accepts.
pub fn image_mime(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// The place of the custom splash image in a branding object; none when the
/// server has no custom image.
pub fn custom_splash(branding: &Value) -> Option<&str> {
    branding["SplashscreenLocation"].as_str().filter(|place| !place.is_empty())
}

/// The block under the splash screen toggle: the image, its state, and the
/// buttons to change it.
pub fn splash_block(branding: &Value, preview: &str, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let custom = custom_splash(branding).is_some();
    let enabled = branding["SplashscreenEnabled"].as_bool().unwrap_or(false);
    let url = format!("{preview}?tag={}", REV.load(Ordering::Relaxed));
    let state = match (custom, enabled) {
        (true, true) => "A custom image is set. Clients show it while they load.",
        (true, false) => "A custom image is set. The splash screen is off, so clients do not show it.",
        (false, true) => "No custom image. Clients show the default image of the server.",
        (false, false) => "No custom image. The splash screen is off.",
    };
    let mut actions = div().flex().gap(px(10.)).child(
        button("branding.splash.upload", "Upload image", ButtonKind::Plain, cx)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_splash(cx))),
    );
    if custom {
        actions = actions.child(
            button("branding.splash.remove", "Remove", ButtonKind::Plain, cx).on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.ask_remove_splash(cx)),
            ),
        );
    }
    div()
        .py(px(11.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            // The preview, over a note that stays when the server has no
            // image or it cannot load.
            div()
                .relative()
                .w(px(320.))
                .h(px(180.))
                .rounded(px(12.))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(0x282828cc))
                .bg(rgba(0x00000059))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .text_color(t.colors.foreground.opacity(0.5))
                .child("No image")
                .child(
                    // The corners of the box, inside its 1 px edge.
                    crate::images::remote_with(url, px(11.), ObjectFit::Cover)
                        .absolute()
                        .inset_0(),
                ),
        )
        .child(
            div()
                .text_size(px(14.))
                .text_color(t.colors.foreground.opacity(0.7))
                .child(state),
        )
        .child(actions)
}

impl Bloom {
    /// Opens the file dialog; the chosen image becomes the splash screen.
    pub fn pick_splash(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use as splash screen".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.upload_splash(path, cx)).ok();
        })
        .detach();
    }

    pub fn upload_splash(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(mime) = image_mime(&path) else {
            self.toast("Splash screen", "Choose a PNG, JPEG, WebP or GIF image.", cx);
            return;
        };
        self.splash_change(
            "The splash screen is set.",
            cx,
            move |client| {
                let bytes = std::fs::read(&path)?;
                anyhow::ensure!(bytes.len() <= MAX_BYTES, "the image is larger than 10 MB");
                client.post_image("/Branding/Splashscreen", &bytes, mime)
            },
        );
    }

    pub fn ask_remove_splash(&mut self, cx: &mut Context<Self>) {
        self.ask_confirm(
            Confirm {
                title: "Remove the splash screen?".to_string(),
                message: "The custom image is deleted from the server. Clients show the default \
                          image again."
                    .to_string(),
                action: "Remove".to_string(),
                danger: true,
                run: Rc::new(|this, cx| this.remove_splash(cx)),
            },
            cx,
        );
    }

    pub fn remove_splash(&mut self, cx: &mut Context<Self>) {
        self.splash_change("The splash screen is removed.", cx, |client| {
            client.call("DELETE", "/Branding/Splashscreen", &[])
        });
    }

    /// Runs a change of the splash screen. The server writes the place of
    /// the image into the branding configuration, so the page takes that key
    /// from the server: a save of an older copy must not undo the change.
    fn splash_change(
        &mut self,
        done: &'static str,
        cx: &mut Context<Self>,
        work: impl FnOnce(&Client) -> Result<()> + Send + 'static,
    ) {
        self.fetch(
            cx,
            move |client| -> Result<Value> {
                work(&client)?;
                REV.fetch_add(1, Ordering::Relaxed);
                client.get::<Value>("/System/Configuration/branding", &[])
            },
            move |this, result, cx| {
                match result {
                    Ok(server) => {
                        this.toast(done, "", cx);
                        this.config_take_key(Section::Branding, 0, "SplashscreenLocation", &server, cx);
                    }
                    Err(err) => this.toast("The server refused the change", format!("{err:#}"), cx),
                }
                cx.notify();
            },
        );
    }

    /// The commands of the Branding page for the debug channel.
    pub fn debug_branding(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" | "state" => {}
            "css-open" => {
                if !self.config_open_text("CustomCss", window, cx) {
                    return "error: open the Branding page first (admin branding) and wait for its load"
                        .into();
                }
            }
            "css-set" => {
                // `\n` is a new line, as the debug channel has one line only.
                if !self.text_editor_set(&arg.replace("\\n", "\n"), window, cx) {
                    return "error: no text dialog is open (branding css-open)".into();
                }
            }
            "css-scroll" => {
                if !self.text_editor_scroll(arg.parse().unwrap_or(0.), cx) {
                    return "error: no text dialog is open (branding css-open)".into();
                }
            }
            // The text goes into the page, then the page saves, as the
            // buttons do.
            "css-save" => {
                if !self.text_editor_open() {
                    return "error: no text dialog is open (branding css-open)".into();
                }
                self.submit_text_editor(window, cx);
                self.config_save(cx);
            }
            "splash-upload" => {
                if arg.is_empty() {
                    return "error: usage: branding splash-upload <path>".into();
                }
                self.upload_splash(PathBuf::from(arg), cx);
            }
            "splash-remove" => self.remove_splash(cx),
            _ => {
                return "error: branding state|css-open|css-set <text>|css-scroll <lines>|css-save|splash-upload <path>|splash-remove"
                    .into();
            }
        }
        self.branding_state(cx)
    }

    /// The Branding page in a line.
    fn branding_state(&self, cx: &gpui_kit::App) -> String {
        let Page::Admin(admin) = &self.page else {
            return "no admin page".into();
        };
        let page = self.config_state();
        let editor = match self.text_editor_value(cx) {
            Some(text) => format!(
                "text dialog open: {} lines, {} characters",
                text.split('\n').count(),
                text.chars().count()
            ),
            None => "no text dialog".to_string(),
        };
        let splash = admin
            .config
            .get(&Section::Branding)
            .map(|d| match config::branding_of(d).and_then(|b| custom_splash(b).map(|_| ())) {
                Some(()) => "custom splash set",
                None => "no custom splash",
            })
            .unwrap_or("branding not loaded");
        format!("{page}; {editor}; {splash}")
    }
}
