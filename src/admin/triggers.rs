// SPDX-License-Identifier: AGPL-3.0-or-later
//! The triggers of one scheduled task: when the server starts the task. The
//! editor is a page inside the Tasks section. It lists the triggers in plain
//! words, removes one, and adds one (daily, weekly, every so many hours, or
//! when the server starts). Each change reads the triggers from the server
//! again, changes that list, and sends the whole array back.

use std::rc::Rc;

use anyhow::{Result, bail};
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgba,
};
use serde_json::{Value, json};

use super::{
    ButtonKind, Field as Form, Prompt, button,
    config::value_button,
    users::action,
};
use crate::{
    app::{Bloom, Page},
    settings::{Choice, field, group, select},
    ui::theme::UiTheme,
};

/// Server ticks in one second, and in one minute.
const TICKS_PER_SECOND: i64 = 10_000_000;
const TICKS_PER_MINUTE: i64 = 60 * TICKS_PER_SECOND;
const DAYS: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

// ----- words and ticks ----------------------------------------------------------

/// A time of day in ticks as "02:00" (24 hours, local time of the server).
pub fn time_text(ticks: i64) -> String {
    let minutes = ticks / TICKS_PER_MINUTE;
    format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60)
}

/// A time such as "2:30" or "02:30" as minutes after midnight.
pub fn parse_time(text: &str) -> Option<i64> {
    let (hour, minute) = text.trim().split_once(':')?;
    let (hour, minute): (i64, i64) = (hour.trim().parse().ok()?, minute.trim().parse().ok()?);
    ((0..24).contains(&hour) && (0..60).contains(&minute)).then_some(hour * 60 + minute)
}

/// A number of hours ("12", "1.5") as whole minutes, at least one.
pub fn parse_hours(text: &str) -> Option<i64> {
    let hours: f64 = text.trim().parse().ok()?;
    (hours.is_finite() && hours > 0.).then(|| ((hours * 60.).round() as i64).max(1))
}

/// A length of time in words: "2 hours", "1 hour 30 minutes", "45 minutes".
fn span(minutes: i64) -> String {
    let unit = |n: i64, name: &str| if n == 1 { format!("{n} {name}") } else { format!("{n} {name}s") };
    match (minutes / 60, minutes % 60) {
        (0, m) => unit(m, "minute"),
        (h, 0) => unit(h, "hour"),
        (h, m) => format!("{} {}", unit(h, "hour"), unit(m, "minute")),
    }
}

/// How often an interval trigger fires: "Every 12 hours", "Every 90 minutes".
fn every(ticks: i64) -> String {
    let seconds = (ticks / TICKS_PER_SECOND).max(1);
    match seconds {
        s if s < 60 || s % 60 != 0 => match s {
            1 => "Every second".to_string(),
            s => format!("Every {s} seconds"),
        },
        s if s % 3600 == 0 => match s / 3600 {
            1 => "Every hour".to_string(),
            h => format!("Every {h} hours"),
        },
        s => format!("Every {} minutes", s / 60),
    }
}

fn day_name(trigger: &Value) -> Option<&'static str> {
    match trigger.get("DayOfWeek")? {
        Value::String(name) => DAYS.iter().copied().find(|d| d.eq_ignore_ascii_case(name)),
        Value::Number(n) => DAYS.get(n.as_u64()? as usize).copied(),
        _ => None,
    }
}

/// One trigger in plain words: "Every day at 02:00", "On Sundays at 03:00",
/// "Every 12 hours", "When the server starts". A time limit follows.
pub fn describe(trigger: &Value) -> String {
    let kind = trigger.get("Type").and_then(Value::as_str).unwrap_or("");
    let at = time_text(trigger.get("TimeOfDayTicks").and_then(Value::as_i64).unwrap_or(0));
    let mut text = match kind {
        "DailyTrigger" => format!("Every day at {at}"),
        "WeeklyTrigger" => match day_name(trigger) {
            Some(day) => format!("On {day}s at {at}"),
            None => format!("Every week at {at}"),
        },
        "IntervalTrigger" => every(trigger.get("IntervalTicks").and_then(Value::as_i64).unwrap_or(0)),
        "StartupTrigger" => "When the server starts".to_string(),
        other => other.trim_end_matches("Trigger").to_string(),
    };
    if let Some(ticks) = trigger.get("MaxRuntimeTicks").and_then(Value::as_i64) {
        text.push_str(&format!(", stops after {}", span((ticks / TICKS_PER_MINUTE).max(1))));
    }
    text
}

