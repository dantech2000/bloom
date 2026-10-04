// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scheduled tasks, in groups by category. A row shows the last run and the
//! schedule of a task, and its progress while it runs. A task can be started
//! and a running task can be stopped.

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, Rgba,
    SharedString, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgb, rgba,
};
use serde::Deserialize;

use crate::{
    admin::{ButtonKind, ago, badge, button, panel, parse_date, rows::rows},
    app::Bloom,
    jellyfin::Client,
    ui::{theme::UiTheme, tip::tip},
    views::cards::icon,
};

/// Width of the sidebar plus the page padding at both sides.
const PAGE_CHROME: f32 = 236. + 56.;
/// The schedule column shows only when the page is at least this wide.
const SCHEDULE_MIN_W: f32 = 900.;
/// Height of a task row: the line above it, the padding and two lines of text.
const ROW_H: f32 = 67.;

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Task {
    #[serde(default)]
    pub name: String,
    /// "Idle", "Running" or "Cancelling".
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub current_progress_percentage: Option<f64>,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub last_execution_result: Option<LastRun>,
    #[serde(default)]
    /// Kept as the server sent them; see `triggers`.
    pub triggers: Vec<serde_json::Value>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct LastRun {
    #[serde(default)]
    pub start_time_utc: String,
    #[serde(default)]
    pub end_time_utc: String,
    /// "Completed", "Failed", "Cancelled" or "Aborted".
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error_message: Option<String>,
}

#[derive(PartialEq)]
pub struct Data {
    /// Categories by name, each with its tasks by name.
    pub groups: Vec<(String, Vec<Task>)>,
}

pub fn load(client: &Client) -> Result<Data> {
    let mut tasks: Vec<Task> =
        client.get("/ScheduledTasks", &[("IsHidden", "false".to_string())])?;
    tasks.sort_by(|a, b| {
        (a.category.to_lowercase(), a.name.to_lowercase())
            .cmp(&(b.category.to_lowercase(), b.name.to_lowercase()))
    });
    let mut groups: Vec<(String, Vec<Task>)> = Vec::new();
    for task in tasks {
        match groups.last_mut() {
            Some((category, list)) if *category == task.category => list.push(task),
            _ => groups.push((task.category.clone(), vec![task])),
        }
    }
    Ok(Data { groups })
}

/// How long a run took: "1.6 s", "42 s", "3 min 5 s", "1 h 12 min".
fn run_time(run: &LastRun) -> Option<String> {
    let start = parse_date(&run.start_time_utc)?.as_millisecond();
    let end = parse_date(&run.end_time_utc)?.as_millisecond();
    let ms = (end - start).max(0);
    Some(match ms / 1000 {
        0 if ms < 100 => "under 0.1 s".to_string(),
        0..10 => format!("{:.1} s", ms as f64 / 1000.),
        s @ 10..60 => format!("{s} s"),
        s @ 60..3600 => format!("{} min {} s", s / 60, s % 60),
        s => format!("{} h {} min", s / 3600, s % 3600 / 60),
    })
}

/// A text as one line: some plugins write their description on several.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Colour of the dot for the result of a run.
fn result_color(status: &str) -> Rgba {
    match status {
        "Completed" => rgb(0x4caf50),
        "Failed" | "Aborted" => rgb(0xef5350),
        _ => rgb(0xffb300),
    }
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    // The trigger editor of one task takes the place of the list.
    if let Some(editor) = app.admin_data().and_then(|d| d.triggers.as_ref()) {
        return super::triggers::render(editor, cx);
    }
    let t = UiTheme::read(cx).clone();

    let tasks = || data.groups.iter().flat_map(|(_, list)| list.iter());
    let running = tasks().filter(|task| task.state != "Idle").count();
    let failed = tasks()
        .filter(|task| {
            task.last_execution_result
                .as_ref()
                .is_some_and(|run| matches!(run.status.as_str(), "Failed" | "Aborted"))
        })
        .count();

    let mut summary = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .text_size(px(15.))
        .text_color(t.colors.muted_foreground)
        .child(format!(
            "{} tasks in {} categories",
            tasks().count(),
            data.groups.len()
        ));
    if running > 0 {
        summary = summary.child(badge(format!("{running} running"), rgb(0x1565c0)));
    }
    if failed > 0 {
        summary = summary.child(badge(format!("{failed} failed"), rgb(0xc62828)));
    }

    let mut page = div().flex().flex_col().gap(px(18.)).child(summary);
    for (group, (category, list)) in data.groups.iter().enumerate() {
        // Only the rows in view are built; see `rows`.
        let list = rows(cx, list.len(), ROW_H, move |this, range, cx| {
            let Some(data) = this.admin_data().and_then(|d| d.tasks.as_ref()) else {
                return Vec::new();
            };
            let show_schedule = this.viewport_w - PAGE_CHROME >= SCHEDULE_MIN_W;
            data.groups[group].1[range.clone()]
                .iter()
                .zip(range)
                .map(|(task, index)| row(task, index, show_schedule, cx).into_any_element())
                .collect()
        });
        let title = if category.is_empty() { "Other" } else { category };
        page = page.child(panel(title.to_string(), cx).pb(px(6.)).child(list));
    }
    page
}

/// One task of a category.
fn row(task: &Task, index: usize, show_schedule: bool, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let busy = task.state != "Idle";
    let progress = (task.current_progress_percentage.unwrap_or(0.) as f32 / 100.)
        .clamp(0., 1.);

    // Last run, or the progress of the run in hand.
    let status = if busy {
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(13.))
                    .text_color(rgb(0x64b5f6))
                    .child(if task.state == "Cancelling" {
                        "Stopping…"
                    } else {
                        "Running"
                    })
                    .child(format!("{:.0}%", progress * 100.)),
            )
            .child(
                div()
                    .h(px(6.))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(rgba(0xffffff1f))
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .w(gpui_kit::relative(progress))
                            .bg(rgb(0x42a5f5)),
                    ),
            )
    } else {
        match &task.last_execution_result {
            Some(run) => div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(14.))
                        .text_color(t.colors.foreground.opacity(0.9))
                        .child(
                            div()
                                .size(px(8.))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(result_color(&run.status)),
                        )
                        .child(div().truncate().child(format!(
                            "{} {}",
                            run.status,
                            ago(&run.end_time_utc)
                        ))),
                )
                .child(
                    div()
                        .pl(px(16.))
                        .truncate()
                        .text_size(px(12.))
                        .text_color(t.colors.muted_foreground)
                        .child(
                            match run.error_message.as_deref().filter(|m| !m.is_empty())
                            {
                                Some(error) => one_line(error),
                                None => run_time(run)
                                    .map(|time| format!("took {time}"))
                                    .unwrap_or_default(),
                            },
                        ),
                ),
            None => div()
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child("Never run"),
        }
    };

    let schedule = super::triggers::schedule(&task.triggers);

    let id = task.id.clone();
    let action = if busy {
        button(
            SharedString::from(format!("admin.tasks.stop.{}", task.id)),
            "Stop",
            ButtonKind::Danger,
            cx,
        )
        .child(icon(LucideIcon::Square, 13., rgb(0xffffff)))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            let id = id.clone();
            this.admin_action("Task stopped", cx, move |client| {
                client.call("DELETE", &format!("/ScheduledTasks/Running/{id}"), &[])
            })
        }))
    } else {
        button(
            SharedString::from(format!("admin.tasks.run.{}", task.id)),
            "Run",
            ButtonKind::Plain,
            cx,
        )
        .child(icon(LucideIcon::Play, 13., t.colors.foreground))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            let id = id.clone();
            this.admin_action("Task started", cx, move |client| {
                client.call("POST", &format!("/ScheduledTasks/Running/{id}"), &[])
            })
        }))
    };

    div()
            .h(px(ROW_H))
            .flex()
            .items_center()
            .gap(px(20.))
            // The line above a row; the first row keeps the space.
            .border_t_1()
            .when(index > 0, |el| el.border_color(rgba(0xf5f5f714)))
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
                            .child(task.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(t.colors.muted_foreground)
                            .child(one_line(&task.description)),
                    ),
            )
            .child(div().w(px(210.)).flex_shrink_0().child(status))
            .when(show_schedule, |el| {
                el.child(
                    div()
                        .w(px(200.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(13.))
                        .text_color(t.colors.foreground.opacity(0.7))
                        .child(icon(LucideIcon::Clock, 14., t.colors.muted_foreground))
                        .child(div().min_w_0().truncate().child(schedule)),
                )
            })
            .child({
                // The triggers of the task, then Run or Stop.
                let (id, name, description) = (task.id.clone(), task.name.clone(), task.description.clone());
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        div()
                            .id(SharedString::from(format!("admin.tasks.triggers.{}", task.id)))
                            .size(px(38.))
                            .flex_shrink_0()
                            .rounded(px(12.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .bg(rgba(0x282828cc))
                            .hover(|s| s.bg(rgba(0xffffff2e)))
                            .tooltip(tip("When this task runs"))
                            .child(icon(LucideIcon::Clock, 16., t.colors.foreground))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.triggers_open(&id, &name, &description, cx)
                            })),
                    )
                    .child(div().w(px(92.)).flex_shrink_0().flex().justify_end().child(action))
            })
}
