// SPDX-License-Identifier: AGPL-3.0-or-later
//! Devices that have signed in to the server, newest activity first.

use std::rc::Rc;

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgba,
};
use serde::Deserialize;

use super::{
    Confirm, ago, badge, card, parse_date,
    rows::rows,
    users::{BLUE, GREEN, absolute, action, avatar, content_width, icon_disc, is_real_date, stat},
};
use crate::{app::Bloom, jellyfin::Client, ui::theme::UiTheme};

/// A device with activity inside this time counts as active now.
const ACTIVE_SECS: i64 = 5 * 60;
/// Height of a row of the list, with the line above it.
const ROW_H: f32 = 65.;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Device {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Name the administrator gave the device, when there is one.
    #[serde(default)]
    pub custom_name: Option<String>,
    #[serde(default)]
    pub app_name: String,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub last_user_name: Option<String>,
    #[serde(default)]
    pub date_last_activity: Option<String>,
}

impl Device {
    fn title(&self) -> &str {
        self.custom_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.name)
    }

    /// Seconds since the last activity.
    fn idle_secs(&self) -> Option<i64> {
        let then = parse_date(self.date_last_activity.as_deref()?)?;
        Some(jiff::Timestamp::now().as_second() - then.as_second())
    }

    fn glyph(&self) -> LucideIcon {
        let app = self.app_name.to_lowercase();
        if app.contains("tv") || app.contains("roku") || app.contains("kodi") {
            LucideIcon::Tv
        } else if app.contains("ios")
            || app.contains("android")
            || app.contains("mobile")
            || app.contains("swiftfin")
            || app.contains("finamp")
        {
            LucideIcon::Smartphone
        } else if app.contains("web") {
            LucideIcon::Globe
        } else if app.contains("jellyui") || app.contains(crate::brand::FOLDER) || app.contains("desktop") || app.contains("media player")
        {
            LucideIcon::Laptop
        } else {
            LucideIcon::MonitorSmartphone
        }
    }
}

pub struct Data {
    pub devices: Vec<Device>,
}

pub fn load(client: &Client) -> Result<Data> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Page {
        #[serde(default)]
        items: Vec<Device>,
    }
    let mut devices = client.get::<Page>("/Devices", &[])?.items;
    // Server dates sort as text.
    devices.sort_by(|a, b| b.date_last_activity.cmp(&a.date_last_activity));
    Ok(Data { devices })
}

pub fn render(_app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let active = data
        .devices
        .iter()
        .filter(|d| d.idle_secs().is_some_and(|s| s < ACTIVE_SECS))
        .count();
    let today = data
        .devices
        .iter()
        .filter(|d| d.idle_secs().is_some_and(|s| s < 86_400))
        .count();

    // Only the rows in view are built; see `rows`.
    let list = card(cx).p(px(6.)).child(rows(cx, data.devices.len(), ROW_H, |this, range, cx| {
        let Some((session, data)) = this
            .session
            .as_ref()
            .zip(this.admin_data().and_then(|d| d.devices.as_ref()))
        else {
            return Vec::new();
        };
        let width = content_width(this);
        let (show_user, show_app) = (width >= 620., width >= 820.);
        range
            .map(|index| {
                let device = &data.devices[index];
                let current = *device.id == *session.client.device_id;
                div()
                    .h(px(ROW_H))
                    .flex()
                    .flex_col()
                    // The line between two rows.
                    .child(
                        div()
                            .mx(px(12.))
                            .h(px(1.))
                            .when(index > 0, |el| el.bg(rgba(0xf5f5f70f))),
                    )
                    .child(row(device, index, current, show_user, show_app, cx))
                    .into_any_element()
            })
            .collect()
    }));

    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .child(stat(LucideIcon::MonitorSmartphone, "Devices", data.devices.len(), cx))
                .child(stat(LucideIcon::Activity, "Active now", active, cx))
                .child(stat(LucideIcon::Clock, "Seen in the last day", today, cx)),
        )
        .child(list)
}

