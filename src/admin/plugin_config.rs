// SPDX-License-Identifier: AGPL-3.0-or-later
//! A generic editor for the stored settings of a plugin.
//!
//! The option page of a Jellyfin plugin is an HTML page for the web client,
//! which this app cannot show. The server also gives the settings of a
//! plugin as one JSON object (`GET /Plugins/{id}/Configuration`) and takes
//! the whole object back (`POST`). The editor builds its fields from the
//! values in that object: a bool is a toggle, a number a number field (an
//! integer stays an integer, a float stays a float), a string a text field,
//! a list of strings a list field, and a nested object a titled group. A
//! list of objects and a null value show as they are, read-only.
//!
//! A save posts the edited copy of the object. Every key the editor does not
//! change goes back as it came, in the same order and with the same type.
//!
//! The value of a setting whose name says "key", "token", "password" or
//! "secret" is hidden until the user chooses Reveal. It is never written to
//! a log, to the debug channel or to a toast.

use std::{collections::HashSet, rc::Rc};

use anyhow::Result;
use gpui_kit::{
    AppContext as _, ClickEvent, Context, Div, Entity, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, px,
};
use serde_json::{Number, Value};

use super::{
    ButtonKind, Confirm, button,
    config::{bar_shell, value_button},
    dialogs::{Field as PromptField, Prompt},
    text_editor::TextEdit,
};
use crate::{
    app::{Bloom, Page},
    settings::{checkbox, field, group},
    ui::{
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
    },
};

/// With more fields than this the page has a filter box.
const FILTER_MIN: usize = 15;
/// A group with this many fields or more builds only the fields in view.
const LAZY_MIN: usize = 4;
/// A text longer than this, or with a new line, is edited in the large dialog.
const LONG_TEXT: usize = 120;
/// What a hidden value shows. It has the same length for every value.
const HIDDEN: &str = "••••••••";

// ----- fields from values -------------------------------------------------------

/// How a field is shown and edited.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Bool,
    Int,
    Float,
    Text,
    /// A text with many lines, or a long one.
    LongText,
    /// A list of texts or of numbers; in the form, separated by commas.
    List { numbers: bool },
    /// A value the editor does not edit; the text tells why.
    Fixed(String),
}

#[derive(Clone, Debug)]
pub struct PField {
    /// The keys from the root object down to the value.
    pub path: Vec<String>,
    pub label: String,
    pub kind: Kind,
    /// The value is hidden until the user reveals it.
    pub secret: bool,
    /// The title of the card the field is in.
    pub group: String,
}

impl PField {
    /// The path as one text, such as "Server.ApiKey".
    pub fn key(&self) -> String {
        self.path.join(".")
    }
}

/// The words of a setting name: "EnableAutoSkip" is "Enable", "Auto",
/// "Skip"; "TMDB_API_KEY" is "TMDB", "API", "KEY".
fn words(key: &str) -> Vec<String> {
    let chars: Vec<char> = key.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '_' | '-' | ' ' | '.') {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            continue;
        }
        if c.is_uppercase()
            && let Some(&before) = i.checked_sub(1).and_then(|n| chars.get(n))
        {
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let starts =
                before.is_lowercase() || before.is_ascii_digit() || (before.is_uppercase() && next_lower);
            if starts && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        }
        word.push(c);
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The label of a setting: the name in words, "EnableAutoSkip" as "Enable
/// auto skip". A short word in capitals, such as "HTTP", stays. A name all in
/// capitals ("TMDB_API_KEY") is a constant, not a row of acronyms.
pub fn label(key: &str) -> String {
    let constant = !key.chars().any(char::is_lowercase);
    let words = words(key);
    let mut out = String::new();
    for (n, word) in words.iter().enumerate() {
        let acronym = !constant && word.chars().count() >= 2 && word.chars().all(|c| !c.is_lowercase());
        let word = if acronym { word.clone() } else { word.to_lowercase() };
        if n > 0 {
            out.push(' ');
        }
        if n == 0 && !acronym {
            let mut chars = word.chars();
            out.extend(chars.next().map(|c| c.to_uppercase().collect::<String>()));
            out.push_str(chars.as_str());
        } else {
            out.push_str(&word);
        }
    }
    if out.is_empty() { key.to_string() } else { out }
}

