// SPDX-License-Identifier: AGPL-3.0-or-later
//! API keys that let other programs talk to the server. The page never shows
//! a whole key: only its first characters, to tell two keys apart.

use std::rc::Rc;

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, ClipboardItem, Context, Div, InteractiveElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled, div, px, rgba,
};
use serde::Deserialize;

use super::{
    Confirm, Field, Prompt, Secret, ago, card,
    users::{AMBER, absolute, action, content_width, icon_disc, is_real_date, primary, stat},
};
use crate::{app::Bloom, jellyfin::Client, ui::theme::UiTheme};
use crate::ui::tip::tip;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Key {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub app_name: String,
    #[serde(default)]
    pub date_created: Option<String>,
}

impl Key {
    /// First characters of the key, then dots.
    fn masked(&self) -> String {
        let head: String = self.access_token.chars().take(4).collect();
        format!("{head}{}", "•".repeat(20))
    }
}

pub struct Data {
    pub keys: Vec<Key>,
}

pub fn load(client: &Client) -> Result<Data> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Page {
        #[serde(default)]
        items: Vec<Key>,
    }
    let mut keys = client.get::<Page>("/Auth/Keys", &[])?.items;
    keys.sort_by(|a, b| b.date_created.cmp(&a.date_created));
    Ok(Data { keys })
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    // The key column goes when the window is narrow.
    let show_key = content_width(app) >= 640.;

    let mut list = card(cx).p(px(6.));
    if data.keys.is_empty() {
        list = list.child(
            div()
                .p(px(12.))
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child("The server has no API keys."),
        );
    }
    for (index, key) in data.keys.iter().enumerate() {
        let created = key.date_created.as_deref().filter(|d| is_real_date(d));
        let mut row = div()
            .h(px(64.))
            .px(px(12.))
            .rounded(px(12.))
            .flex()
            .items_center()
            .gap(px(14.))
            .hover(|s| s.bg(rgba(0xffffff0a)))
            .child(icon_disc(LucideIcon::KeyRound, rgba(AMBER), 40.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(15.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(t.colors.foreground)
                            .child(if key.app_name.is_empty() {
                                "Unnamed".to_string()
                            } else {
                                key.app_name.clone()
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(t.colors.foreground.opacity(0.55))
                            .child(match created {
                                Some(date) => format!("Created {} · {}", absolute(date), ago(date)),
                                None => "Creation date unknown".to_string(),
                            }),
                    ),
            );
        if show_key {
            row = row.child(
                div()
                    .flex_shrink_0()
                    .h(px(30.))
                    .px(px(12.))
                    .rounded(px(10.))
                    .bg(rgba(0x00000052))
                    .flex()
                    .items_center()
                    .font_family("Menlo")
                    .text_size(px(13.))
                    .text_color(t.colors.foreground.opacity(0.75))
                    .child(key.masked()),
            );
        }
        // The key goes to the clipboard and stays masked on the page.
        let token = key.access_token.clone();
        row = row.child(
            action(format!("keys.copy.{index}"), LucideIcon::Copy, "Copy", false, cx)
                .tooltip(tip("Copy the key to the clipboard"))
                .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(token.clone()));
                    this.toast("API key copied", "", cx);
                }),
            ),
        );
        let target = key.clone();
        row = row.child(
            action(
                format!("keys.revoke.{index}"),
                LucideIcon::Trash,
                "Revoke",
                true,
                cx,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                ask_revoke(this, &target, cx)
            })),
        );
        if index > 0 {
            list = list.child(div().mx(px(12.)).h(px(1.)).bg(rgba(0xf5f5f70f)));
        }
        list = list.child(row);
    }

    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .child(stat(LucideIcon::KeyRound, "API keys", data.keys.len(), cx))
                .child(
                    primary("keys.new", LucideIcon::Plus, "New API key", cx).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| ask_new(this, cx)),
                    ),
                ),
        )
        .child(list)
        .child(
            div()
                .text_size(px(13.))
                .text_color(t.colors.foreground.opacity(0.5))
                .child(
                    "A key gives a program full access to the server. Revoke a key that is no \
                     longer in use.",
                ),
        )
}

fn ask_revoke(app: &mut Bloom, key: &Key, cx: &mut Context<Bloom>) {
    let token = key.access_token.clone();
    let name = if key.app_name.is_empty() {
        "this key".to_string()
    } else {
        format!("the key of {}", key.app_name)
    };
    let confirm = Confirm {
        title: "Revoke API key?".to_string(),
        message: format!(
            "The program that uses {name} loses its access to the server at once. You cannot \
             undo this."
        ),
        action: "Revoke".to_string(),
        danger: true,
        run: Rc::new(move |this, cx| {
            let token = token.clone();
            this.admin_action("API key revoked", cx, move |client| {
                // The error text of `call` holds the path, and so the key.
                client
                    .call("DELETE", &format!("/Auth/Keys/{token}"), &[])
                    .map_err(|_| anyhow::anyhow!("the key was not revoked"))
            });
        }),
    };
    app.ask_confirm(confirm, cx);
}

/// Asks to revoke the key of a program, by its name. For the debug channel.
pub(crate) fn ask_revoke_named(app: &mut Bloom, name: &str, cx: &mut Context<Bloom>) -> bool {
    let crate::app::Page::Admin(data) = &app.page else {
        return false;
    };
    let key = data
        .api_keys
        .as_ref()
        .and_then(|keys| keys.keys.iter().find(|key| key.app_name == name).cloned());
    match key {
        Some(key) => {
            ask_revoke(app, &key, cx);
            true
        }
        None => false,
    }
}

pub(crate) fn ask_new(app: &mut Bloom, cx: &mut Context<Bloom>) {
    let prompt = Prompt {
        title: "New API key".to_string(),
        message: "Give the name of the program that will use the key. The key gives full access \
                  to the server."
            .to_string(),
        action: "Create key".to_string(),
        fields: vec![Field {
            label: "App name".to_string(),
            placeholder: "Sonarr".to_string(),
            value: String::new(),
            masked: false,
            required: true,
        }],
        run: Rc::new(|this, values, cx| {
            let name = values.into_iter().next().unwrap_or_default();
            create(this, name, cx);
        }),
    };
    app.ask_prompt(prompt, cx);
}

/// Makes the key on the server, then shows it once.
fn create(app: &mut Bloom, name: String, cx: &mut Context<Bloom>) {
    let shown = name.clone();
    app.fetch(
        cx,
        move |client| -> Result<Option<String>> {
            // The server does not answer with the key, so the new one is the
            // key that was not there before.
            let before: Vec<String> = load(&client)?
                .keys
                .into_iter()
                .map(|k| k.access_token)
                .collect();
            client.call("POST", "/Auth/Keys", &[("app", name.clone())])?;
            Ok(load(&client)?
                .keys
                .into_iter()
                .find(|k| k.app_name == name && !before.contains(&k.access_token))
                .map(|k| k.access_token))
        },
        move |this, result, cx| {
            match result {
                Ok(Some(value)) => {
                    this.admin_secret = Some(Secret {
                        title: format!("API key for {shown}"),
                        message: "Copy the key now and keep it in a safe place.".to_string(),
                        value,
                    });
                }
                Ok(None) => this.toast("API key created", "", cx),
                Err(err) => this.toast("The server refused the change", format!("{err:#}"), cx),
            }
            if matches!(this.page, crate::app::Page::Admin(_)) {
                this.load_page(cx);
            }
            cx.notify();
        },
    );
}
