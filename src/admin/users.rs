// SPDX-License-Identifier: AGPL-3.0-or-later
//! Users of the server as a list of rows, each opening the page of one
//! user with its facts and actions (2026-10-23; a grid of cards with four
//! buttons each before). Also holds the small widgets that the devices,
//! activity and API key pages share.

use crate::ui::tone::{self, State};
use std::{rc::Rc, sync::Arc};

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, ObjectFit, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgba,
};
use serde::Deserialize;
use serde_json::Value;

use super::{Confirm, Field, Prompt, ago, badge, find_field, nothing_found, parse_date};
use crate::{
    app::Bloom,
    jellyfin::Client,
    ui::theme::UiTheme,
    icons::icon,
};


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
    /// The policy as the list of the users gave it, for the badges. A
    /// change reads the policy from the server again and sends that back.
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
    /// Shared, so the click handlers of a page take a user without a copy
    /// of its policy.
    pub users: Vec<Arc<User>>,
}

pub fn load(client: &Client) -> Result<Data> {
    let mut users: Vec<User> = client.get("/Users", &[])?;
    users.sort_by_key(|u| u.name.to_lowercase());
    Ok(Data { users: users.into_iter().map(Arc::new).collect() })
}

/// The user of the Users page with this name. For the debug channel.
pub(super) fn user_named(app: &Bloom, name: &str) -> Option<Arc<User>> {
    app.admin_data()?.users.as_ref()?.users.iter().find(|user| user.name == name).cloned()
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    // The access editor of one user takes the place of the page.
    if let Some(editor) = app.admin_data().and_then(|d| d.access.as_ref()) {
        return super::access::render(editor, cx);
    }
    let Some(session) = app.session.as_ref() else {
        return div();
    };
    // One user open: their page, with the facts and the actions.
    let open = app
        .admin_data()
        .and_then(|d| d.user_open.as_ref())
        .and_then(|id| data.users.iter().find(|user| &user.id == id));
    match open {
        Some(user) => render_user(app, user, session, cx),
        None => render_list(app, data, session, cx),
    }
}

/// The users as a list of rows: a row is a name with what matters at a
/// glance, and opens the user's page. The grid of cards before had four
/// buttons on each of ten cards, and on a phone one card was a screen.
fn render_list(app: &Bloom, data: &Data, session: &crate::app::Session, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let compact = app.compact_w();
    let client = &session.client;

    let admins = data.users.iter().filter(|u| u.is_admin()).count();
    let disabled = data.users.iter().filter(|u| u.is_disabled()).count();
    let open = data.users.iter().filter(|u| !u.has_password).count();

    let query = app.admin_users_filter.read(cx).value().trim().to_lowercase();
    let shown: Vec<&Arc<User>> = data
        .users
        .iter()
        .filter(|user| query.is_empty() || user.name.to_lowercase().contains(&query))
        .collect();

    let mut page = div().flex().flex_col().gap(px(if compact { 14. } else { 18. }));
    if compact {
        // A phone: the counts as one quiet line, not four pills over the list.
        page = page.child(
            div()
                .px(px(4.))
                .text_size(px(13.))
                .text_color(t.colors.foreground.opacity(0.6))
                .child(format!(
                    "{} users · {} administrators · {} disabled · {} without password",
                    data.users.len(),
                    admins,
                    disabled,
                    open
                )),
        );
    } else {
        page = page.child(
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
        );
    }
    // A field to find a user, once there are enough to look for one.
    if data.users.len() > 6 {
        page = page.child(find_field(&app.admin_users_filter, "Find a user", compact));
    }

    let mut rows = div().relative().flex().flex_col();
    for (index, user) in shown.iter().enumerate() {
        if index > 0 {
            rows = rows.child(hairline());
        }
        rows = rows.child(user_row(user, user.id == session.user_id, client, compact, &t, cx));
    }
    if shown.is_empty() {
        rows = rows.child(nothing_found("user", &query, cx));
    }
    if compact {
        // On a phone the new user is the last row of the list, where iOS
        // keeps "Add" in a list of accounts.
        if !shown.is_empty() {
            rows = rows.child(hairline());
        }
        rows = rows.child(
            div()
                .id("users.new")
                .role(gpui_kit::Role::Button)
                .aria_label("New user")
                .h(px(60.))
                .px(px(16.))
                .flex()
                .items_center()
                .gap(px(14.))
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xf5f5f714)))
                .child(icon_disc(LucideIcon::UserPlus, None, 40.))
                .child(
                    div()
                        .text_size(px(17.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(t.colors.foreground)
                        .child("New user\u{2026}"),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| ask_new(this, cx))),
        );
    }
    page.child(super::surface(div()).overflow_hidden().child(rows))
}