/// The schedule of a task in one line, for its row.
pub fn schedule(triggers: &[Value]) -> String {
    match triggers {
        [] => "Manual only".to_string(),
        triggers => triggers.iter().map(describe).collect::<Vec<_>>().join(", "),
    }
}

// ----- the trigger to add ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Daily,
    Weekly,
    Interval,
    Startup,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Daily, Kind::Weekly, Kind::Interval, Kind::Startup];

    fn label(self) -> &'static str {
        match self {
            Kind::Daily => "Daily",
            Kind::Weekly => "Weekly",
            Kind::Interval => "Interval",
            Kind::Startup => "Startup",
        }
    }
}

/// The form of a new trigger.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub kind: Kind,
    /// Minutes after midnight, for a daily or weekly trigger.
    pub minutes: i64,
    /// Index in `DAYS`, for a weekly trigger.
    pub day: usize,
    pub interval_minutes: i64,
    /// The task stops after this long; `None` lets it run to its end.
    pub limit_minutes: Option<i64>,
}

impl Default for Draft {
    fn default() -> Self {
        Self { kind: Kind::Daily, minutes: 3 * 60, day: 0, interval_minutes: 12 * 60, limit_minutes: None }
    }
}

impl Draft {
    /// The trigger as the server takes it.
    pub fn to_value(&self) -> Value {
        let mut trigger = match self.kind {
            Kind::Daily => json!({ "Type": "DailyTrigger", "TimeOfDayTicks": self.minutes * TICKS_PER_MINUTE }),
            Kind::Weekly => json!({
                "Type": "WeeklyTrigger",
                "DayOfWeek": DAYS[self.day % 7],
                "TimeOfDayTicks": self.minutes * TICKS_PER_MINUTE,
            }),
            Kind::Interval => json!({ "Type": "IntervalTrigger", "IntervalTicks": self.interval_minutes * TICKS_PER_MINUTE }),
            Kind::Startup => json!({ "Type": "StartupTrigger" }),
        };
        if let Some(limit) = self.limit_minutes {
            trigger["MaxRuntimeTicks"] = json!(limit * TICKS_PER_MINUTE);
        }
        trigger
    }

    /// A draft from words: `daily 02:00`, `weekly Sunday 03:00`,
    /// `interval 12` (hours), `startup`, each with an optional `limit <hours>`.
    pub fn parse(text: &str) -> Result<Self> {
        let mut words: Vec<&str> = text.split_whitespace().collect();
        let mut draft = Draft::default();
        if let Some(at) = words.iter().position(|w| w.eq_ignore_ascii_case("limit")) {
            let Some(hours) = words.get(at + 1).and_then(|w| parse_hours(w)) else {
                bail!("limit needs a number of hours");
            };
            draft.limit_minutes = Some(hours);
            words.truncate(at);
        }
        let mut words = words.into_iter();
        let kind = words.next().unwrap_or("").to_lowercase();
        let time = |word: Option<&str>| word.and_then(parse_time).ok_or_else(|| anyhow::anyhow!("a time such as 02:00 is missing"));
        match kind.as_str() {
            "daily" => {
                draft.kind = Kind::Daily;
                draft.minutes = time(words.next())?;
            }
            "weekly" => {
                draft.kind = Kind::Weekly;
                let day = words.next().unwrap_or("");
                let Some(at) = DAYS.iter().position(|d| d.eq_ignore_ascii_case(day)) else {
                    bail!("a day such as Sunday is missing");
                };
                draft.day = at;
                draft.minutes = time(words.next())?;
            }
            "interval" => {
                draft.kind = Kind::Interval;
                let Some(minutes) = words.next().and_then(parse_hours) else {
                    bail!("a number of hours is missing");
                };
                draft.interval_minutes = minutes;
            }
            "startup" => draft.kind = Kind::Startup,
            _ => bail!("daily <HH:MM> | weekly <Day> <HH:MM> | interval <hours> | startup"),
        }
        if let Some(extra) = words.next() {
            bail!("what is {extra:?}?");
        }
        Ok(draft)
    }
}

