// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plugins: a card for each installed plugin with its image, version and
//! status. A plugin can be enabled, disabled or uninstalled.

use std::{collections::HashSet, rc::Rc};

use anyhow::Result;
use gpui_kit::{
    ClickEvent, Context, Div, ObjectFit, ParentElement as _, Rgba,
    SharedString, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgb, rgba,
};
use serde::Deserialize;

use crate::{
    admin::{ButtonKind, Confirm, badge, button},
    app::Bloom,
    jellyfin::Client,
    ui::theme::UiTheme,
};

/// Smallest width of a plugin card; the grid fits as many as the page holds.
const CARD_MIN_W: f32 = 300.;
const CARD_GAP: f32 = 18.;
/// Height of the image at the top of a card.
const IMAGE_H: f32 = 150.;
/// Width of the sidebar plus the page padding at both sides.
const PAGE_CHROME: f32 = 236. + 56.;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Plugin {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub can_uninstall: bool,
    #[serde(default)]
    pub has_image: bool,
    /// "Active", "Restart", "Disabled", "Malfunctioned", "NotSupported",
    /// "Superseded" or "Deleted".
    #[serde(default)]
    pub status: String,
}

pub struct Data {
    pub plugins: Vec<Plugin>,
    /// Ids of the plugins that have stored settings the editor can show.
    pub settings: HashSet<String>,
}

pub fn load(client: &Client) -> Result<Data> {
    let mut plugins: Vec<Plugin> = client.get("/Plugins", &[])?;
    plugins.sort_by_key(|p| p.name.to_lowercase());
    // A plugin has settings when the server answers its configuration with
    // an object that is not empty. A plugin with none answers 404 or {}.
    // One request for each plugin that runs, all at the same time.
    let settings = std::thread::scope(|scope| {
        let asks: Vec<_> = plugins
            .iter()
            .filter(|p| matches!(p.status.as_str(), "Active" | "Restart"))
            .map(|p| {
                let path = format!("/Plugins/{}/Configuration", p.id);
                (p.id.clone(), scope.spawn(move || client.get::<serde_json::Value>(&path, &[])))
            })
            .collect();
        asks.into_iter()
            .filter_map(|(id, ask)| {
                let value = ask.join().ok()?.ok()?;
                value.as_object().is_some_and(|o| !o.is_empty()).then_some(id)
            })
            .collect::<HashSet<String>>()
    });
    Ok(Data { plugins, settings })
}