/// A row of the list: the picture, the name, a line with what matters
/// (signed in here, administrator, disabled, hidden, when last active)
/// and a chevron. A tap opens the user's page.
fn user_row(
    user: &User,
    me: bool,
    client: &Client,
    compact: bool,
    t: &UiTheme,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let mut detail: Vec<String> = Vec::new();
    if me {
        detail.push("You".to_string());
    }
    if user.is_admin() {
        detail.push("Administrator".to_string());
    }
    if user.is_disabled() {
        detail.push("Disabled".to_string());
    }
    if user.is_hidden() {
        detail.push("Hidden".to_string());
    }
    detail.push(match user.last_activity_date.as_deref().filter(|d| is_real_date(d)) {
        Some(date) => format!("Active {}", ago(date)),
        None => "Never active".to_string(),
    });
    let id = user.id.clone();
    div()
        .id(SharedString::from(format!("users.row.{}", user.id)))
        .role(gpui_kit::Role::Button)
        .aria_label(user.name.clone())
        .h(px(if compact { 60. } else { 54. }))
        .px(px(16.))
        .flex()
        .items_center()
        .gap(px(14.))
        .cursor_pointer()
        .hover(|s| s.bg(rgba(0xf5f5f714)))
        .when(user.is_disabled(), |el| el.opacity(0.62))
        .child(avatar(&user.name, user.image_url(client), if compact { 40. } else { 36. }))
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
                        .text_size(px(if compact { 17. } else { 15. }))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(t.colors.foreground)
                        .child(user.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(13.))
                        .text_color(t.colors.foreground.opacity(0.6))
                        .child(detail.join(" \u{b7} ")),
                ),
        )
        .child(icon(LucideIcon::ChevronRight, 16., t.colors.foreground.opacity(0.35)))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.admin_open_user(id.clone(), cx)))
}

/// The page of one user: the picture and the name, the facts, and the
/// actions as rows, the dangerous one last.
fn render_user(app: &Bloom, user: &Arc<User>, session: &crate::app::Session, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let compact = app.compact_w();
    let me = user.id == session.user_id;
    let client = &session.client;

    let mut badges = div().flex().flex_wrap().gap(px(6.));
    if me {
        badges = badges.child(badge("You", None));
    }
    if user.is_admin() {
        badges = badges.child(badge("Admin", None));
    }
    if user.is_disabled() {
        badges = badges.child(badge("Disabled", Some(State::Bad)));
    }
    if user.is_hidden() {
        badges = badges.child(badge("Hidden", None));
    }

    let seen = |date: &Option<String>| match date.as_deref().filter(|d| is_real_date(d)) {
        Some(date) => ago(date),
        None => "Never".to_string(),
    };
    let fact = |label: &'static str, value: String, warn: bool| {
        div()
            .h(px(if compact { 50. } else { 44. }))
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(12.))
            .text_size(px(if compact { 17. } else { 15. }))
            .child(
                div()
                    .w(px(120.))
                    .flex_shrink_0()
                    .text_color(t.colors.foreground.opacity(0.6))
                    .child(label),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if warn { rgba(0xf0b34aff) } else { t.colors.foreground })
                    .child(value),
            )
    };

    let facts = super::surface(div())
        .overflow_hidden()
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .child(fact("Last active", seen(&user.last_activity_date), false))
                .child(hairline())
                .child(fact("Last login", seen(&user.last_login_date), false))
                .child(hairline())
                .child(if user.has_password {
                    fact("Password", "Set".to_string(), false)
                } else {
                    fact("Password", "No password".to_string(), true)
                }),
        );

    let mut actions = div().relative().flex().flex_col().child({
        let target = user.clone();
        action_row("users.access", LucideIcon::ShieldCheck, "Access", "Libraries and rights", false, true, compact, &t)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.access_open(&target, cx)))
    });
    if me {
        actions = actions.child(hairline()).child(
            div()
                .px(px(16.))
                .py(px(14.))
                .text_size(px(13.))
                .text_color(t.colors.foreground.opacity(0.6))
                .child("This is the account signed in here. It cannot be disabled or deleted from this device."),
        );
    } else {
        let (label, glyph) = if user.is_disabled() {
            ("Enable", LucideIcon::UserCheck)
        } else {
            ("Disable", LucideIcon::Ban)
        };
        let toggle = user.clone();
        let password = user.clone();
        let delete = user.clone();
        actions = actions
            .child(hairline())
            .child(
                action_row("users.password", LucideIcon::KeyRound, "Change password", "", false, false, compact, &t)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| ask_password(this, &password, cx))),
            )
            .child(hairline())
            .child(
                action_row("users.toggle", glyph, label, "", false, false, compact, &t).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| ask_set_disabled(this, &toggle, !toggle.is_disabled(), cx),
                )),
            )
            .child(hairline())
            .child(
                action_row("users.delete", LucideIcon::Trash, "Delete user", "", true, false, compact, &t)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| ask_delete(this, &delete, cx))),
            );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .max_w(px(640.))
        // The Mac has no back arrow in its bar: a link back to the list.
        .when(!compact, |el| {
            el.child(
                div().flex().child(
                    action("users.back", LucideIcon::ArrowLeft, "Users", false, cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.admin_close_user(cx))),
                ),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(16.))
                .px(px(4.))
                .child(avatar(&user.name, user.image_url(client), if compact { 64. } else { 72. }))
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(22.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child(user.name.clone()),
                        )
                        .child(badges),
                ),
        )
        .child(facts)
        .child(super::surface(div()).overflow_hidden().child(actions))
}

