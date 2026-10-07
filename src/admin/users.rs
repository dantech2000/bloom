// SPDX-License-Identifier: AGPL-3.0-or-later
//! Users of the server as a grid of cards: who is an administrator, who is
//! disabled, and when each user was last here. Also holds the small widgets
//! that the devices, activity and API key pages share.

use std::rc::Rc;

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, ObjectFit, ParentElement as _, Rgba,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgb, rgba,
};
use serde::Deserialize;
use serde_json::Value;

use super::{Confirm, Field, Prompt, ago, badge, card, parse_date};
use crate::{
    app::Bloom,
    jellyfin::Client,
    ui::{theme::UiTheme, tip::tip},
    views::cards::icon,
};

/// Narrowest user card; the grid takes as many columns as fit.
const CARD_MIN_W: f32 = 300.;
const GRID_GAP: f32 = 16.;

pub const GREEN: u32 = 0x2e9e5bff;
pub const AMBER: u32 = 0xc98a1bff;
pub const RED: u32 = 0xc62828ff;
pub const BLUE: u32 = 0x3b6fd4ff;
pub const GREY: u32 = 0xffffff2e;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct User {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub primary_image_tag: Option<String>,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default)]
    pub last_login_date: Option<String>,
    #[serde(default)]
    pub last_activity_date: Option<String>,
    /// Whole policy as the server sent it, so a change sends back every field.
    #[serde(default)]
    pub policy: Value,
}

impl User {
    fn flag(&self, name: &str) -> bool {
        self.policy.get(name).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn is_admin(&self) -> bool {
        self.flag("IsAdministrator")
    }

    pub fn is_disabled(&self) -> bool {
        self.flag("IsDisabled")
    }

    pub fn is_hidden(&self) -> bool {
        self.flag("IsHidden")
    }

    pub fn image_url(&self, client: &Client) -> Option<String> {
        self.primary_image_tag
            .as_deref()
            .map(|tag| client.user_image_url(&self.id, tag))
    }
}

pub struct Data {
    pub users: Vec<User>,
}

pub fn load(client: &Client) -> Result<Data> {
    let mut users: Vec<User> = client.get("/Users", &[])?;
    users.sort_by_key(|u| u.name.to_lowercase());
    Ok(Data { users })
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    // The access editor of one user takes the place of the grid.
    if let Some(editor) = app.admin_data().and_then(|d| d.access.as_ref()) {
        return super::access::render(editor, cx);
    }
    let t = UiTheme::read(cx).clone();
    let Some(session) = app.session.as_ref() else {
        return div();
    };
    let client = &session.client;

    let width = content_width(app);
    let columns = (((width + GRID_GAP) / (CARD_MIN_W + GRID_GAP)).floor() as usize).max(1);
    let card_w = ((width - GRID_GAP * (columns - 1) as f32) / columns as f32).floor();

    let admins = data.users.iter().filter(|u| u.is_admin()).count();
    let disabled = data.users.iter().filter(|u| u.is_disabled()).count();
    let open = data.users.iter().filter(|u| !u.has_password).count();

    let mut grid = div().flex().flex_wrap().gap(px(GRID_GAP));
    for user in &data.users {
        let me = user.id == session.user_id;
        let mut badges = div().flex().flex_wrap().gap(px(6.));
        if me {
            badges = badges.child(badge("You", rgba(BLUE)));
        }
        if user.is_admin() {
            badges = badges.child(badge("Admin", rgba(GREY)));
        }
        if user.is_disabled() {
            badges = badges.child(badge("Disabled", rgba(RED)));
        }
        if user.is_hidden() {
            badges = badges.child(badge("Hidden", rgba(GREY)));
        }
        if !user.is_admin() && !user.is_disabled() && !user.is_hidden() && !me {
            badges = badges.child(badge("User", rgba(GREY)));
        }

        let seen = |date: &Option<String>| match date.as_deref().filter(|d| is_real_date(d)) {
            Some(date) => ago(date),
            None => "Never".to_string(),
        };
        let line = |glyph: LucideIcon, label: &'static str, value: String, warn: bool| {
            let color = if warn {
                rgba(0xf0b34aff)
            } else {
                t.colors.foreground.opacity(0.78)
            };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(13.))
                .child(icon(glyph, 14., t.colors.foreground.opacity(0.45)))
                .child(
                    div()
                        .w(px(84.))
                        .flex_shrink_0()
                        .text_color(t.colors.foreground.opacity(0.5))
                        .child(label),
                )
                .child(div().min_w_0().truncate().text_color(color).child(value))
        };