/// Text and colour of the badge for a plugin status.
fn status_badge(status: &str) -> (&'static str, Rgba) {
    match status {
        "Active" => ("Active", rgb(0x2e7d32)),
        "Restart" => ("Restart needed", rgb(0xb26a00)),
        "Disabled" => ("Disabled", rgb(0x616161)),
        "Malfunctioned" => ("Malfunctioned", rgb(0xc62828)),
        "NotSupported" => ("Not supported", rgb(0xc62828)),
        "Superseded" => ("Superseded", rgb(0x616161)),
        "Deleted" => ("Removed at restart", rgb(0x616161)),
        _ => ("Unknown", rgb(0x616161)),
    }
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let Some(client) = app.session.as_ref().map(|s| s.client.clone()) else {
        return div();
    };
    let content_w = (app.viewport_w - PAGE_CHROME).max(CARD_MIN_W);
    let columns = (((content_w + CARD_GAP) / (CARD_MIN_W + CARD_GAP)).floor() as usize).max(1);
    let card_w = ((content_w - CARD_GAP * (columns - 1) as f32) / columns as f32).floor();

    let active = data.plugins.iter().filter(|p| p.status == "Active").count();
    let restart = data
        .plugins
        .iter()
        .filter(|p| matches!(p.status.as_str(), "Restart" | "Deleted"))
        .count();
    let mut summary = format!("{} installed · {active} active", data.plugins.len());
    if restart > 0 {
        summary.push_str(&format!(" · {restart} wait for a server restart"));
    }

    let mut grid = div().flex().flex_wrap().gap(px(CARD_GAP));
    for plugin in &data.plugins {
        let (status, status_color) = status_badge(&plugin.status);
        let image = plugin.has_image.then(|| {
            client.url(
                &format!("/Plugins/{}/{}/Image", plugin.id, plugin.version),
                &[],
            )
        });
        let initial: String = plugin
            .name
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_default();

        let mut actions = div().flex().flex_wrap().gap(px(8.));
        // A plugin that is on can be turned off, and the other way round.
        let toggle = match plugin.status.as_str() {
            "Active" | "Restart" => Some("Disable"),
            "Disabled" => Some("Enable"),
            _ => None,
        };
        if let Some(verb) = toggle {
            let (id, version, name) = (
                plugin.id.clone(),
                plugin.version.clone(),
                plugin.name.clone(),
            );
            actions = actions.child(
                button(
                    SharedString::from(format!("admin.plugins.toggle.{}", plugin.id)),
                    verb,
                    ButtonKind::Plain,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let (id, version) = (id.clone(), version.clone());
                    this.ask_confirm(
                        Confirm {
                            title: format!("{verb} {name}?"),
                            message: "The change applies after the next server restart."
                                .to_string(),
                            action: verb.to_string(),
                            danger: false,
                            run: Rc::new(move |this, cx| {
                                let (id, version) = (id.clone(), version.clone());
                                let done = if verb == "Enable" {
                                    "Plugin enabled"
                                } else {
                                    "Plugin disabled"
                                };
                                this.admin_action(done, cx, move |client| {
                                    client.call(
                                        "POST",
                                        &format!("/Plugins/{id}/{version}/{verb}"),
                                        &[],
                                    )
                                })
                            }),
                        },
                        cx,
                    )
                })),
            );
        }
        // The editor of the stored settings, when the plugin has any.
        if data.settings.contains(&plugin.id) {
            let (id, name, version) = (plugin.id.clone(), plugin.name.clone(), plugin.version.clone());
            actions = actions.child(
                button(
                    SharedString::from(format!("admin.plugins.settings.{}", plugin.id)),
                    "Settings",
                    ButtonKind::Plain,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_plugin_config(id.clone(), name.clone(), version.clone(), window, cx)
                })),
            );
        }
        // The settings page of a plugin is a web page of the plugin itself.
        if plugin.status == "Active" {
            let url = client.web_plugin_url(&plugin.id);
            actions = actions.child(
                button(
                    SharedString::from(format!("admin.plugins.web.{}", plugin.id)),
                    "Open settings in the web",
                    ButtonKind::Plain,
                    cx,
                )
                .on_click(move |_: &ClickEvent, _, cx| cx.open_url(&url)),
            );
        }
        if plugin.can_uninstall && plugin.status != "Deleted" {
            let (id, version, name) = (
                plugin.id.clone(),
                plugin.version.clone(),
                plugin.name.clone(),
            );
            actions = actions.child(
                button(
                    SharedString::from(format!("admin.plugins.uninstall.{}", plugin.id)),
                    "Uninstall",
                    ButtonKind::Plain,
                    cx,
                )
                .text_color(rgb(0xef5350))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let (id, version) = (id.clone(), version.clone());
                    this.ask_confirm(
                        Confirm {
                            title: format!("Uninstall {name}?"),
                            message: format!(
                                "Version {version} and its settings are removed from the \
                                 server. The change applies after the next server restart."
                            ),
                            action: "Uninstall".to_string(),
                            danger: true,
                            run: Rc::new(move |this, cx| {
                                let (id, version) = (id.clone(), version.clone());
                                this.admin_action("Plugin uninstalled", cx, move |client| {
                                    client.call(
                                        "DELETE",
                                        &format!("/Plugins/{id}/{version}"),
                                        &[],
                                    )
                                })
                            }),
                        },
                        cx,
                    )
                })),
            );
        }

        grid = grid.child(
            div()
                .w(px(card_w))
                .rounded(px(24.))
                .border_1()
                .border_color(rgba(0xf5f5f733))
                .bg(rgba(0x2a2a2ab0))
                .overflow_hidden()
                .flex()
                .flex_col()
                .when(plugin.status == "Disabled", |el| el.opacity(0.7))
                // The image, over a coloured tile with the first letter. The
                // tile stays when the plugin has no image or it cannot load.
                .child(
                    div()
                        .relative()
                        .w(px(card_w))
                        .h(px(IMAGE_H))
                        .bg(rgba(0xffffff0f))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_size(px(56.))
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .text_color(rgba(0xffffff59))
                                .child(initial),
                        )
                        .when_some(image, |el, url| {
                            el.child(
                                crate::images::remote_with(url, px(0.), ObjectFit::Cover)
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .w(px(card_w))
                                    .h(px(IMAGE_H)),
                            )
                        })
                        .child(
                            div()
                                .absolute()
                                .top(px(10.))
                                .right(px(10.))
                                .child(badge(status, status_color).bg(rgba(0x000000a6))),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .p(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .justify_between()
                                .gap(px(10.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(17.))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(t.colors.foreground)
                                        .child(plugin.name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .font_family("Menlo")
                                        .text_size(px(12.))
                                        .text_color(t.colors.muted_foreground)
                                        .child(format!("v{}", plugin.version)),
                                ),
                        )
                        .child(
                            div()
                                .h(px(40.))
                                .line_clamp(2)
                                .text_size(px(14.))
                                .line_height(px(20.))
                                .text_color(t.colors.foreground.opacity(0.7))
                                .child(if plugin.description.is_empty() {
                                    "No description.".to_string()
                                } else {
                                    plugin.description.clone()
                                }),
                        )
                        .child(div().flex_1())
                        .child(actions),
                ),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .child(
            div()
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child(summary),
        )
        .child(grid)
}
