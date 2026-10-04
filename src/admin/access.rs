// SPDX-License-Identifier: AGPL-3.0-or-later
//! What one user may do: the library access and the rights of the policy of
//! the user. The editor is a page inside the Users section. It keeps two
//! copies of the policy, as the server gave it and as the user edits it, and
//! shows the Save bar while the two differ (as the configuration pages do).
//! A save reads the policy from the server again, changes only the keys the
//! user edited, and sends the whole object back.

use std::rc::Rc;

use anyhow::{Result, bail};
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, div, px, rgb, rgba,
};
use serde_json::{Value, json};

use super::{
    ButtonKind, Confirm, Field as Form, Prompt, badge, button,
    config::value_button,
    users::{User, action, avatar},
};
use crate::{
    app::{Bloom, Page},
    settings::{Choice, checkbox, field, group, select},
    ui::{glass::glass, theme::UiTheme},
    views::cards::icon,
};

/// Why the user cannot change a right of the account that is signed in.
const OWN_ACCOUNT: &str = "You cannot change this for the account you are signed in with.";
/// Why an administrator cannot be disabled (the server refuses it).
const ADMIN_ACCOUNT: &str = "The server does not disable an administrator.";
/// Keys of the policy that hold the library access.
const ALL_FOLDERS: &str = "EnableAllFolders";
const FOLDERS: &str = "EnabledFolders";

#[derive(Clone, Copy)]
enum Kind {
    Flag,
    Choice(&'static [(&'static str, &'static str)]),
    /// A whole number. The page shows it as `value / scale` in `unit`.
    Number { unit: &'static str, scale: f64, min: i64 },
}

struct Row {
    key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: Kind,
}

const fn flag(key: &'static str, label: &'static str, help: &'static str) -> Row {
    Row { key, label, help, kind: Kind::Flag }
}

const SYNCPLAY: &[(&str, &str)] = &[
    ("CreateAndJoinGroups", "Create and join groups"),
    ("JoinGroups", "Join groups"),
    ("None", "No access"),
];

const ACCOUNT: &[Row] = &[
    flag("IsAdministrator", "Administrator", "Can change every setting of the server."),
    flag("IsDisabled", "Disabled", "The user cannot sign in."),
    flag("IsHidden", "Hidden", "The user does not show on the sign-in page."),
];

const REST: &[(&str, &[Row])] = &[
    (
        "Playback",
        &[
            flag("EnableMediaPlayback", "Play media", "The user can play videos and music."),
            flag(
                "EnableVideoPlaybackTranscoding",
                "Transcode video",
                "The server may convert video for this user.",
            ),
            flag(
                "EnableAudioPlaybackTranscoding",
                "Transcode audio",
                "The server may convert audio for this user.",
            ),
            flag(
                "EnablePlaybackRemuxing",
                "Remux",
                "The server may change the container of a file without a new encode.",
            ),
        ],
    ),
    (
        "Features",
        &[
            flag("EnableContentDownloading", "Download media", "The user can download files."),
            flag("EnableContentDeletion", "Delete media", "The user can delete items from the libraries."),
            flag("EnableCollectionManagement", "Manage collections", "The user can make and change collections."),
            flag("EnableSubtitleManagement", "Manage subtitles", "The user can search for, upload and delete subtitles."),
            flag(
                "EnableRemoteControlOfOtherUsers",
                "Control other users",
                "The user can control the sessions of other users.",
            ),
            flag(
                "EnableSharedDeviceControl",
                "Control shared devices",
                "The user can control a device that other users also use.",
            ),
            Row {
                key: "SyncPlayAccess",
                label: "SyncPlay",
                help: "What the user can do in a watch-together group.",
                kind: Kind::Choice(SYNCPLAY),
            },
        ],
    ),
    (
        "Limits",
        &[
            Row {
                key: "RemoteClientBitrateLimit",
                label: "Bitrate limit away from home",
                help: "The highest bitrate of a stream outside the local network. 0 means no limit.",
                kind: Kind::Number { unit: "Mbps", scale: 1_000_000., min: 0 },
            },
            Row {
                key: "MaxActiveSessions",
                label: "Active sessions",
                help: "How many sessions the user can have at one time. 0 means no limit.",
                kind: Kind::Number { unit: "", scale: 1., min: 0 },
            },
            Row {
                key: "LoginAttemptsBeforeLockout",
                label: "Failed sign-ins before lockout",
                help: "-1 never locks the account. 0 locks it after 3 failed sign-ins.",
                kind: Kind::Number { unit: "", scale: 1., min: -1 },
            },
        ],
    ),
];

fn rows() -> impl Iterator<Item = &'static Row> {
    ACCOUNT.iter().chain(REST.iter().flat_map(|(_, rows)| rows.iter()))
}

fn find_row(key: &str) -> Option<&'static Row> {
    rows().find(|row| row.key.eq_ignore_ascii_case(key))
}