/// True when the name of a setting says it holds a secret. The rule works on
/// the words of the name, so "SnapToKeyframe" and "EnableKeyboardControls"
/// are not secrets and "TmdbApiKey" is.
pub fn is_secret(key: &str) -> bool {
    const WORDS: [&str; 11] = [
        "key", "keys", "token", "tokens", "password", "passwords", "secret", "secrets", "apikey",
        "apikeys", "passphrase",
    ];
    const ENDINGS: [&str; 4] = ["token", "password", "secret", "apikey"];
    words(key).iter().any(|word| {
        let word = word.to_lowercase();
        WORDS.contains(&word.as_str()) || ENDINGS.iter().any(|end| word.ends_with(end))
    })
}

fn kind_of(value: &Value) -> Kind {
    match value {
        Value::Bool(_) => Kind::Bool,
        Value::Number(n) if n.is_i64() || n.is_u64() => Kind::Int,
        Value::Number(_) => Kind::Float,
        Value::String(text) if text.contains('\n') || text.chars().count() > LONG_TEXT => Kind::LongText,
        Value::String(_) => Kind::Text,
        Value::Null => Kind::Fixed("Not set. The editor leaves it as it is.".to_string()),
        Value::Array(items) if items.iter().all(Value::is_string) => Kind::List { numbers: false },
        Value::Array(items) if items.iter().all(Value::is_number) => Kind::List { numbers: true },
        Value::Array(items) => Kind::Fixed(format!(
            "A list of {} items that are not plain texts. The editor leaves it as it is.",
            items.len()
        )),
        Value::Object(_) => Kind::Fixed(String::new()),
    }
}

/// The fields of a configuration object, in the order of the object. The
/// plain values of the top level go in the group "Settings"; each nested
/// object is a group of its own, named by its path.
pub fn fields_of(root: &Value) -> Vec<PField> {
    fn walk(map: &serde_json::Map<String, Value>, path: &mut Vec<String>, secret: bool, out: &mut Vec<PField>) {
        let group = if path.is_empty() {
            "Settings".to_string()
        } else {
            path.iter().map(|key| label(key)).collect::<Vec<_>>().join(" / ")
        };
        for (key, value) in map {
            path.push(key.clone());
            let secret = secret || is_secret(key);
            match value {
                Value::Object(inner) => walk(inner, path, secret, out),
                value => out.push(PField {
                    path: path.clone(),
                    label: label(key),
                    kind: kind_of(value),
                    // Only text is hidden: a toggle or a count tells nothing.
                    secret: secret && matches!(kind_of(value), Kind::Text | Kind::LongText | Kind::List { .. }),
                    group: group.clone(),
                }),
            }
            path.pop();
        }
    }
    let mut out = Vec::new();
    if let Value::Object(map) = root {
        walk(map, &mut Vec::new(), false, &mut out);
    }
    out
}

pub fn get<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(root, |value, key| value.get(key))
}

/// Puts a value at a place that exists. The key keeps its place in the
/// object; no other key moves.
pub fn put(root: &mut Value, path: &[String], new: Value) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut place = root;
    for key in parents {
        match place.get_mut(key) {
            Some(next) => place = next,
            None => return false,
        }
    }
    match place.get_mut(last) {
        Some(slot) => {
            *slot = new;
            true
        }
        None => false,
    }
}

/// The value a field gets from text the user typed. The old value decides
/// the type of the new one: a float stays a float, an integer an integer,
/// and a text stays a text even when it looks like a number.
pub fn from_text(kind: &Kind, typed: &str) -> Result<Value, String> {
    let typed_trim = typed.trim();
    match kind {
        Kind::Bool => match typed_trim.to_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Ok(Value::Bool(true)),
            "false" | "off" | "no" | "0" => Ok(Value::Bool(false)),
            _ => Err("That is not true or false.".to_string()),
        },
        Kind::Int => {
            if let Ok(n) = typed_trim.parse::<i64>() {
                Ok(Value::from(n))
            } else if let Ok(n) = typed_trim.parse::<u64>() {
                Ok(Value::from(n))
            } else {
                // A setting that is whole now can be a float for the plugin.
                float(typed_trim)
            }
        }
        Kind::Float => float(typed_trim),
        Kind::Text | Kind::LongText => Ok(Value::String(typed.to_string())),
        Kind::List { numbers } => {
            let mut items = Vec::new();
            for entry in typed.split([',', '\n']).map(str::trim).filter(|e| !e.is_empty()) {
                items.push(if *numbers {
                    match entry.parse::<i64>() {
                        Ok(n) => Value::from(n),
                        Err(_) => float(entry)?,
                    }
                } else {
                    Value::String(entry.to_string())
                });
            }
            Ok(Value::Array(items))
        }
        Kind::Fixed(_) => Err("The editor does not change this value.".to_string()),
    }
}