/// One device of the list.
fn row(
    device: &Device,
    index: usize,
    current: bool,
    show_user: bool,
    show_app: bool,
    cx: &mut Context<Bloom>,
) -> Div {
    let t = UiTheme::read(cx).clone();
    let is_active = device.idle_secs().is_some_and(|s| s < ACTIVE_SECS);
    let date = device
        .date_last_activity
        .as_deref()
        .filter(|d| is_real_date(d));
    let app_line = format!("{} {}", device.app_name, device.app_version);
    let user = device.last_user_name.clone().unwrap_or_default();

    let mut row = div()
        .h(px(64.))
        .px(px(12.))
        .rounded(px(12.))
        .flex()
        .items_center()
        .gap(px(14.))
        .hover(|s| s.bg(rgba(0xffffff0a)))
        .child(
            div()
                .relative()
                .child(icon_disc(
                    device.glyph(),
                    if is_active {
                        rgba(GREEN)
                    } else {
                        t.colors.foreground.opacity(0.7)
                    },
                    40.,
                ))
                // Dot for a device that is in use now.
                .when(is_active, |el| {
                    el.child(
                        div()
                            .absolute()
                            .right(px(-1.))
                            .bottom(px(-1.))
                            .size(px(12.))
                            .rounded_full()
                            .border_2()
                            .border_color(t.colors.background)
                            .bg(rgba(0x3ddc84ff)),
                    )
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(15.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child(device.title().to_string()),
                        )
                        .when(current, |el| el.child(badge("This device", rgba(BLUE)))),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(13.))
                        .text_color(t.colors.foreground.opacity(0.55))
                        .child(if show_app {
                            date.map(absolute).unwrap_or_default()
                        } else {
                            app_line.clone()
                        }),
                ),
        );
    if show_app {
        row = row.child(
            div()
                .w(px(210.))
                .flex_shrink_0()
                .truncate()
                .text_size(px(14.))
                .text_color(t.colors.foreground.opacity(0.8))
                .child(app_line),
        );
    }
    if show_user {
        row = row.child(
            div()
                .w(px(150.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .when(!user.is_empty(), |el| {
                    el.child(avatar(&user, None, 24.)).child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .text_color(t.colors.foreground.opacity(0.8))
                            .child(user.clone()),
                    )
                }),
        );
    }
    row = row.child(
        div()
            .w(px(120.))
            .flex_shrink_0()
            .text_size(px(13.))
            .text_color(if is_active {
                rgba(0x6ee7a0ff)
            } else {
                t.colors.foreground.opacity(0.6)
            })
            .child(match (is_active, date) {
                (true, _) => "Active now".to_string(),
                (false, Some(date)) => ago(date),
                (false, None) => "Never".to_string(),
            }),
    );
    // This device cannot delete itself: that signs the app out.
    row = row.child(div().w(px(92.)).flex_shrink_0().flex().justify_end().when(
        !current,
        |el| {
            let target = device.clone();
            el.child(
                action(
                    format!("devices.delete.{index}"),
                    LucideIcon::Trash,
                    "Delete",
                    true,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    ask_delete(this, &target, cx)
                })),
            )
        },
    ));
    row
}

fn ask_delete(app: &mut Bloom, device: &Device, cx: &mut Context<Bloom>) {
    let id = device.id.clone();
    let name = device.title().to_string();
    let confirm = Confirm {
        title: format!("Delete {name}?"),
        message: format!(
            "{} on {name} is signed out. The device shows again after its next sign-in.",
            device.app_name
        ),
        action: "Delete device".to_string(),
        danger: true,
        run: Rc::new(move |this, cx| {
            let id = id.clone();
            this.admin_action("Device deleted", cx, move |client| {
                client.call("DELETE", "/Devices", &[("id", id)])
            });
        }),
    };
    app.ask_confirm(confirm, cx);
}