        let mut actions = div().flex().flex_wrap().gap(px(8.)).child({
            let target = user.clone();
            action(format!("users.access.{}", user.id), LucideIcon::ShieldCheck, "Access", false, cx)
                .tooltip(tip("Libraries and rights of this user"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.access_open(&target, cx)))
        });
        // A user cannot disable or delete the account that is signed in here.
        if !me {
            let (label, glyph) = if user.is_disabled() {
                ("Enable", LucideIcon::UserCheck)
            } else {
                ("Disable", LucideIcon::Ban)
            };
            let target = user.clone();
            actions = actions
                .child(
                    action(format!("users.toggle.{}", user.id), glyph, label, false, cx).on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            ask_set_disabled(this, &target, !target.is_disabled(), cx)
                        }),
                    ),
                )
                .child({
                    let target = user.clone();
                    action(
                        format!("users.password.{}", user.id),
                        LucideIcon::KeyRound,
                        "Password",
                        false,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        ask_password(this, &target, cx)
                    }))
                })
                .child({
                    let target = user.clone();
                    action(
                        format!("users.delete.{}", user.id),
                        LucideIcon::Trash,
                        "Delete",
                        true,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        ask_delete(this, &target, cx)
                    }))
                });
        } else {
            actions = actions.child(
                div()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .text_color(t.colors.foreground.opacity(0.45))
                    .child("Signed in on this device"),
            );
        }

        grid = grid.child(
            card(cx)
                .w(px(card_w))
                .gap(px(14.))
                .when(user.is_disabled(), |el| el.opacity(0.62))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(14.))
                        .child(avatar(&user.name, user.image_url(client), 56.))
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(18.))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(t.colors.foreground)
                                        .child(user.name.clone()),
                                )
                                .child(badges),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(line(
                            LucideIcon::Activity,
                            "Last active",
                            seen(&user.last_activity_date),
                            false,
                        ))
                        .child(line(
                            LucideIcon::LogIn,
                            "Last login",
                            seen(&user.last_login_date),
                            false,
                        ))
                        .child(if user.has_password {
                            line(LucideIcon::Lock, "Password", "Set".to_string(), false)
                        } else {
                            line(
                                LucideIcon::LockOpen,
                                "Password",
                                "No password".to_string(),
                                true,
                            )
                        }),
                )
                .child(div().h(px(1.)).bg(rgba(0xf5f5f714)))
                .child(actions),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .child(stat(LucideIcon::Users, "Users", data.users.len(), cx))
                .child(stat(LucideIcon::ShieldCheck, "Administrators", admins, cx))
                .child(stat(LucideIcon::Ban, "Disabled", disabled, cx))
                .child(stat(LucideIcon::LockOpen, "Without password", open, cx))
                .child(div().flex_1())
                .child(
                    div().h(px(56.)).flex().items_center().child(
                        primary("users.new", LucideIcon::UserPlus, "New user", cx).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| ask_new(this, cx)),
                        ),
                    ),
                ),
        )
        .child(grid)
}

fn ask_set_disabled(app: &mut Bloom, user: &User, disable: bool, cx: &mut Context<Bloom>) {
    let (id, name) = (user.id.clone(), user.name.clone());
    let mut policy = user.policy.clone();
    let Some(fields) = policy.as_object_mut() else {
        app.toast("The server sent no policy for this user", "", cx);
        return;
    };
    fields.insert("IsDisabled".to_string(), Value::Bool(disable));
    let confirm = Confirm {
        title: if disable {
            format!("Disable {name}?")
        } else {
            format!("Enable {name}?")
        },
        message: if disable {
            format!("{name} cannot sign in while the account is disabled. The sessions of {name} end.")
        } else {
            format!("{name} can sign in again.")
        },
        action: if disable { "Disable" } else { "Enable" }.to_string(),
        danger: disable,
        run: Rc::new(move |this, cx| {
            let (id, policy) = (id.clone(), policy.clone());
            let done = if disable { "User disabled" } else { "User enabled" };
            this.admin_action(done, cx, move |client| {
                client
                    .post(&format!("/Users/{id}/Policy"), &policy)
                    .map(|_| ())
            });
        }),
    };
    app.ask_confirm(confirm, cx);
}