fn float(typed: &str) -> Result<Value, String> {
    typed
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map(Value::Number)
        .ok_or_else(|| "That is not a number.".to_string())
}

/// A value as the text of its field.
fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ----- the editor ----------------------------------------------------------------

pub struct Editor {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The object as the server gave it.
    original: Value,
    /// The text the server sent, to check the round trip.
    raw: String,
    /// The copy the user edits.
    edited: Value,
    fields: Vec<PField>,
    filter: String,
    filter_input: Entity<InputState>,
    /// Paths of the secret values that are on show.
    revealed: HashSet<String>,
    saving: bool,
    _filter_change: Subscription,
}

impl Editor {
    pub fn dirty(&self) -> bool {
        self.original != self.edited
    }

    /// The fields whose value is not the one of the server.
    pub fn changed(&self) -> Vec<&PField> {
        self.fields
            .iter()
            .filter(|f| get(&self.original, &f.path) != get(&self.edited, &f.path))
            .collect()
    }

    /// The fields that match the filter, by group, in the order of the object.
    pub fn groups(&self) -> Vec<(String, Vec<usize>)> {
        let wanted = self.filter.trim().to_lowercase();
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (n, f) in self.fields.iter().enumerate() {
            if !wanted.is_empty()
                && ![f.label.to_lowercase(), f.key().to_lowercase(), f.group.to_lowercase()]
                    .iter()
                    .any(|text| text.contains(&wanted))
            {
                continue;
            }
            match groups.iter_mut().find(|(title, _)| *title == f.group) {
                Some((_, members)) => members.push(n),
                None => groups.push((f.group.clone(), vec![n])),
            }
        }
        groups
    }

    /// The field of a path such as "Server.Port"; the case does not matter.
    fn find(&self, key: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.key().eq_ignore_ascii_case(key))
    }

    fn value(&self, field: &PField) -> &Value {
        get(&self.edited, &field.path).unwrap_or(&Value::Null)
    }

    /// Sets a field from text. The error never holds the text.
    fn set_text(&mut self, n: usize, typed: &str) -> Result<(), String> {
        let field = &self.fields[n];
        let value = from_text(&field.kind, typed)?;
        put(&mut self.edited, &field.path.clone(), value);
        Ok(())
    }
}

impl Bloom {
    pub fn plugin_editor(&self) -> Option<&Editor> {
        match &self.page {
            Page::Admin(admin) => admin.plugin_editor.as_ref(),
            _ => None,
        }
    }

    fn plugin_editor_mut(&mut self) -> Option<&mut Editor> {
        match &mut self.page {
            Page::Admin(admin) => admin.plugin_editor.as_mut(),
            _ => None,
        }
    }