// ----- the editor ------------------------------------------------------------------

pub struct Editor {
    pub task_id: String,
    pub name: String,
    pub description: String,
    pub loading: bool,
    /// A change is on its way to the server.
    pub busy: bool,
    pub error: Option<String>,
    pub triggers: Vec<Value>,
    pub draft: Draft,
}

/// A change of the list of triggers.
enum Change {
    Add(Value),
    Remove(Value),
}

impl Bloom {
    fn triggers_editor(&self) -> Option<&Editor> {
        match &self.page {
            Page::Admin(admin) => admin.triggers.as_ref(),
            _ => None,
        }
    }

    fn triggers_editor_mut(&mut self) -> Option<&mut Editor> {
        match &mut self.page {
            Page::Admin(admin) => admin.triggers.as_mut(),
            _ => None,
        }
    }

    /// Opens the editor of a task and reads its triggers from the server.
    pub fn triggers_open(&mut self, id: &str, name: &str, description: &str, cx: &mut Context<Self>) {
        let editor = Editor {
            task_id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            loading: true,
            busy: false,
            error: None,
            triggers: Vec::new(),
            draft: Draft::default(),
        };
        if let Page::Admin(admin) = &mut self.page {
            admin.triggers = Some(editor);
        }
        cx.notify();
        let (work_id, id) = (id.to_string(), id.to_string());
        self.fetch(
            cx,
            move |client| read_triggers(&client, &work_id),
            move |this, result, cx| {
                if let Some(editor) = this.triggers_editor_mut()
                    && editor.task_id == id
                {
                    editor.loading = false;
                    match result {
                        Ok(list) => editor.triggers = list,
                        Err(err) => editor.error = Some(format!("{err:#}")),
                    }
                }
                cx.notify();
            },
        );
    }

    /// Opens the editor of the task with this name. For the debug channel.
    pub fn triggers_open_named(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let Page::Admin(admin) = &self.page else {
            return false;
        };
        let found = admin.tasks.as_ref().and_then(|data| {
            data.groups
                .iter()
                .flat_map(|(_, list)| list.iter())
                .find(|task| task.name.eq_ignore_ascii_case(name))
                .map(|task| (task.id.clone(), task.name.clone(), task.description.clone()))
        });
        match found {
            Some((id, name, description)) => {
                self.triggers_open(&id, &name, &description, cx);
                true
            }
            None => false,
        }
    }

    pub fn triggers_close(&mut self, cx: &mut Context<Self>) {
        if let Page::Admin(admin) = &mut self.page {
            admin.triggers = None;
        }
        cx.notify();
    }

    fn triggers_draft(&mut self, change: impl FnOnce(&mut Draft), cx: &mut Context<Self>) {
        if let Some(editor) = self.triggers_editor_mut() {
            change(&mut editor.draft);
        }
        cx.notify();
    }