/// A row that runs an action: an icon, a label, a detail line when there
/// is one, and a chevron when the row opens a page. Red for the dangerous one.
#[allow(clippy::too_many_arguments)]
fn action_row(
    id: &'static str,
    glyph: LucideIcon,
    label: &'static str,
    detail: &'static str,
    danger: bool,
    opens: bool,
    compact: bool,
    t: &UiTheme,
) -> Stateful<Div> {
    let color = if danger { rgba(0xff8a80ff) } else { t.colors.foreground };
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(label)
        .min_h(px(if compact { 50. } else { 46. }))
        .px(px(16.))
        .py(px(8.))
        .flex()
        .items_center()
        .gap(px(14.))
        .cursor_pointer()
        .hover(move |s| s.bg(if danger { rgba(0xc6282833) } else { rgba(0xf5f5f714) }))
        .child(icon(glyph, 18., color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .text_size(px(if compact { 17. } else { 15. }))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(color)
                        .child(label),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(13.))
                            .text_color(t.colors.foreground.opacity(0.6))
                            .child(detail),
                    )
                }),
        )
        .when(opens, |el| el.child(icon(LucideIcon::ChevronRight, 16., t.colors.foreground.opacity(0.35))))
}

/// The line between two rows of a list.
fn hairline() -> Div {
    div().h(px(1.)).mx(px(16.)).bg(rgba(0xf5f5f714))
}

fn ask_set_disabled(app: &mut Bloom, user: &User, disable: bool, cx: &mut Context<Bloom>) {
    let (id, name) = (user.id.clone(), user.name.clone());
    if !user.policy.is_object() {
        app.toast("The server sent no policy for this user", "", cx);
        return;
    }
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
            let id = id.clone();
            let done = if disable { "User disabled" } else { "User enabled" };
            this.admin_action(done, cx, move |client| {
                // The policy the server has now, with the one key changed:
                // the list of the users can be older than a change of the
                // access of this user, and its policy would put that back.
                let user: Value = client.get(&format!("/Users/{id}"), &[])?;
                let mut policy = user.get("Policy").cloned().unwrap_or(Value::Null);
                let Some(fields) = policy.as_object_mut() else {
                    anyhow::bail!("the server sent no policy for this user");
                };
                fields.insert("IsDisabled".to_string(), Value::Bool(disable));
                client
                    .post(&format!("/Users/{id}/Policy"), &policy)
                    .map(|_| ())
            });
        }),
    };
    app.ask_confirm(confirm, cx);
}

/// Opens the page of the user with this name. For the debug channel.
pub(crate) fn open_user_named(app: &mut Bloom, name: &str, cx: &mut Context<Bloom>) -> bool {
    let user = user_named(app, name);
    if let Some(user) = &user {
        app.admin_open_user(user.id.clone(), cx);
    }
    user.is_some()
}

/// Opens the password form for the user with this name. For the debug channel.
pub(crate) fn ask_password_named(app: &mut Bloom, name: &str, cx: &mut Context<Bloom>) -> bool {
    let user = user_named(app, name);
    if let Some(user) = &user {
        ask_password(app, user, cx);
    }
    user.is_some()
}

/// Asks to delete a user, by name. For the debug channel.
pub(crate) fn ask_delete_named(app: &mut Bloom, name: &str, cx: &mut Context<Bloom>) -> bool {
    let user = user_named(app, name);
    if let Some(user) = &user {
        ask_delete(app, user, cx);
    }
    user.is_some()
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

/// Width the page content has, beside the sidebar and inside the padding
/// (`Bloom::admin_content_w`; on a phone the window less the margins).
pub(super) fn content_width(app: &Bloom) -> f32 {
    app.admin_content_w()
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
        .bg(tone::DISC)
        .text_size(px(size * 0.42))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(tone::INK)
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

/// Round icon on the quiet disc of the theme, in the ink, or in the
/// colour of the state it stands for.
pub(super) fn icon_disc(glyph: LucideIcon, state: Option<State>, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(tone::DISC)
        .child(icon(glyph, size * 0.48, tone::state_ink(state)))
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
) -> gpui_kit::base::Button {
    let color = UiTheme::read(cx).colors.primary_foreground;
    // `button` puts its label alone; here an icon leads, a little smaller.
    crate::ui::button::frame(id, label, super::ButtonKind::Primary, cx)
        .h(px(36.))
        .text_size(px(14.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child(icon(glyph, 16., color))
        .child(label)
}