/// The editor of one user.
pub struct Editor {
    pub user_id: String,
    pub name: String,
    image: Option<String>,
    /// The signed-in account.
    pub me: bool,
    pub loading: bool,
    pub saving: bool,
    pub error: Option<String>,
    /// Libraries of the server: id and name.
    pub folders: Vec<(String, String)>,
    /// The policy as the server gave it.
    pub original: Value,
    /// The policy with the edits of the user.
    pub edited: Value,
}

/// A folder id without dashes and in lower case, so two spellings compare.
fn plain_id(id: &str) -> String {
    id.chars().filter(|c| *c != '-').collect::<String>().to_lowercase()
}

fn ids(value: &Value) -> Vec<String> {
    let mut list: Vec<String> = value
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).map(plain_id).collect())
        .unwrap_or_default();
    list.sort();
    list
}

/// Whether the value of a key is the same in two policies. The order of the
/// library ids does not count.
fn same(key: &str, a: &Value, b: &Value) -> bool {
    let (a, b) = (a.get(key).unwrap_or(&Value::Null), b.get(key).unwrap_or(&Value::Null));
    match key {
        FOLDERS => ids(a) == ids(b),
        _ => a == b,
    }
}

/// The keys whose value differs in two policies, in key order.
pub fn changed_keys(original: &Value, edited: &Value) -> Vec<String> {
    let mut keys: Vec<String> = original
        .as_object()
        .into_iter()
        .chain(edited.as_object())
        .flat_map(|object| object.keys().cloned())
        .collect();
    keys.sort();
    keys.dedup();
    keys.retain(|key| !same(key, original, edited));
    keys
}

/// The policy to send: the policy of the server now, with the keys the user
/// edited (and only those) taken from `edited`. A key in `locked` never
/// changes. A key the app does not know stays as the server has it.
pub fn merge_policy(fresh: &Value, original: &Value, edited: &Value, locked: &[&str]) -> Value {
    let mut policy = fresh.clone();
    if let Some(object) = policy.as_object_mut() {
        for key in changed_keys(original, edited) {
            if locked.contains(&key.as_str()) {
                continue;
            }
            match edited.get(&key) {
                Some(value) => object.insert(key, value.clone()),
                None => object.remove(&key),
            };
        }
    }
    policy
}

impl Editor {
    pub fn changes(&self) -> Vec<String> {
        changed_keys(&self.original, &self.edited)
    }

    pub fn dirty(&self) -> bool {
        !self.changes().is_empty()
    }

    /// Edits shown to the user: the two library keys count as one.
    fn change_count(&self) -> usize {
        let keys = self.changes();
        let library = keys.iter().filter(|k| *k == ALL_FOLDERS || *k == FOLDERS).count();
        keys.len() - library.saturating_sub(1)
    }

    fn flag(&self, key: &str) -> bool {
        self.edited.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    /// Keys the user cannot change here, with the reason.
    fn locked(&self, key: &str) -> Option<&'static str> {
        match key {
            "IsAdministrator" if self.me => Some(OWN_ACCOUNT),
            "IsDisabled" if self.me => Some(OWN_ACCOUNT),
            "IsDisabled" if self.flag("IsAdministrator") => Some(ADMIN_ACCOUNT),
            _ => None,
        }
    }