    /// Changes the list on the server. The list is read again first; a
    /// removal takes out the first trigger equal to the one the user chose.
    fn triggers_apply(&mut self, change: Change, cx: &mut Context<Self>) {
        let Some(editor) = self.triggers_editor_mut() else {
            return;
        };
        if editor.busy || editor.loading {
            return;
        }
        editor.busy = true;
        let id = editor.task_id.clone();
        let work_id = id.clone();
        let done = match &change {
            Change::Add(_) => "Trigger added",
            Change::Remove(_) => "Trigger removed",
        };
        cx.notify();
        self.fetch(
            cx,
            move |client| -> Result<Vec<Value>> {
                let mut list = read_triggers(&client, &work_id)?;
                match &change {
                    Change::Add(trigger) => list.push(trigger.clone()),
                    Change::Remove(trigger) => match list.iter().position(|t| t == trigger) {
                        Some(at) => {
                            list.remove(at);
                        }
                        None => bail!("the trigger is not on the server any more"),
                    },
                }
                client.post(&format!("/ScheduledTasks/{work_id}/Triggers"), &list)?;
                read_triggers(&client, &work_id)
            },
            move |this, result, cx| {
                match &result {
                    Ok(_) => this.toast(done, "", cx),
                    Err(err) => this.toast("The server refused the change", format!("{err:#}"), cx),
                }
                if let Some(editor) = this.triggers_editor_mut()
                    && editor.task_id == id
                {
                    editor.busy = false;
                    if let Ok(list) = result {
                        editor.triggers = list;
                    }
                }
                // The task list behind shows the new schedule.
                if matches!(this.page, Page::Admin(_)) {
                    this.load_page(cx);
                }
                cx.notify();
            },
        );
    }

    /// Removes the trigger at a place of the list (1 is the first).
    pub fn triggers_remove(&mut self, place: usize, cx: &mut Context<Self>) -> Result<()> {
        let Some(trigger) = self
            .triggers_editor()
            .and_then(|editor| editor.triggers.get(place.wrapping_sub(1)).cloned())
        else {
            bail!("no trigger at place {place}");
        };
        self.triggers_apply(Change::Remove(trigger), cx);
        Ok(())
    }

    /// Adds a trigger from words; see [`Draft::parse`]. For the debug channel.
    pub fn triggers_add_text(&mut self, text: &str, cx: &mut Context<Self>) -> Result<()> {
        if self.triggers_editor().is_none() {
            bail!("the trigger editor is not open");
        }
        let draft = Draft::parse(text)?;
        self.triggers_apply(Change::Add(draft.to_value()), cx);
        Ok(())
    }

    /// The triggers in a numbered list, for the debug channel.
    pub fn triggers_describe(&self) -> String {
        let Some(editor) = self.triggers_editor() else {
            return "trigger editor closed".to_string();
        };
        let mut lines = format!(
            "task={} loading={} busy={} error={:?} triggers={}",
            editor.name,
            editor.loading,
            editor.busy,
            editor.error,
            editor.triggers.len()
        );
        for (n, trigger) in editor.triggers.iter().enumerate() {
            lines.push_str(&format!("\n{}. {}  {}", n + 1, describe(trigger), trigger));
        }
        lines
    }

    /// `triggers open|list|add|remove`; see `Bloom::debug_access`.
    pub fn debug_triggers(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        let arg = arg.trim();
        match verb {
            "open" => {
                if !self.triggers_open_named(arg, cx) {
                    return "error: no such task on the Scheduled Tasks page (open it with: admin tasks)".into();
                }
            }
            "list" | "" => {}
            "add" => {
                if let Err(err) = self.triggers_add_text(arg, cx) {
                    return format!("error: {err:#}");
                }
            }
            // Fills the form of a new trigger without sending it.
            "draft" => match Draft::parse(arg) {
                Ok(draft) => self.triggers_draft(|d| *d = draft, cx),
                Err(err) => return format!("error: {err:#}"),
            },
            "remove" => match arg.parse::<usize>() {
                Ok(place) => {
                    if let Err(err) = self.triggers_remove(place, cx) {
                        return format!("error: {err:#}");
                    }
                }
                Err(_) => return "error: triggers remove <place>".into(),
            },
            _ => {
                return "error: triggers open <task> | list | add <daily HH:MM | weekly <Day> HH:MM | \
                        interval <hours> | startup> [limit <hours>] | draft <as add> | remove <place>"
                    .into();
            }
        }
        self.triggers_describe()
    }