/// Asks to delete a user, by name. For the debug channel.
/// Opens the password form for the user with this name. For the debug channel.
pub(crate) fn ask_password_named(app: &mut Bloom, name: &str, cx: &mut Context<Bloom>) -> bool {
    let crate::app::Page::Admin(data) = &app.page else {
        return false;
    };
    let user = data
        .users
        .as_ref()
        .and_then(|users| users.users.iter().find(|user| user.name == name).cloned());
    match user {
        Some(user) => {
            ask_password(app, &user, cx);
            true
        }
        None => false,
    }
}

pub(crate) fn ask_delete_named(app: &mut Bloom, name: &str, cx: &mut Context<Bloom>) -> bool {
    let crate::app::Page::Admin(data) = &app.page else {
        return false;
    };
    let user = data
        .users
        .as_ref()
        .and_then(|users| users.users.iter().find(|user| user.name == name).cloned());
    match user {
        Some(user) => {
            ask_delete(app, &user, cx);
            true
        }
        None => false,
    }
}

pub(crate) fn ask_new(app: &mut Bloom, cx: &mut Context<Bloom>) {
    let prompt = Prompt {
        title: "New user".to_string(),
        message: "The user can sign in at once and sees every library. Use Access on the card \
                  of the user to change that."
            .to_string(),
        action: "Create user".to_string(),
        fields: vec![
            Field {
                label: "Name".to_string(),
                placeholder: "Name".to_string(),
                value: String::new(),
                masked: false,
                required: true,
            },
            Field {
                label: "Password".to_string(),
                placeholder: "Password (can be empty)".to_string(),
                value: String::new(),
                masked: true,
                required: false,
            },
        ],
        run: Rc::new(|this, values, cx| {
            let mut values = values.into_iter();
            let body = serde_json::json!({
                "Name": values.next().unwrap_or_default(),
                "Password": values.next().unwrap_or_default(),
            });
            this.admin_action("User created", cx, move |client| {
                client.post("/Users/New", &body).map(|_| ())
            });
        }),
    };
    app.ask_prompt(prompt, cx);
}

fn ask_password(app: &mut Bloom, user: &User, cx: &mut Context<Bloom>) {
    let (id, name) = (user.id.clone(), user.name.clone());
    let prompt = Prompt {
        title: format!("New password for {name}"),
        message: format!("{name} signs in with the new password from now on."),
        action: "Set password".to_string(),
        fields: vec![Field {
            label: "New password".to_string(),
            placeholder: "Password".to_string(),
            value: String::new(),
            masked: true,
            required: true,
        }],
        run: Rc::new(move |this, values, cx| {
            let id = id.clone();
            let body = serde_json::json!({ "NewPw": values.into_iter().next().unwrap_or_default() });
            this.admin_action("Password changed", cx, move |client| {
                // The id is a hex number, so it needs no escape.
                let path = format!("/Users/Password?userId={id}");
                client.post(&path, &body).map(|_| ())
            });
        }),
    };
    app.ask_prompt(prompt, cx);
}

fn ask_delete(app: &mut Bloom, user: &User, cx: &mut Context<Bloom>) {
    let (id, name) = (user.id.clone(), user.name.clone());
    let confirm = Confirm {
        title: format!("Delete {name}?"),
        message: format!(
            "The account of {name}, with its watch history and settings, is removed from the \
             server. You cannot undo this."
        ),
        action: "Delete user".to_string(),
        danger: true,
        run: Rc::new(move |this, cx| {
            let id = id.clone();
            this.admin_action("User deleted", cx, move |client| {
                client.call("DELETE", &format!("/Users/{id}"), &[])
            });
        }),
    };
    app.ask_confirm(confirm, cx);
}