    /// The keys a save leaves as the server has them.
    fn locked_keys(&self) -> Vec<&'static str> {
        if self.me { vec!["IsAdministrator", "IsDisabled"] } else { Vec::new() }
    }

    fn folder_name(&self, id: &str) -> String {
        self.folders
            .iter()
            .find(|(folder, _)| plain_id(folder) == plain_id(id))
            .map_or_else(|| id.to_string(), |(_, name)| name.clone())
    }
}

/// A number as the page shows it, with the unit.
fn number_text(row: &Row, raw: i64) -> String {
    let Kind::Number { unit, scale, .. } = row.kind else {
        return raw.to_string();
    };
    let value = raw as f64 / scale;
    let typed = if value.fract() == 0. { format!("{value:.0}") } else { value.to_string() };
    match (row.key, raw) {
        ("RemoteClientBitrateLimit" | "MaxActiveSessions", 0) => "No limit".to_string(),
        ("LoginAttemptsBeforeLockout", -1) => "Never".to_string(),
        _ if unit.is_empty() => typed,
        _ => format!("{typed} {unit}"),
    }
}

/// The number a text stands for, in the units of the policy.
fn parse_number(row: &Row, text: &str) -> Result<i64> {
    let Kind::Number { scale, min, .. } = row.kind else {
        bail!("{} is not a number", row.key);
    };
    let value: f64 = text.trim().parse().map_err(|_| anyhow::anyhow!("{text:?} is not a number"))?;
    let raw = (value * scale).round() as i64;
    if raw < min {
        bail!("the lowest value is {}", min as f64 / scale);
    }
    Ok(raw)
}

fn parse_flag(text: &str) -> Result<bool> {
    match text.trim().to_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        other => bail!("{other:?} is not true or false"),
    }
}

impl Bloom {
    fn access(&self) -> Option<&Editor> {
        match &self.page {
            Page::Admin(admin) => admin.access.as_ref(),
            _ => None,
        }
    }

    fn access_mut(&mut self) -> Option<&mut Editor> {
        match &mut self.page {
            Page::Admin(admin) => admin.access.as_mut(),
            _ => None,
        }
    }