    /// The form of the time limit.
    fn triggers_ask_limit(&mut self, current: Option<i64>, cx: &mut Context<Self>) {
        let prompt = Prompt {
            title: "Time limit".to_string(),
            message: "The task stops when it runs longer than this. Leave the field empty for no limit.".to_string(),
            action: "Set".to_string(),
            fields: vec![Form {
                label: "Hours".to_string(),
                placeholder: "No limit".to_string(),
                value: current.map(|m| hours_text(m)).unwrap_or_default(),
                masked: false,
                required: false,
            }],
            run: Rc::new(|this, values, cx| {
                let text = values.into_iter().next().unwrap_or_default();
                match (text.is_empty(), parse_hours(&text)) {
                    (true, _) => this.triggers_draft(|d| d.limit_minutes = None, cx),
                    (false, Some(minutes)) => this.triggers_draft(|d| d.limit_minutes = Some(minutes), cx),
                    (false, None) => this.toast("This is not a number of hours", "", cx),
                }
            }),
        };
        self.ask_prompt(prompt, cx);
    }

    fn triggers_ask_time(&mut self, current: i64, cx: &mut Context<Self>) {
        let prompt = Prompt {
            title: "Time of day".to_string(),
            message: "The time on the server, as hours and minutes: 02:30.".to_string(),
            action: "Set".to_string(),
            fields: vec![Form {
                label: "Time".to_string(),
                placeholder: "HH:MM".to_string(),
                value: format!("{:02}:{:02}", current / 60, current % 60),
                masked: false,
                required: true,
            }],
            run: Rc::new(|this, values, cx| match values.first().and_then(|t| parse_time(t)) {
                Some(minutes) => this.triggers_draft(|d| d.minutes = minutes, cx),
                None => this.toast("This is not a time", "Write hours and minutes: 02:30.", cx),
            }),
        };
        self.ask_prompt(prompt, cx);
    }

    fn triggers_ask_interval(&mut self, current: i64, cx: &mut Context<Self>) {
        let prompt = Prompt {
            title: "Interval".to_string(),
            message: "How many hours pass between two runs. A part of an hour is fine: 1.5.".to_string(),
            action: "Set".to_string(),
            fields: vec![Form {
                label: "Hours".to_string(),
                placeholder: "Hours".to_string(),
                value: hours_text(current),
                masked: false,
                required: true,
            }],
            run: Rc::new(|this, values, cx| match values.first().and_then(|t| parse_hours(t)) {
                Some(minutes) => this.triggers_draft(|d| d.interval_minutes = minutes, cx),
                None => this.toast("This is not a number of hours", "", cx),
            }),
        };
        self.ask_prompt(prompt, cx);
    }
}

/// Hours as text: 90 minutes is "1.5".
fn hours_text(minutes: i64) -> String {
    let hours = minutes as f64 / 60.;
    if hours.fract() == 0. { format!("{hours:.0}") } else { format!("{hours:.2}").trim_end_matches('0').to_string() }
}

fn read_triggers(client: &crate::jellyfin::Client, id: &str) -> Result<Vec<Value>> {
    let task: Value = client.get(&format!("/ScheduledTasks/{id}"), &[])?;
    Ok(task.get("Triggers").and_then(Value::as_array).cloned().unwrap_or_default())
}

// ----- the page -------------------------------------------------------------------