// ----- widgets shared with the devices, activity and API key pages ------------

/// Width the page content has, beside the sidebar and inside the padding.
pub(super) fn content_width(app: &Bloom) -> f32 {
    (app.viewport_w - 236. - 56.).max(320.)
}

/// False for the "year 1" date the server sends for "never".
pub(super) fn is_real_date(date: &str) -> bool {
    !date.starts_with("0001-")
}

/// A server date in the local time zone.
pub(super) fn local(date: &str) -> Option<jiff::Zoned> {
    parse_date(date).map(|ts| ts.to_zoned(jiff::tz::TimeZone::system()))
}

/// "Oct 3, 2026, 10:41 AM" in local time.
pub(super) fn absolute(date: &str) -> String {
    local(date)
        .map(|z| z.strftime("%b %-d, %Y, %-I:%M %p").to_string())
        .unwrap_or_default()
}

/// Round picture of a user: the profile image, or the first letter of the
/// name on the quiet disc of the theme.
pub(super) fn avatar(name: &str, image: Option<String>, size: f32) -> Div {
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    div()
        .relative()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(super::DISC))
        .text_size(px(size * 0.42))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(rgb(0xf5f5f7))
        .when(image.is_none(), |el| el.child(initial))
        .when_some(image, |el, url| {
            el.child(
                div()
                    .absolute()
                    .inset_0()
                    .child(crate::images::remote_with(url, px(size / 2.), ObjectFit::Cover).size_full()),
            )
        })
}

/// Round icon on the quiet disc of the theme. The colour shows only when
/// it stands for a state (see `state_color`).
pub(super) fn icon_disc(glyph: LucideIcon, color: Rgba, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(super::DISC))
        .child(icon(
            glyph,
            size * 0.48,
            super::state_color(color).unwrap_or(rgb(0xf5f5f7)),
        ))
}

/// A number with its label, for the summary line at the top of a page.
pub(super) fn stat(
    glyph: LucideIcon,
    label: &'static str,
    value: usize,
    cx: &Context<Bloom>,
) -> Div {
    let t = UiTheme::read(cx);
    div()
        .h(px(56.))
        .px(px(16.))
        .map(super::surface)
        .flex()
        .items_center()
        .gap(px(12.))
        .child(icon(glyph, 18., t.colors.foreground.opacity(0.6)))
        .child(
            div()
                .text_size(px(22.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child(value.to_string()),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(t.colors.foreground.opacity(0.6))
                .child(label),
        )
}

/// Small button with an icon, for the actions of one row or card.
/// Add `.on_click(...)`.
pub(super) fn action(
    id: impl Into<SharedString>,
    glyph: LucideIcon,
    label: &'static str,
    danger: bool,
    cx: &Context<Bloom>,
) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    let color = if danger {
        rgba(0xff8a80ff)
    } else {
        t.colors.foreground
    };
    let hover = if danger {
        rgba(0xc6282859)
    } else {
        rgba(0xffffff2e)
    };
    div()
        .id(id.into())
        .h(px(30.))
        .px(px(12.))
        .flex_shrink_0()
        .rounded(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .cursor_pointer()
        .bg(rgba(0xffffff14))
        .text_size(px(13.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(color)
        .hover(move |s| s.bg(hover))
        .child(icon(glyph, 14., color))
        .child(label)
}

/// The primary button of a page with an icon. Add `.on_click(...)`.
pub(super) fn primary(
    id: &'static str,
    glyph: LucideIcon,
    label: &'static str,
    cx: &Context<Bloom>,
) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    let color = t.colors.primary_foreground;
    // `super::button` puts its label first; here the icon leads.
    div()
        .id(id)
        .h(px(36.))
        .px(px(16.))
        .rounded(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .bg(t.colors.primary)
        .text_color(color)
        .text_size(px(14.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .hover(|s| s.opacity(0.88))
        .child(icon(glyph, 16., color))
        .child(label)
}
