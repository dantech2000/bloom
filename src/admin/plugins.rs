// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plugins: a card for each installed plugin with its image, version and
//! status. A card has one button, its settings, and a "…" menu with the
//! rest: enable or disable, the web settings, uninstall (2026-10-23; four
//! buttons on every card before).

use crate::ui::tone::State;
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::Result;
use gpui_kit::{
    ClickEvent, Context, Div, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgba,
};
use serde::Deserialize;

use gpui_icons::LucideIcon;

use crate::{
    admin::{ButtonKind, Confirm, badge, button},
    app::Bloom,
    jellyfin::Client,
    ui::{menu::MenuItem, theme::UiTheme},
};

/// Smallest width of a plugin card; the grid fits as many as the page holds.
const CARD_MIN_W: f32 = 300.;
const CARD_GAP: f32 = 18.;
/// Height of the image at the top of a card.
const IMAGE_H: f32 = 150.;

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

/// How long an answer to "has this plugin settings?" holds. The page loads
/// again after each action on a plugin, and asked every plugin again each
/// time, whole configurations for a yes or no.
const PROBE_HOLDS: Duration = Duration::from_secs(60);

/// What the last loads learned: the server asked, when, and for each
/// plugin (id and version) whether it has settings.
struct Probed {
    server: String,
    at: Instant,
    settings: HashMap<(String, String), bool>,
}

static PROBED: Mutex<Option<Probed>> = Mutex::new(None);

impl Probed {
    /// The answers that still hold for this server; none after the time.
    fn known(&self, server: &str, now: Instant) -> Option<&HashMap<(String, String), bool>> {
        (self.server == server && now.duration_since(self.at) < PROBE_HOLDS).then_some(&self.settings)
    }
}

pub fn load(client: &Client) -> Result<Data> {
    let mut plugins: Vec<Plugin> = client.get("/Plugins", &[])?;
    plugins.sort_by_key(|p| p.name.to_lowercase());
    let server = client.url("", &[]);
    let now = Instant::now();
    let mut known = PROBED
        .lock()
        .ok()
        .and_then(|probed| probed.as_ref()?.known(&server, now).cloned())
        .unwrap_or_default();
    // A plugin has settings when the server answers its configuration with
    // an object that is not empty. A plugin with none answers 404 or {}.
    // One request for each plugin that runs and is not known yet, all at
    // the same time.
    let asked: Vec<((String, String), bool)> = std::thread::scope(|scope| {
        let asks: Vec<_> = plugins
            .iter()
            .filter(|p| matches!(p.status.as_str(), "Active" | "Restart"))
            .map(|p| (p.id.clone(), p.version.clone()))
            .filter(|key| !known.contains_key(key))
            .map(|key| {
                let path = format!("/Plugins/{}/Configuration", key.0);
                (key, scope.spawn(move || client.get::<serde_json::Value>(&path, &[])))
            })
            .collect();
        asks.into_iter()
            .map(|(key, ask)| {
                let has = ask
                    .join()
                    .ok()
                    .and_then(Result::ok)
                    .is_some_and(|value| value.as_object().is_some_and(|o| !o.is_empty()));
                (key, has)
            })
            .collect()
    });
    let fresh = known.is_empty();
    known.extend(asked);
    let settings = plugins
        .iter()
        .filter(|p| matches!(p.status.as_str(), "Active" | "Restart"))
        .filter(|p| known.get(&(p.id.clone(), p.version.clone())).copied().unwrap_or(false))
        .map(|p| p.id.clone())
        .collect();
    if let Ok(mut probed) = PROBED.lock() {
        // The time runs from the first answer, so every answer is asked
        // for again within it.
        let at = match probed.as_ref() {
            Some(old) if !fresh && old.server == server => old.at,
            _ => now,
        };
        *probed = Some(Probed { server, at, settings: known });
    }
    Ok(Data { plugins, settings })
}

/// What the "…" menu of a plugin needs to know.
#[derive(Clone)]
struct MenuPlugin {
    id: String,
    version: String,
    name: String,
    status: String,
    can_uninstall: bool,
    web_url: String,
}