pub fn render(editor: &Editor, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let mut page = div().max_w(px(920.)).flex().flex_col().gap(px(18.)).child(
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(
                action("triggers.back", LucideIcon::ArrowLeft, "Tasks", false, cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.triggers_close(cx))),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(20.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(t.colors.foreground)
                            .child(format!("Triggers of {}", editor.name)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(t.colors.muted_foreground)
                            .child(editor.description.split_whitespace().collect::<Vec<_>>().join(" ")),
                    ),
            ),
    );

    if editor.loading || editor.error.is_some() {
        return page.child(
            div()
                .py(px(30.))
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child(match &editor.error {
                    Some(error) => format!("Could not read the triggers of this task: {error}"),
                    None => "Loading…".to_string(),
                }),
        );
    }

    // The triggers now.
    let mut list = group("Triggers", cx);
    if editor.triggers.is_empty() {
        list = list.child(
            div()
                .pb(px(12.))
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child("No trigger. The task runs only when you start it."),
        );
    }
    for (n, trigger) in editor.triggers.iter().enumerate() {
        let target = trigger.clone();
        list = list.child(
            div()
                .py(px(8.))
                .when(n > 0, |el| el.border_t_1().border_color(rgba(0xf5f5f714)))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(15.))
                        .text_color(t.colors.foreground)
                        .child(describe(trigger)),
                )
                .child(
                    action(format!("triggers.remove.{n}"), LucideIcon::Trash, "Remove", true, cx).on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.triggers_apply(Change::Remove(target.clone()), cx)
                        }),
                    ),
                ),
        );
    }
    page = page.child(list);

    // The form of a new trigger.
    let draft = editor.draft.clone();
    let mut form = group("Add a trigger", cx);
    let kind = draft.kind;
    form = form.child(field(
        "Type",
        "",
        select("triggers.kind", kind.label(), "Interval", cx).on_click(cx.listener(
            move |this, event: &ClickEvent, window, cx| {
                let choices = Kind::ALL
                    .iter()
                    .map(|option| {
                        let option = *option;
                        Choice::new(option.label(), option == kind, move |this, cx| {
                            this.triggers_draft(|d| d.kind = option, cx)
                        })
                    })
                    .collect();
                this.open_choices(choices, event.position(), window, cx);
            },
        )),
        cx,
    ));
    if kind == Kind::Weekly {
        let day = draft.day;
        form = form.child(field(
            "Day",
            "",
            select("triggers.day", DAYS[day % 7], "Wednesday", cx).on_click(cx.listener(
                move |this, event: &ClickEvent, window, cx| {
                    let choices = DAYS
                        .iter()
                        .enumerate()
                        .map(|(at, name)| {
                            Choice::new(*name, at == day, move |this, cx| this.triggers_draft(|d| d.day = at, cx))
                        })
                        .collect();
                    this.open_choices(choices, event.position(), window, cx);
                },
            )),
            cx,
        ));
    }
    if matches!(kind, Kind::Daily | Kind::Weekly) {
        let minutes = draft.minutes;
        form = form.child(field(
            "Time",
            "The time of day on the server.",
            value_button(SharedString::from("triggers.time"), format!("{:02}:{:02}", minutes / 60, minutes % 60), cx)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.triggers_ask_time(minutes, cx))),
            cx,
        ));
    }
    if kind == Kind::Interval {
        let minutes = draft.interval_minutes;
        form = form.child(field(
            "Every",
            "",
            value_button(SharedString::from("triggers.interval"), every(minutes * TICKS_PER_MINUTE).replace("Every ", ""), cx)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.triggers_ask_interval(minutes, cx))),
            cx,
        ));
    }
    let limit = draft.limit_minutes;
    form = form.child(field(
        "Time limit",
        "The task stops when it runs longer than this.",
        value_button(SharedString::from("triggers.limit"), limit.map(span).unwrap_or_default(), cx)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.triggers_ask_limit(limit, cx))),
        cx,
    ));
    form = form.child(
        div()
            .py(px(12.))
            .flex()
            .items_center()
            .gap(px(14.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.))
                    .text_color(t.colors.foreground.opacity(0.75))
                    .child(describe(&draft.to_value())),
            )
            .child(
                button("triggers.add", "Add trigger", ButtonKind::Primary, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| this.triggers_apply(Change::Add(draft.to_value()), cx),
                )),
            ),
    );
    page.child(form)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 60 * TICKS_PER_MINUTE;

    #[test]
    fn ticks_and_time_of_day_agree() {
        assert_eq!(time_text(0), "00:00");
        assert_eq!(time_text(2 * HOUR), "02:00");
        assert_eq!(time_text(15 * HOUR + 45 * TICKS_PER_MINUTE), "15:45");
        assert_eq!(parse_time("2:30"), Some(150));
        assert_eq!(parse_time("02:30"), Some(150));
        assert_eq!(parse_time("23:59"), Some(23 * 60 + 59));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("12:60"), None);
        assert_eq!(parse_time("noon"), None);
        // The ticks of a draft read back as the same time.
        let draft = Draft { minutes: 17 * 60 + 5, ..Draft::default() };
        let ticks = draft.to_value()["TimeOfDayTicks"].as_i64().unwrap();
        assert_eq!(time_text(ticks), "17:05");
    }

    #[test]
    fn a_trigger_reads_in_plain_words() {
        let daily = json!({ "Type": "DailyTrigger", "TimeOfDayTicks": 2 * HOUR });
        assert_eq!(describe(&daily), "Every day at 02:00");
        let weekly = json!({ "Type": "WeeklyTrigger", "DayOfWeek": "Sunday", "TimeOfDayTicks": 3 * HOUR });
        assert_eq!(describe(&weekly), "On Sundays at 03:00");
        assert_eq!(describe(&json!({ "Type": "WeeklyTrigger", "DayOfWeek": 1, "TimeOfDayTicks": 0 })), "On Mondays at 00:00");
        assert_eq!(describe(&json!({ "Type": "IntervalTrigger", "IntervalTicks": 12 * HOUR })), "Every 12 hours");
        assert_eq!(describe(&json!({ "Type": "IntervalTrigger", "IntervalTicks": HOUR })), "Every hour");
        assert_eq!(describe(&json!({ "Type": "IntervalTrigger", "IntervalTicks": 24 * HOUR })), "Every 24 hours");
        assert_eq!(describe(&json!({ "Type": "IntervalTrigger", "IntervalTicks": 90 * TICKS_PER_MINUTE })), "Every 90 minutes");
        assert_eq!(describe(&json!({ "Type": "StartupTrigger" })), "When the server starts");
        let limited = json!({ "Type": "DailyTrigger", "TimeOfDayTicks": 2 * HOUR, "MaxRuntimeTicks": 90 * TICKS_PER_MINUTE });
        assert_eq!(describe(&limited), "Every day at 02:00, stops after 1 hour 30 minutes");
        assert_eq!(describe(&json!({ "Type": "DailyTrigger", "TimeOfDayTicks": 0, "MaxRuntimeTicks": 2 * HOUR })), "Every day at 00:00, stops after 2 hours");
        assert_eq!(schedule(&[]), "Manual only");
    }

    #[test]
    fn words_make_the_trigger_the_server_takes() {
        let daily = Draft::parse("daily 02:00").unwrap().to_value();
        assert_eq!(daily, json!({ "Type": "DailyTrigger", "TimeOfDayTicks": 2 * HOUR }));
        let weekly = Draft::parse("weekly sunday 03:30 limit 2").unwrap().to_value();
        assert_eq!(
            weekly,
            json!({ "Type": "WeeklyTrigger", "DayOfWeek": "Sunday", "TimeOfDayTicks": 3 * HOUR + 30 * TICKS_PER_MINUTE, "MaxRuntimeTicks": 2 * HOUR })
        );
        assert_eq!(Draft::parse("interval 12").unwrap().to_value(), json!({ "Type": "IntervalTrigger", "IntervalTicks": 12 * HOUR }));
        assert_eq!(Draft::parse("interval 1.5").unwrap().to_value()["IntervalTicks"], 90 * TICKS_PER_MINUTE);
        assert_eq!(Draft::parse("startup").unwrap().to_value(), json!({ "Type": "StartupTrigger" }));
        for bad in ["", "daily", "daily 25:00", "weekly 03:00", "weekly funday 03:00", "interval", "interval 0", "startup now", "daily 02:00 limit"] {
            assert!(Draft::parse(bad).is_err(), "{bad:?}");
        }
    }
}