    /// Opens the editor of a user and reads the policy from the server.
    pub fn access_open(&mut self, user: &User, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let editor = Editor {
            user_id: user.id.clone(),
            name: user.name.clone(),
            image: user.image_url(&session.client),
            me: user.id == session.user_id,
            loading: true,
            saving: false,
            error: None,
            folders: Vec::new(),
            original: Value::Null,
            edited: Value::Null,
        };
        let id = user.id.clone();
        if let Page::Admin(admin) = &mut self.page {
            admin.access = Some(editor);
        }
        cx.notify();
        self.fetch(
            cx,
            {
                let id = id.clone();
                move |client| -> Result<(Value, Vec<(String, String)>)> {
                    let user: Value = client.get(&format!("/Users/{id}"), &[])?;
                    let policy = user.get("Policy").cloned().unwrap_or(Value::Null);
                    if !policy.is_object() {
                        bail!("the server sent no policy for this user");
                    }
                    let folders: Value = client.get("/Library/MediaFolders", &[])?;
                    let folders = folders
                        .get("Items")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| {
                                    Some((
                                        item.get("Id")?.as_str()?.to_string(),
                                        item.get("Name")?.as_str()?.to_string(),
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Ok((policy, folders))
                }
            },
            move |this, result, cx| {
                if let Some(editor) = this.access_mut()
                    && editor.user_id == id
                {
                    editor.loading = false;
                    match result {
                        Ok((policy, folders)) => {
                            editor.original = policy.clone();
                            editor.edited = policy;
                            editor.folders = folders;
                        }
                        Err(err) => editor.error = Some(format!("{err:#}")),
                    }
                }
                cx.notify();
            },
        );
    }

    /// Opens the editor of the user with this name, from the Users page. For
    /// the debug channel.
    pub fn access_open_named(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let Page::Admin(admin) = &self.page else {
            return false;
        };
        let user = admin
            .users
            .as_ref()
            .and_then(|data| data.users.iter().find(|user| user.name == name).cloned());
        match user {
            Some(user) => {
                self.access_open(&user, cx);
                true
            }
            None => false,
        }
    }

    /// Leaves the editor. Edits that are not saved need a yes first.
    pub fn access_close(&mut self, cx: &mut Context<Self>) {
        if self.access().is_some_and(Editor::dirty) {
            self.ask_confirm(
                Confirm {
                    title: "Discard the changes?".to_string(),
                    message: "The changes to the access of this user are not saved.".to_string(),
                    action: "Discard".to_string(),
                    danger: true,
                    run: Rc::new(|this, cx| this.access_leave(cx)),
                },
                cx,
            );
        } else {
            self.access_leave(cx);
        }
    }

    fn access_leave(&mut self, cx: &mut Context<Self>) {
        if let Page::Admin(admin) = &mut self.page {
            admin.access = None;
        }
        cx.notify();
    }

    pub fn access_discard(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.access_mut() {
            editor.edited = editor.original.clone();
        }
        cx.notify();
    }

    fn access_put(&mut self, key: &str, value: Value, cx: &mut Context<Self>) {
        if let Some(editor) = self.access_mut()
            && let Some(object) = editor.edited.as_object_mut()
        {
            object.insert(key.to_string(), value);
        }
        cx.notify();
    }

    /// Sets a yes-or-no key. A change of the administrator right asks first.
    fn access_set_flag(&mut self, key: &str, on: bool, cx: &mut Context<Self>) -> Result<()> {
        let Some(editor) = self.access() else {
            bail!("the access editor is not open");
        };
        if editor.loading {
            bail!("the policy is still loading");
        }
        if let Some(why) = editor.locked(key) {
            bail!("{why}");
        }
        if editor.flag(key) == on {
            return Ok(());
        }
        if key == "IsAdministrator" {
            let name = editor.name.clone();
            let confirm = Confirm {
                title: if on {
                    format!("Make {name} an administrator?")
                } else {
                    format!("Remove the administrator right of {name}?")
                },
                message: if on {
                    format!(
                        "{name} can change every setting of the server, and the users and keys \
                         of other people. The change applies when you save."
                    )
                } else {
                    format!("{name} loses the access to the dashboard. The change applies when you save.")
                },
                action: if on { "Make administrator" } else { "Remove right" }.to_string(),
                danger: true,
                run: Rc::new(move |this, cx| this.access_put("IsAdministrator", json!(on), cx)),
            };
            self.ask_confirm(confirm, cx);
            return Ok(());
        }
        if key == ALL_FOLDERS && on {
            // As the web dashboard does: the list is empty while the user
            // sees all libraries.
            self.access_put(FOLDERS, json!([]), cx);
        }
        self.access_put(key, json!(on), cx);
        Ok(())
    }

    /// A click on a checkbox: the toast says why a change is not possible.
    fn access_toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        let on = self.access().is_some_and(|editor| editor.flag(key));
        if let Err(err) = self.access_set_flag(key, !on, cx) {
            self.toast("This cannot change", format!("{err:#}"), cx);
        }
    }

    fn access_toggle_folder(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(editor) = self.access() else {
            return;
        };
        let mut chosen: Vec<String> = editor
            .edited
            .get(FOLDERS)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        match chosen.iter().position(|item| plain_id(item) == plain_id(id)) {
            Some(at) => {
                chosen.remove(at);
            }
            None => chosen.push(id.to_string()),
        }
        self.access_put(FOLDERS, json!(chosen), cx);
    }

    /// Sets a key from text, as the page does with its controls. The number
    /// is in the unit the page shows (Mbps for the bitrate). For the form
    /// and the debug channel.
    pub fn access_set(&mut self, key: &str, text: &str, cx: &mut Context<Self>) -> Result<()> {
        let Some(editor) = self.access() else {
            bail!("the access editor is not open");
        };
        if editor.loading {
            bail!("the policy is still loading");
        }
        if key.eq_ignore_ascii_case(ALL_FOLDERS) {
            return self.access_set_flag(ALL_FOLDERS, parse_flag(text)?, cx);
        }
        if key.eq_ignore_ascii_case(FOLDERS) {
            let mut chosen = Vec::new();
            for word in text.split(',').map(str::trim).filter(|w| !w.is_empty()) {
                let found = editor
                    .folders
                    .iter()
                    .find(|(id, name)| plain_id(id) == plain_id(word) || name.eq_ignore_ascii_case(word));
                match found {
                    Some((id, _)) => chosen.push(id.clone()),
                    None => bail!("no library {word:?}"),
                }
            }
            self.access_put(FOLDERS, json!(chosen), cx);
            return Ok(());
        }
        let Some(row) = find_row(key) else {
            bail!("no such key {key:?}");
        };
        match row.kind {
            Kind::Flag => self.access_set_flag(row.key, parse_flag(text)?, cx),
            Kind::Choice(options) => {
                let Some((value, _)) = options.iter().find(|(value, label)| {
                    value.eq_ignore_ascii_case(text.trim()) || label.eq_ignore_ascii_case(text.trim())
                }) else {
                    bail!("{} takes one of: {}", row.key, options.iter().map(|(v, _)| *v).collect::<Vec<_>>().join(", "));
                };
                self.access_put(row.key, json!(value), cx);
                Ok(())
            }
            Kind::Number { .. } => {
                let raw = parse_number(row, text)?;
                self.access_put(row.key, json!(raw), cx);
                Ok(())
            }
        }
    }

    /// The form of a number key.
    fn access_ask_number(&mut self, row: &'static Row, current: String, cx: &mut Context<Self>) {
        let prompt = Prompt {
            title: row.label.to_string(),
            message: row.help.to_string(),
            action: "Set".to_string(),
            fields: vec![Form {
                label: match row.kind {
                    Kind::Number { unit, .. } if !unit.is_empty() => format!("Value in {unit}"),
                    _ => "Value".to_string(),
                },
                placeholder: "A number".to_string(),
                value: current,
                masked: false,
                required: true,
            }],
            run: Rc::new(move |this, values, cx| {
                let text = values.into_iter().next().unwrap_or_default();
                if let Err(err) = this.access_set(row.key, &text, cx) {
                    this.toast("This value does not fit", format!("{err:#}"), cx);
                }
            }),
        };
        self.ask_prompt(prompt, cx);
    }

    /// Sends the edits. The policy is read again first, so a change that
    /// somebody else made to a key the user did not edit stays.
    pub fn access_save(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.access_mut() else {
            return;
        };
        if editor.saving || editor.loading || !editor.dirty() {
            return;
        }
        editor.saving = true;
        let (id, original, edited) = (editor.user_id.clone(), editor.original.clone(), editor.edited.clone());
        let locked = editor.locked_keys();
        cx.notify();
        let work_id = id.clone();
        self.fetch(
            cx,
            move |client| -> Result<Value> {
                let path = format!("/Users/{work_id}");
                let user: Value = client.get(&path, &[])?;
                let fresh = user.get("Policy").cloned().unwrap_or(Value::Null);
                // The server refuses a policy without these two.
                for key in ["AuthenticationProviderId", "PasswordResetProviderId"] {
                    if fresh.get(key).and_then(Value::as_str).is_none_or(str::is_empty) {
                        bail!("the policy of the server has no {key}");
                    }
                }
                let policy = merge_policy(&fresh, &original, &edited, &locked);
                client.post(&format!("{path}/Policy"), &policy)?;
                let user: Value = client.get(&path, &[])?;
                Ok(user.get("Policy").cloned().unwrap_or(Value::Null))
            },
            move |this, result, cx| {
                match &result {
                    Ok(_) => this.toast("Access saved", "", cx),
                    Err(err) => this.toast("The server refused the change", format!("{err:#}"), cx),
                }
                if let Some(editor) = this.access_mut()
                    && editor.user_id == id
                {
                    editor.saving = false;
                    if let Ok(policy) = result {
                        editor.original = policy.clone();
                        editor.edited = policy;
                    }
                }
                // The Users page behind shows the new badges.
                if matches!(this.page, Page::Admin(_)) {
                    this.load_page(cx);
                }
                cx.notify();
            },
        );
    }

    /// The editor in a line, for the debug channel.
    pub fn access_describe(&self) -> String {
        let Some(editor) = self.access() else {
            return "access editor closed".to_string();
        };
        let keys: Vec<String> = editor
            .changes()
            .into_iter()
            .map(|key| {
                let value = editor.edited.get(&key).cloned().unwrap_or(Value::Null);
                let shown = match (key.as_str(), &value) {
                    (FOLDERS, Value::Array(items)) => format!(
                        "[{}]",
                        items.iter().filter_map(Value::as_str).map(|id| editor.folder_name(id)).collect::<Vec<_>>().join(",")
                    ),
                    _ => value.to_string(),
                };
                format!("{key}={shown}")
            })
            .collect();
        format!(
            "user={} me={} loading={} saving={} error={:?} unsaved={} changes={} edited=[{}]",
            editor.name,
            editor.me,
            editor.loading,
            editor.saving,
            editor.error,
            if editor.dirty() { "yes" } else { "no" },
            editor.change_count(),
            keys.join(", ")
        )
    }

    /// The debug commands `access ...` and `triggers ...`; one entry in
    /// `src/debug.rs`. Opening and saving run on a background thread: read
    /// the result with `access state` or `triggers list`.
    pub fn debug_access(
        &mut self,
        command: &str,
        rest: &str,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> String {
        if command == "triggers" {
            return self.debug_triggers(rest, cx);
        }
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        let arg = arg.trim();
        match verb {
            "open" => {
                if !self.access_open_named(arg, cx) {
                    return "error: no such user on the Users page (open it with: admin users)".into();
                }
            }
            "set" => {
                let (key, value) = arg.split_once(' ').unwrap_or((arg, ""));
                // A change of IsAdministrator opens the confirm dialog;
                // `write confirm` says yes.
                if let Err(err) = self.access_set(key, value, cx) {
                    return format!("error: {err:#}");
                }
            }
            "save" => match self.access() {
                Some(editor) if !editor.name.starts_with("jellyui-test") => {
                    return "error: the debug save works only for users named jellyui-test...".into();
                }
                Some(_) => self.access_save(cx),
                None => return "error: the access editor is not open".into(),
            },
            "discard" => self.access_discard(cx),
            "state" | "" => {}
            _ => return "error: access open <user> | set <Key> <value> | state | save | discard".into(),
        }
        self.access_describe()
    }

    /// The bar with Save and Discard, while the editor has changes.
    pub fn render_access_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let editor = self.access()?;
        if !editor.dirty() || !matches!(&self.page, Page::Admin(admin) if admin.section == super::Section::Users) {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let text = match (editor.saving, editor.change_count()) {
            (true, _) => "Saving…".to_string(),
            (_, 1) => "1 change is not saved.".to_string(),
            (_, n) => format!("{n} changes are not saved."),
        };
        Some(
            div()
                .absolute()
                .left(px(super::SIDEBAR_W + 28.))
                .right(px(28.))
                .bottom(px(20.))
                .h(px(64.))
                .rounded(px(24.))
                .border_1()
                .border_color(rgba(0xf5f5f733))
                .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
                .px(px(20.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_1().text_size(px(15.)).text_color(t.colors.foreground).child(text))
                .child(
                    button("access.discard", "Discard", ButtonKind::Plain, cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.access_discard(cx))),
                )
                .child(
                    button("access.save", "Save", ButtonKind::Primary, cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.access_save(cx))),
                ),
        )
    }
}

// ----- the page -----------------------------------------------------------------

/// A checkbox that shows a lock: dim, and a click only tells why.
fn lockable(control: Stateful<Div>, locked: bool) -> Stateful<Div> {
    if locked { control.opacity(0.4) } else { control }
}

pub fn render(editor: &Editor, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let mut page = div().max_w(px(920.)).flex().flex_col().gap(px(18.));

    let mut badges = div().flex().gap(px(6.));
    if editor.me {
        badges = badges.child(badge("You", rgba(0xffffff2e)));
    }
    page = page.child(
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(
                action("access.back", LucideIcon::ArrowLeft, "Users", false, cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.access_close(cx))),
            )
            .child(avatar(&editor.name, editor.image.clone(), 40.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(20.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(t.colors.foreground)
                    .child(format!("Access of {}", editor.name)),
            )
            .child(badges),
    );

    if editor.loading || editor.error.is_some() {
        return page.child(
            div()
                .py(px(30.))
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child(match &editor.error {
                    Some(error) => format!("Could not read the policy of this user: {error}"),
                    None => "Loading…".to_string(),
                }),
        );
    }

    // Account, then libraries, then the rest.
    page = page.child(rows_card("Account", ACCOUNT, editor, cx));
    page = page.child(libraries_card(editor, cx));
    for (title, list) in REST {
        page = page.child(rows_card(title, list, editor, cx));
    }
    page.child(div().h(px(if editor.dirty() { 72. } else { 0. })))
}

fn rows_card(title: &'static str, list: &'static [Row], editor: &Editor, cx: &mut Context<Bloom>) -> Div {
    let mut card = group(title, cx);
    for row in list {
        let id = SharedString::from(format!("access.{}", row.key));
        let value = editor.edited.get(row.key).cloned().unwrap_or(Value::Null);
        let locked = editor.locked(row.key);
        let key = row.key;
        let control: Stateful<Div> = match row.kind {
            Kind::Flag => lockable(checkbox(id, value.as_bool().unwrap_or(false), cx), locked.is_some())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.access_toggle(key, cx))),
            Kind::Choice(options) => {
                let current = value.as_str().unwrap_or("").to_string();
                let shown = options
                    .iter()
                    .find(|(option, _)| *option == current)
                    .map_or_else(|| current.clone(), |(_, label)| label.to_string());
                let longest = options.iter().map(|(_, label)| *label).max_by_key(|l| l.len()).unwrap_or_default();
                select(id, shown, longest, cx).on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        let choices = options
                            .iter()
                            .map(|(option, label)| {
                                let option = *option;
                                Choice::new(*label, option == current, move |this, cx| {
                                    this.access_put(key, json!(option), cx)
                                })
                            })
                            .collect();
                        this.open_choices(choices, event.position(), window, cx);
                    },
                ))
            }
            Kind::Number { scale, .. } => {
                let raw = value.as_i64().unwrap_or(0);
                let typed = {
                    let n = raw as f64 / scale;
                    if n.fract() == 0. { format!("{n:.0}") } else { n.to_string() }
                };
                value_button(id, number_text(row, raw), cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| this.access_ask_number(row, typed.clone(), cx),
                ))
            }
        };
        card = card.child(field(row.label, locked.unwrap_or(row.help), control, cx));
    }
    card
}