impl MenuPlugin {
    fn toggle(&self) -> Option<&'static str> {
        match self.status.as_str() {
            "Active" | "Restart" => Some("Disable"),
            "Disabled" => Some("Enable"),
            _ => None,
        }
    }

    fn has_entries(&self) -> bool {
        self.toggle().is_some() || self.status == "Active" || (self.can_uninstall && self.status != "Deleted")
    }

    /// The entries of the menu: enable or disable, the settings page of
    /// the plugin in the web, and uninstall last, after a line.
    fn items(&self, cx: &Context<Bloom>) -> Vec<MenuItem> {
        let handle = cx.weak_entity();
        let mut items = Vec::new();
        if let Some(verb) = self.toggle() {
            let (handle, plugin) = (handle.clone(), self.clone());
            items.push(
                MenuItem::new("admin.plugins.menu.toggle", verb)
                    .icon(if verb == "Enable" { LucideIcon::Power } else { LucideIcon::PowerOff })
                    .detail("Applies after the next server restart")
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |this, cx| plugin.ask_toggle(this, verb, cx)).ok();
                    }),
            );
        }
        if self.status == "Active" {
            let url = self.web_url.clone();
            items.push(
                MenuItem::new("admin.plugins.menu.web", "Open settings in the web")
                    .icon(LucideIcon::ExternalLink)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        if self.can_uninstall && self.status != "Deleted" {
            if !items.is_empty() {
                items.push(MenuItem::separator());
            }
            let (handle, plugin) = (handle.clone(), self.clone());
            items.push(
                MenuItem::new("admin.plugins.menu.uninstall", "Uninstall…")
                    .icon(LucideIcon::Trash)
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |this, cx| plugin.ask_uninstall(this, cx)).ok();
                    }),
            );
        }
        items
    }

    fn ask_toggle(&self, app: &mut Bloom, verb: &'static str, cx: &mut Context<Bloom>) {
        let (id, version) = (self.id.clone(), self.version.clone());
        app.ask_confirm(
            Confirm {
                title: format!("{verb} {}?", self.name),
                message: "The change applies after the next server restart.".to_string(),
                action: verb.to_string(),
                danger: false,
                run: Rc::new(move |this, cx| {
                    let (id, version) = (id.clone(), version.clone());
                    let done = if verb == "Enable" { "Plugin enabled" } else { "Plugin disabled" };
                    this.admin_action(done, cx, move |client| {
                        client.call("POST", &format!("/Plugins/{id}/{version}/{verb}"), &[])
                    })
                }),
            },
            cx,
        );
    }

    fn ask_uninstall(&self, app: &mut Bloom, cx: &mut Context<Bloom>) {
        let (id, version) = (self.id.clone(), self.version.clone());
        app.ask_confirm(
            Confirm {
                title: format!("Uninstall {}?", self.name),
                message: format!(
                    "Version {version} and its settings are removed from the server. The change \
                     applies after the next server restart."
                ),
                action: "Uninstall".to_string(),
                danger: true,
                run: Rc::new(move |this, cx| {
                    let (id, version) = (id.clone(), version.clone());
                    this.admin_action("Plugin uninstalled", cx, move |client| {
                        client.call("DELETE", &format!("/Plugins/{id}/{version}"), &[])
                    })
                }),
            },
            cx,
        );
    }
}

/// Text and state of the badge for a plugin status.
fn status_badge(status: &str) -> (&'static str, Option<State>) {
    match status {
        "Active" => ("Active", Some(State::Ok)),
        "Restart" => ("Restart needed", Some(State::Warn)),
        "Disabled" => ("Disabled", None),
        "Malfunctioned" => ("Malfunctioned", Some(State::Bad)),
        "NotSupported" => ("Not supported", Some(State::Bad)),
        "Superseded" => ("Superseded", None),
        "Deleted" => ("Removed at restart", None),
        _ => ("Unknown", None),
    }
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let Some(client) = app.session.as_ref().map(|s| s.client.clone()) else {
        return div();
    };
    let content_w = app.admin_content_w().max(CARD_MIN_W);
    let (_, card_w) = crate::ui::rows::grid(content_w, CARD_MIN_W, CARD_GAP, 1, usize::MAX);

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
    if data.plugins.is_empty() {
        grid = grid.child(
            super::card(cx)
                .text_size(px(15.))
                .text_color(t.colors.foreground.opacity(0.6))
                .child("No plugins are installed."),
        );
    }
    for plugin in &data.plugins {
        let (status, status_state) = status_badge(&plugin.status);
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

        // One action on the card, the settings when the plugin has any in
        // the app; the rest in the "…" menu (enable or disable, the web
        // settings, uninstall last).
        let mut actions = div().flex().items_center().gap(px(8.));
        if data.settings.contains(&plugin.id) {
            let (id, name, version) = (plugin.id.clone(), plugin.name.clone(), plugin.version.clone());
            actions = actions.child(
                button(
                    // With the version: a plugin can be installed twice (one
                    // superseded), and two elements of one id abort a debug build.
                    SharedString::from(format!("admin.plugins.settings.{}.{}", plugin.id, plugin.version)),
                    "Settings",
                    ButtonKind::Plain,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_plugin_config(id.clone(), name.clone(), version.clone(), window, cx)
                })),
            );
        }
        let menu_plugin = MenuPlugin {
            id: plugin.id.clone(),
            version: plugin.version.clone(),
            name: plugin.name.clone(),
            status: plugin.status.clone(),
            can_uninstall: plugin.can_uninstall,
            web_url: client.web_plugin_url(&plugin.id),
        };
        if menu_plugin.has_entries() {
            actions = actions.child(div().flex_1()).child(
                super::more_button(
                    SharedString::from(format!("admin.plugins.more.{}.{}", plugin.id, plugin.version)),
                    cx,
                )
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        let items = menu_plugin.items(cx);
                        this.open_admin_menu(items, event.position(), window, cx);
                    })),
            );
        }

        grid = grid.child(
            div()
                .w(px(card_w))
                // A set least and most width spares a layout pass (`Bloom::render_settings`).
                .min_w(px(card_w))
                .max_w(px(card_w))
                .map(super::surface)
                .overflow_hidden()
                .flex()
                .flex_col()
                .when(
                    matches!(plugin.status.as_str(), "Disabled" | "Superseded" | "Deleted"),
                    |el| el.opacity(0.62),
                )
                // The image, over a quiet tile with the first letter. The
                // tile stays when the plugin has no image or it cannot load.
                .child(super::libraries::image_header(
                    div().h(px(IMAGE_H)).bg(rgba(0xffffff0f)),
                    div()
                        .text_size(px(56.))
                        .font_weight(gpui_kit::FontWeight::BOLD)
                        .text_color(rgba(0xffffff59))
                        .child(initial),
                    image,
                    badge(status, status_state),
                ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_holds_for_its_server_and_its_time() {
        let at = Instant::now();
        let probed = Probed {
            server: "http://a".to_string(),
            at,
            settings: HashMap::from([(("p".to_string(), "1.0".to_string()), true)]),
        };
        assert!(probed.known("http://a", at + Duration::from_secs(5)).is_some());
        assert!(probed.known("http://b", at + Duration::from_secs(5)).is_none());
        assert!(probed.known("http://a", at + PROBE_HOLDS).is_none());
    }
}