    /// Loads the settings of a plugin and shows them in the editor.
    pub fn open_plugin_config(
        &mut self,
        id: String,
        name: String,
        version: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter the settings"));
        let path = format!("/Plugins/{id}/Configuration");
        self.fetch(
            cx,
            move |client| client.get_text(&path, &[]),
            move |this, result, cx| {
                let result = result.and_then(|text| {
                    let object = serde_json::from_str::<Value>(&text)?;
                    Ok((text, object))
                });
                let (raw, object) = match result {
                    Ok((raw, object)) if object.is_object() => (raw, object),
                    Ok(_) => {
                        this.toast(name, "The plugin has no settings the editor can show.", cx);
                        return;
                    }
                    Err(err) => {
                        this.toast(name, format!("The settings did not load: {err:#}"), cx);
                        return;
                    }
                };
                let change = cx.subscribe(&filter_input, |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = input.read(cx).value().to_string();
                        if let Some(editor) = this.plugin_editor_mut() {
                            editor.filter = text;
                            cx.notify();
                        }
                    }
                });
                let editor = Editor {
                    fields: fields_of(&object),
                    original: object.clone(),
                    edited: object,
                    id,
                    name,
                    version,
                    raw,
                    filter: String::new(),
                    filter_input,
                    revealed: HashSet::new(),
                    saving: false,
                    _filter_change: change,
                };
                if let Page::Admin(admin) = &mut this.page {
                    admin.plugin_editor = Some(editor);
                    this.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
                }
                cx.notify();
            },
        );
    }

    fn plugin_put(&mut self, n: usize, value: Value, cx: &mut Context<Self>) {
        if let Some(editor) = self.plugin_editor_mut() {
            let path = editor.fields[n].path.clone();
            put(&mut editor.edited, &path, value);
            cx.notify();
        }
    }

    fn plugin_discard(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.plugin_editor_mut() {
            editor.edited = editor.original.clone();
            cx.notify();
        }
    }

    /// Leaves the editor; with unsaved changes the user confirms first.
    fn plugin_close(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.plugin_editor() else {
            return;
        };
        if editor.dirty() {
            self.ask_confirm(
                Confirm {
                    title: format!("Leave the settings of {}?", editor.name),
                    message: "The changes are not saved.".to_string(),
                    action: "Leave".to_string(),
                    danger: true,
                    run: Rc::new(|this, cx| this.plugin_leave(cx)),
                },
                cx,
            );
        } else {
            self.plugin_leave(cx);
        }
    }

    fn plugin_leave(&mut self, cx: &mut Context<Self>) {
        if let Page::Admin(admin) = &mut self.page {
            admin.plugin_editor = None;
        }
        super::lazy::forget("plugin.");
        cx.notify();
    }

    /// Sends the whole edited object, then reads it back: the editor then
    /// shows what the server holds.
    fn plugin_save(&mut self, cx: &mut Context<Self>) {
        self.plugin_send(false, cx)
    }

    /// `force` sends the object also when nothing changed; the debug
    /// channel uses it to check that a save without changes changes nothing.
    fn plugin_send(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.plugin_editor_mut() else {
            return;
        };
        if !(editor.dirty() || force) || editor.saving {
            return;
        }
        editor.saving = true;
        let (path, body) = (format!("/Plugins/{}/Configuration", editor.id), editor.edited.clone());
        self.fetch(
            cx,
            move |client| -> Result<Value> {
                client.post(&path, &body)?;
                client.get::<Value>(&path, &[])
            },
            |this, result, cx| {
                match result {
                    Ok(server) => {
                        this.toast("Settings saved", "", cx);
                        if let Some(editor) = this.plugin_editor_mut() {
                            editor.fields = fields_of(&server);
                            editor.raw = serde_json::to_string(&server).unwrap_or_default();
                            editor.original = server.clone();
                            editor.edited = server;
                            editor.saving = false;
                        }
                    }
                    Err(err) => {
                        this.toast("The server refused the change", format!("{err:#}"), cx);
                        if let Some(editor) = this.plugin_editor_mut() {
                            editor.saving = false;
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    fn plugin_toggle_reveal(&mut self, n: usize, cx: &mut Context<Self>) {
        if let Some(editor) = self.plugin_editor_mut() {
            let key = editor.fields[n].key();
            if !editor.revealed.remove(&key) {
                editor.revealed.insert(key);
            }
            cx.notify();
        }
    }

    /// Opens the form of a field: a prompt, or the large dialog for a long
    /// text.
    fn plugin_ask(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.plugin_editor() else {
            return;
        };
        let field = editor.fields[n].clone();
        let current = shown(editor.value(&field));
        let run_kind = field.kind.clone();
        let label = field.label.clone();
        match field.kind {
            Kind::LongText => self.open_text_editor(
                TextEdit {
                    title: field.label.clone(),
                    message: "The text as the plugin stores it.".to_string(),
                    action: "Use this text".to_string(),
                    mono: false,
                    value: current,
                    apply: Rc::new(move |this, value, cx| this.plugin_put(n, Value::String(value), cx)),
                },
                window,
                cx,
            ),
            Kind::Int | Kind::Float | Kind::Text | Kind::List { .. } => {
                let message = match &field.kind {
                    Kind::List { .. } => "Put a comma between two values.",
                    Kind::Int => "A whole number.",
                    Kind::Float => "A number.",
                    _ => "",
                };
                self.ask_prompt(
                    Prompt {
                        title: field.label.clone(),
                        message: message.to_string(),
                        action: "Set".to_string(),
                        fields: vec![PromptField {
                            label: field.label.clone(),
                            placeholder: String::new(),
                            value: current,
                            masked: field.secret,
                            required: false,
                        }],
                        run: Rc::new(move |this, values, cx| {
                            let typed = values.first().map(String::as_str).unwrap_or_default();
                            match from_text(&run_kind, typed) {
                                Ok(value) => this.plugin_put(n, value, cx),
                                Err(note) => this.toast(label.clone(), note, cx),
                            }
                        }),
                    },
                    cx,
                );
            }
            Kind::Bool | Kind::Fixed(_) => {}
        }
    }

    /// The bar with Save and Discard, while the editor has changes.
    pub fn render_plugin_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let editor = self.plugin_editor().filter(|e| e.dirty())?;
        let changes = editor.changed().len().max(1);
        Some(bar_shell(
            changes,
            button("plugin.discard", "Discard", ButtonKind::Plain, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.plugin_discard(cx))),
            button("plugin.save", "Save", ButtonKind::Primary, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.plugin_save(cx))),
            cx,
        ))
    }

    /// The commands of the editor for the debug channel.
    pub fn debug_plugin_config(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" | "state" => {}
            "open" => {
                let wanted = arg.trim().to_lowercase();
                let found = match &self.page {
                    Page::Admin(admin) => admin.plugins.as_ref().and_then(|data| {
                        data.plugins
                            .iter()
                            .find(|p| p.name.to_lowercase() == wanted)
                            .or_else(|| data.plugins.iter().find(|p| p.name.to_lowercase().contains(&wanted)))
                            .map(|p| (p.id.clone(), p.name.clone(), p.version.clone(), data.settings.contains(&p.id)))
                    }),
                    _ => None,
                };
                match found {
                    None => return "error: open the Plugins page first (admin plugins), and use a plugin name".into(),
                    Some((_, name, _, false)) => return format!("error: {name} has no settings"),
                    Some((id, name, version, true)) => self.open_plugin_config(id, name, version, window, cx),
                }
            }
            "set" => {
                let (key, value) = arg.split_once(' ').unwrap_or((arg, ""));
                let Some(editor) = self.plugin_editor_mut() else {
                    return "error: no plugin editor is open".into();
                };
                let Some(n) = editor.find(key) else {
                    return format!("error: no field {key:?}");
                };
                if let Err(note) = editor.set_text(n, value) {
                    return format!("error: {note}");
                }
                cx.notify();
            }
            "filter" => {
                let Some(editor) = self.plugin_editor_mut() else {
                    return "error: no plugin editor is open".into();
                };
                let input = editor.filter_input.clone();
                let text = arg.to_string();
                editor.filter = text.clone();
                input.update(cx, |input, cx| input.set_value(text, window, cx));
                cx.notify();
            }
            "reveal" => {
                let Some(editor) = self.plugin_editor() else {
                    return "error: no plugin editor is open".into();
                };
                let Some(n) = editor.find(arg.trim()) else {
                    return format!("error: no field {arg:?}");
                };
                self.plugin_toggle_reveal(n, cx);
            }
            "save" => self.plugin_save(cx),
            "save-unchanged" => self.plugin_send(true, cx),
            "discard" => self.plugin_discard(cx),
            "close" => self.plugin_leave(cx),
            _ => return "error: plugin-config open <name>|state|set <Key.Path> <value>|filter <text>|reveal <Key.Path>|save|save-unchanged|discard|close".into(),
        }
        self.plugin_config_state()
    }

    /// The editor in a line. It names the changed keys and never shows a
    /// value.
    pub fn plugin_config_state(&self) -> String {
        let Some(editor) = self.plugin_editor() else {
            return "plugin-config: no editor open".into();
        };
        let groups = editor.groups();
        let shown: usize = groups.iter().map(|(_, members)| members.len()).sum();
        let changed: Vec<String> = editor.changed().iter().map(|f| f.key()).collect();
        let secrets = editor.fields.iter().filter(|f| f.secret).count();
        format!(
            "plugin-config: {} v{}: {} fields ({} hidden), {} groups, {} shown, filter={:?}, dirty={}, saving={}, roundtrip={}, edited=[{}]",
            editor.name,
            editor.version,
            editor.fields.len(),
            secrets,
            groups.len(),
            shown,
            editor.filter,
            editor.dirty(),
            editor.saving,
            serde_json::to_string(&editor.original).is_ok_and(|text| text == editor.raw),
            changed.join(", ")
        )
    }
}

// ----- the page ------------------------------------------------------------------

/// One row of the editor: the label of a field with its control.
fn field_row(editor: &Editor, n: usize, cx: &mut Context<Bloom>) -> Div {
    let f = &editor.fields[n];
    let id = SharedString::from(format!("plugin.field.{n}"));
    let value = editor.value(f).clone();
    let revealed = editor.revealed.contains(&f.key());
    let hidden = f.secret && !revealed;
    let t = UiTheme::read(cx).clone();
    let control: Div = match &f.kind {
        Kind::Bool => {
            let on = value.as_bool().unwrap_or(false);
            div().child(checkbox(id, on, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.plugin_put(n, Value::Bool(!on), cx)
            })))
        }
        Kind::Fixed(note) => div()
            .max_w(px(360.))
            .text_size(px(13.))
            .text_color(t.colors.foreground.opacity(0.5))
            .child(note.clone()),
        _ => {
            let text = shown(&value);
            let lines = text.split('\n').count();
            let on_screen = match (text.is_empty(), hidden) {
                (true, _) => String::new(),
                (false, true) => HIDDEN.to_string(),
                (false, false) if lines > 1 => format!("{lines} lines"),
                (false, false) => text.clone(),
            };
            let mut control = div().flex().items_center().gap(px(8.)).child(
                value_button(id, on_screen, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.plugin_ask(n, window, cx),
                )),
            );
            if f.secret && !text.is_empty() {
                control = control.child(
                    button(
                        SharedString::from(format!("plugin.reveal.{n}")),
                        if revealed { "Hide" } else { "Reveal" },
                        ButtonKind::Plain,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.plugin_toggle_reveal(n, cx)
                    })),
                );
            }
            control
        }
    };
    let help = if hidden && !shown(&value).is_empty() {
        "Hidden until you choose Reveal."
    } else {
        ""
    };
    field(f.label.clone(), help, control, cx)
}