fn libraries_card(editor: &Editor, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let all = editor.flag(ALL_FOLDERS);
    let mut card = group("Libraries", cx).child(field(
        "All libraries",
        "The user sees every library, including libraries that you add later.",
        checkbox("access.all", all, cx)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.access_toggle(ALL_FOLDERS, cx))),
        cx,
    ));
    if all {
        return card;
    }
    let chosen = ids(editor.edited.get(FOLDERS).unwrap_or(&Value::Null));
    for (n, (id, name)) in editor.folders.iter().enumerate() {
        let on = chosen.contains(&plain_id(id));
        let id = id.clone();
        card = card.child(field(
            name.clone(),
            "",
            checkbox(format!("access.folder.{n}"), on, cx)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.access_toggle_folder(&id, cx))),
            cx,
        ));
    }
    if editor.folders.is_empty() {
        card = card.child(div().py(px(10.)).text_size(px(13.)).text_color(t.colors.muted_foreground).child("The server has no libraries."));
    } else if chosen.is_empty() {
        // A state that needs a look: the user would see nothing.
        card = card.child(
            div()
                .py(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(13.))
                .text_color(t.colors.foreground.opacity(0.8))
                .child(icon(LucideIcon::TriangleAlert, 14., rgb(0xe3b341)))
                .child("No library is chosen. The user sees no library."),
        );
    }
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Value {
        json!({
            "IsAdministrator": false,
            "IsDisabled": false,
            "EnableAllFolders": true,
            "EnabledFolders": [],
            "EnableContentDownloading": true,
            "RemoteClientBitrateLimit": 0,
            "AuthenticationProviderId": "Default",
            "PasswordResetProviderId": "Reset",
            "SomethingNew": { "Kept": [1, 2] },
        })
    }

    #[test]
    fn a_merge_changes_only_the_edited_keys() {
        let original = policy();
        let mut edited = original.clone();
        edited["EnableContentDownloading"] = json!(false);
        edited["RemoteClientBitrateLimit"] = json!(8_000_000);
        // Somebody else changed two other keys and added one after the read.
        let mut fresh = original.clone();
        fresh["IsHidden"] = json!(true);
        fresh["EnableAllFolders"] = json!(false);
        fresh["SomethingNew"] = json!({ "Kept": [3] });
        let sent = merge_policy(&fresh, &original, &edited, &[]);
        assert_eq!(sent["EnableContentDownloading"], false);
        assert_eq!(sent["RemoteClientBitrateLimit"], 8_000_000);
        assert_eq!(sent["IsHidden"], true);
        assert_eq!(sent["EnableAllFolders"], false);
        assert_eq!(sent["SomethingNew"], json!({ "Kept": [3] }));
        assert_eq!(sent["AuthenticationProviderId"], "Default");
        // No edit: the policy of the server goes back unchanged.
        assert_eq!(merge_policy(&fresh, &original, &original, &[]), fresh);
    }

    #[test]
    fn a_locked_key_never_changes() {
        let original = policy();
        let mut edited = original.clone();
        edited["IsAdministrator"] = json!(true);
        edited["IsDisabled"] = json!(true);
        edited["IsHidden"] = json!(true);
        let sent = merge_policy(&original, &original, &edited, &["IsAdministrator", "IsDisabled"]);
        assert_eq!(sent["IsAdministrator"], false);
        assert_eq!(sent["IsDisabled"], false);
        assert_eq!(sent["IsHidden"], true);
    }

    #[test]
    fn library_ids_compare_without_order_and_dashes() {
        let a = json!({ "EnabledFolders": ["AAAA1111", "bbbb2222"] });
        let b = json!({ "EnabledFolders": ["bbbb2222", "aaaa-1111"] });
        assert!(changed_keys(&a, &b).is_empty());
        let c = json!({ "EnabledFolders": ["bbbb2222"] });
        assert_eq!(changed_keys(&a, &c), vec!["EnabledFolders"]);
    }

    #[test]
    fn numbers_go_between_text_and_policy() {
        let bitrate = find_row("remoteclientbitratelimit").unwrap();
        assert_eq!(parse_number(bitrate, "8").unwrap(), 8_000_000);
        assert_eq!(parse_number(bitrate, "1.5").unwrap(), 1_500_000);
        assert!(parse_number(bitrate, "-1").is_err());
        assert!(parse_number(bitrate, "fast").is_err());
        assert_eq!(number_text(bitrate, 8_000_000), "8 Mbps");
        assert_eq!(number_text(bitrate, 0), "No limit");
        let lockout = find_row("LoginAttemptsBeforeLockout").unwrap();
        assert_eq!(parse_number(lockout, "-1").unwrap(), -1);
        assert_eq!(number_text(lockout, -1), "Never");
        assert_eq!(number_text(lockout, 5), "5");
    }

    #[test]
    fn every_key_of_the_page_is_unique() {
        let mut keys: Vec<&str> = rows().map(|row| row.key).collect();
        let count = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), count);
    }
}