pub fn render(app: &Bloom, editor: &Editor, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let web = app.session.as_ref().map(|s| s.client.web_plugin_url(&editor.id));
    let groups = editor.groups();

    let mut page = div().max_w(px(920.)).flex().flex_col().gap(px(18.));
    page = page.child(
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(
                button("plugin.back", "All plugins", ButtonKind::Plain, cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.plugin_close(cx))),
            )
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(t.colors.foreground)
                    .child(format!("{} settings", editor.name)),
            )
            .child(
                div()
                    .font_family("Menlo")
                    .text_size(px(12.))
                    .text_color(t.colors.muted_foreground)
                    .child(format!("v{}", editor.version)),
            ),
    );
    let mut note = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .px(px(4.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(14.))
                .line_height(px(20.))
                .text_color(t.colors.foreground.opacity(0.7))
                .child(
                    "A generic view of the settings the server stores for this plugin. The \
                     labels come from the names of the settings, and the plugin may check \
                     values that this editor cannot. A save sends every setting back.",
                ),
        );
    if let Some(url) = web {
        note = note.child(
            button("plugin.web", "Open settings in the web", ButtonKind::Plain, cx)
                .on_click(move |_: &ClickEvent, _, cx| cx.open_url(&url)),
        );
    }
    page = page.child(note);
    if editor.fields.len() > FILTER_MIN {
        page = page.child(Input::new(&editor.filter_input).aria_label("Filter the settings").w(px(360.)));
    }
    if groups.is_empty() {
        page = page.child(
            div()
                .py(px(24.))
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child(if editor.fields.is_empty() {
                    "This plugin has no settings."
                } else {
                    "No setting matches the filter."
                }),
        );
    }
    for (title, members) in groups {
        let mut card = group(title.clone(), cx);
        if members.len() >= LAZY_MIN {
            let (plugin, filter, group_title) = (editor.id.clone(), editor.filter.clone(), title.clone());
            card = card.child(super::lazy::lazy(
                cx,
                format!("plugin.{plugin}.{group_title}.{filter}.{}", members.len()),
                members.len(),
                move |app, range, cx| {
                    let Some(editor) = app.plugin_editor() else {
                        return Vec::new();
                    };
                    let groups = editor.groups();
                    let Some((_, members)) = groups.iter().find(|(t, _)| *t == group_title) else {
                        return Vec::new();
                    };
                    range
                        .filter_map(|i| members.get(i))
                        .map(|&n| gpui_kit::IntoElement::into_any_element(field_row(editor, n, cx)))
                        .collect()
                },
            ));
        } else {
            for &n in &members {
                card = card.child(field_row(editor, n, cx));
            }
        }
        page = page.child(card);
    }
    // Room for the bar with Save, so it does not cover the last field.
    page.child(div().h(px(if editor.dirty() { 72. } else { 0. })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An object as a plugin could send it: every kind of value, a nested
    /// object, an unknown value and an order that is not alphabetical.
    fn sample() -> Value {
        serde_json::from_str(
            r#"{
              "Zeta": true,
              "EnableAutoSkip": false,
              "MaxRetries": 3,
              "Ratio": 0.5,
              "WholeFloat": 1.0,
              "Big": 18446744073709551615,
              "Negative": -7,
              "Name": "Intro",
              "TmdbApiKey": "abc123",
              "Languages": ["en", "de"],
              "Ports": [80, 443],
              "Nothing": null,
              "Rules": [{"A": 1}, {"A": 2}],
              "Mixed": [1, "a"],
              "Server": { "Host": "h", "Retry": { "Count": 2, "Wait": 1.5 }, "Token": "t" },
              "Alpha": "last"
            }"#,
        )
        .unwrap()
    }

    fn editor_fields(value: &Value) -> Vec<PField> {
        fields_of(value)
    }

    #[test]
    fn round_trip_keeps_every_key_type_and_order() {
        let original = sample();
        // The text of the object is the same after a copy and a serialize.
        let edited = original.clone();
        let text = serde_json::to_string(&edited).unwrap();
        assert_eq!(text, serde_json::to_string(&original).unwrap());
        // The keys are in the order the server sent them, not sorted.
        let keys: Vec<&String> = original.as_object().unwrap().keys().collect();
        assert_eq!(keys[0], "Zeta");
        assert_eq!(keys[1], "EnableAutoSkip");
        assert_eq!(keys.last().unwrap().as_str(), "Alpha");
        // A whole float stays a float, an integer stays an integer.
        assert!(text.contains("\"WholeFloat\":1.0"));
        assert!(text.contains("\"MaxRetries\":3,"));
        assert!(text.contains("\"Big\":18446744073709551615"));
        assert!(text.contains("\"Nothing\":null"));
        // Reading the fields changes nothing.
        let fields = editor_fields(&original);
        assert!(!fields.is_empty());
        assert_eq!(serde_json::to_string(&original).unwrap(), text);
    }

    #[test]
    fn an_edit_changes_only_its_key() {
        let original = sample();
        let mut edited = original.clone();
        assert!(put(&mut edited, &["EnableAutoSkip".to_string()], json!(true)));
        assert!(put(&mut edited, &["Server".into(), "Retry".into(), "Count".into()], json!(5)));
        // Text with a different value only at the two places.
        let before = serde_json::to_string(&original).unwrap();
        let after = serde_json::to_string(&edited).unwrap();
        assert_eq!(
            after,
            before
                .replace("\"EnableAutoSkip\":false", "\"EnableAutoSkip\":true")
                .replace("\"Count\":2", "\"Count\":5")
        );
        // A key that does not exist is not made.
        assert!(!put(&mut edited, &["Nope".to_string()], json!(1)));
        assert_eq!(edited.as_object().unwrap().len(), original.as_object().unwrap().len());
    }

    #[test]
    fn fields_follow_the_kind_of_each_value() {
        let fields = editor_fields(&sample());
        let kind = |key: &str| fields.iter().find(|f| f.key() == key).map(|f| f.kind.clone());
        assert_eq!(kind("Zeta"), Some(Kind::Bool));
        assert_eq!(kind("MaxRetries"), Some(Kind::Int));
        assert_eq!(kind("Big"), Some(Kind::Int));
        assert_eq!(kind("Negative"), Some(Kind::Int));
        assert_eq!(kind("Ratio"), Some(Kind::Float));
        assert_eq!(kind("WholeFloat"), Some(Kind::Float));
        assert_eq!(kind("Name"), Some(Kind::Text));
        assert_eq!(kind("Languages"), Some(Kind::List { numbers: false }));
        assert_eq!(kind("Ports"), Some(Kind::List { numbers: true }));
        assert!(matches!(kind("Nothing"), Some(Kind::Fixed(_))));
        assert!(matches!(kind("Rules"), Some(Kind::Fixed(_))));
        assert!(matches!(kind("Mixed"), Some(Kind::Fixed(_))));
        // A nested object is a group, with the nesting in its title.
        let retry = fields.iter().find(|f| f.key() == "Server.Retry.Count").unwrap();
        assert_eq!(retry.group, "Server / Retry");
        let host = fields.iter().find(|f| f.key() == "Server.Host").unwrap();
        assert_eq!(host.group, "Server");
        assert_eq!(fields.iter().find(|f| f.key() == "Name").unwrap().group, "Settings");
        // Every plain value has a field, and an object has none of its own.
        assert_eq!(fields.len(), 19);
    }

    #[test]
    fn typed_text_keeps_the_type_of_the_field() {
        assert_eq!(from_text(&Kind::Int, " 42 "), Ok(json!(42)));
        assert_eq!(serde_json::to_string(&from_text(&Kind::Float, "2").unwrap()).unwrap(), "2.0");
        assert_eq!(from_text(&Kind::Float, "0.25"), Ok(json!(0.25)));
        // A text that looks like a number stays a text.
        assert_eq!(from_text(&Kind::Text, "123"), Ok(json!("123")));
        assert!(from_text(&Kind::Int, "abc").is_err());
        assert!(from_text(&Kind::Float, "NaN").is_err());
        assert_eq!(from_text(&Kind::Bool, "True"), Ok(json!(true)));
        assert_eq!(from_text(&Kind::List { numbers: true }, "80, 443"), Ok(json!([80, 443])));
        assert_eq!(from_text(&Kind::List { numbers: false }, "a,\n b ,"), Ok(json!(["a", "b"])));
        assert_eq!(from_text(&Kind::List { numbers: false }, ""), Ok(json!([])));
        assert!(from_text(&Kind::Fixed(String::new()), "x").is_err());
    }

    #[test]
    fn makes_labels_from_names() {
        assert_eq!(label("EnableAutoSkip"), "Enable auto skip");
        assert_eq!(label("MaxRetries"), "Max retries");
        assert_eq!(label("TMDB_API_KEY"), "Tmdb api key");
        assert_eq!(label("HTTPServerPort"), "HTTP server port");
        assert_eq!(label("H264Crf"), "H264 crf");
        assert_eq!(label("name"), "Name");
        assert_eq!(label("Skip_intro-length"), "Skip intro length");
        assert_eq!(label("X"), "X");
        assert_eq!(label(""), "");
    }

    #[test]
    fn hides_the_values_of_secret_names() {
        for key in ["TmdbApiKey", "TMDB_API_KEY", "ApiKey", "Password", "AccessToken", "AnalyticsInstallSecret", "ApiKeys", "ClientSecret", "token"] {
            assert!(is_secret(key), "{key}");
        }
        for key in ["SnapToKeyframe", "EnableKeyboardControls", "StripCollectionKeywords", "Name", "Keyframes"] {
            assert!(!is_secret(key), "{key}");
        }
    }

    #[test]
    fn secret_fields_are_text_only_and_the_secret_spreads_to_children() {
        let fields = editor_fields(&sample());
        let secret = |key: &str| fields.iter().find(|f| f.key() == key).unwrap().secret;
        assert!(secret("TmdbApiKey"));
        assert!(secret("Server.Token"));
        assert!(!secret("Server.Host"));
        // A toggle named like a secret has nothing to hide.
        let object = json!({ "EnableKeyboardControls": true, "SecretMode": true, "Tokens": ["a"], "Secrets": { "Inner": "x", "Flag": false } });
        let fields = fields_of(&object);
        let secret = |key: &str| fields.iter().find(|f| f.key() == key).unwrap().secret;
        assert!(!secret("SecretMode"));
        assert!(secret("Tokens"));
        assert!(secret("Secrets.Inner"));
        assert!(!secret("Secrets.Flag"));
    }
}
