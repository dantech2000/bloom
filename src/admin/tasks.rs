// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scheduled tasks, in groups by category. A row shows the last run and the
//! schedule of a task, and its progress while it runs. A task can be started
//! and a running task can be stopped.

use crate::ui::tone::{self, State};
use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, Rgba,
    SharedString, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgb,
    rgba,
};
use serde::Deserialize;

use crate::{
    admin::{ButtonKind, ago, badge, button, panel, parse_date},
    ui::rows::rows,
    app::Bloom,
    jellyfin::Client,
    ui::{menu::MenuItem, theme::UiTheme},
    icons::icon,
};

/// The schedule column shows only when the page is at least this wide.
const SCHEDULE_MIN_W: f32 = 900.;
/// Height of a task row: the line above it, the padding and two lines of text.
const ROW_H: f32 = 67.;
/// Space above and below the texts of a row, and the line heights of the
/// name and the description (`row_height`).
const ROW_PAD: f32 = 12.;
const NAME_LINE_H: f32 = 20.;
const DESCRIPTION_LINE_H: f32 = 18.;

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
    let mut tasks: Vec<Task> = client.scheduled_tasks()?;
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
        summary = summary.child(badge(format!("{running} running"), Some(State::Ok)));
    }
    if failed > 0 {
        summary = summary.child(badge(format!("{failed} failed"), Some(State::Bad)));
    }

    let mut page = div().flex().flex_col().gap(px(18.)).child(summary);
    let content_w = app.admin_content_w();
    let compact = app.compact_w();
    let show_schedule = content_w >= SCHEDULE_MIN_W;

    // A field to find a task by its name or its description, once there
    // are enough to look for one; a category with no match is left out.
    let query = app.admin_tasks_filter.read(cx).value().trim().to_lowercase();
    if tasks().count() >= super::FIND_FROM {
        page = page.child(super::find_field(&app.admin_tasks_filter, "Find a task", compact));
    }
    let matches = |task: &Task| {
        query.is_empty()
            || task.name.to_lowercase().contains(&query)
            || task.description.to_lowercase().contains(&query)
    };
    let mut any = false;
    for (group, (category, list)) in data.groups.iter().enumerate() {
        let shown: Vec<usize> =
            list.iter().enumerate().filter(|(_, task)| matches(task)).map(|(i, _)| i).collect();
        if shown.is_empty() {
            continue;
        }
        any = true;
        // Only the rows in view are built; see `rows`. The rows of a
        // category share one height: the tallest a name and a description
        // of the category need (a plain column, where each row grows on its
        // own, cost 12 ms of layout per frame on 2026-10-23).
        let row_h = shown
            .iter()
            .map(|&i| row_height(&list[i], compact, show_schedule, content_w))
            .fold(0., f32::max);
        let list = rows(cx, shown.len(), row_h, move |this, range, cx| {
            let Some(data) = this.admin_data().and_then(|d| d.tasks.as_ref()) else {
                return Vec::new();
            };
            let content_w = this.admin_content_w();
            let compact = this.compact_w();
            let show_schedule = content_w >= SCHEDULE_MIN_W;
            // The data is read again here: a task that is gone is left out.
            let Some((_, list)) = data.groups.get(group) else {
                return Vec::new();
            };
            shown
                .get(range.clone())
                .unwrap_or_default()
                .iter()
                .zip(range)
                .filter_map(|(&i, index)| {
                    let task = list.get(i)?;
                    let lines = text_lines(task, compact, show_schedule, content_w);
                    Some(row(task, index, show_schedule, row_h, lines, cx).into_any_element())
                })
                .collect()
        });
        let title = if category.is_empty() { "Other" } else { category };
        page = page.child(panel(title.to_string(), cx).pb(px(6.)).child(list));
    }
    if !any {
        page = page.child(super::surface(div()).child(super::nothing_found("task", &query, cx)));
    }
    page
}

/// Lines of the name and of the description of a task at the width the
/// text column has, estimated from the average width of a glyph. The row
/// clamps each text to its estimate, so a text that runs longer ends in an
/// ellipsis rather than outside the row.
fn text_lines(task: &Task, compact: bool, show_schedule: bool, content_w: f32) -> (usize, usize) {
    // The panel's padding, the status and schedule columns, the buttons,
    // and the gaps between them (`row`).
    let fixed = if compact {
        40. + 118. + 10.
    } else {
        40. + 210. + if show_schedule { 200. + 20. } else { 0. } + 118. + 40.
    };
    let width = (content_w - fixed).max(120.);
    let lines = |text: &str, glyph_w: f32, max: usize| {
        let per_line = (width / glyph_w).max(8.) as usize;
        text.chars().count().div_ceil(per_line).clamp(1, max)
    };
    // Two lines of the description on the Mac, the rest under the pointer;
    // three on a phone, which has no pointer. A name wraps on a phone only.
    (
        lines(&task.name, 8.2, if compact { 2 } else { 1 }),
        lines(&one_line(&task.description), 6.6, if compact { 3 } else { 2 }),
    )
}

/// Height of a row that holds the texts of this task.
fn row_height(task: &Task, compact: bool, show_schedule: bool, content_w: f32) -> f32 {
    let (name_lines, description_lines) = text_lines(task, compact, show_schedule, content_w);
    let mut text_h = name_lines as f32 * NAME_LINE_H + 2. + description_lines as f32 * DESCRIPTION_LINE_H;
    if compact {
        // The state under the texts: one line, two with the run time or the
        // progress bar.
        let busy = task.state != "Idle";
        text_h += 4. + if busy || task.last_execution_result.is_some() { 38. } else { 20. };
    }
    (text_h + 2. * ROW_PAD).max(ROW_H)
}

/// One task of a category.
fn row(
    task: &Task,
    index: usize,
    show_schedule: bool,
    row_h: f32,
    (name_lines, description_lines): (usize, usize),
    cx: &mut Context<Bloom>,
) -> Div {
    // The colours alone: a row is built for every frame of a scroll.
    let colors = UiTheme::read(cx).colors;
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
                    // A task that works now is the live state: green.
                    .text_color(State::Ok.color())
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
                    .bg(tone::HOVER)
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .w(gpui_kit::relative(progress))
                            .bg(State::Ok.color()),
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
                        .text_color(colors.foreground.opacity(0.9))
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
                        .text_color(colors.muted_foreground)
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
                .text_color(colors.muted_foreground)
                .child("Never run"),
        }
    };

    let schedule = super::triggers::schedule(&task.triggers);

    // Stop for a task that runs, Run for one that does not.
    let id = task.id.clone();
    let (verb, label, kind, glyph, ink, done) = if busy {
        ("stop", "Stop", ButtonKind::Danger, LucideIcon::Square, rgb(0xffffff), "Task stopped")
    } else {
        ("run", "Run", ButtonKind::Plain, LucideIcon::Play, colors.foreground, "Task started")
    };
    let action = button(SharedString::from(format!("admin.tasks.{verb}.{}", task.id)), label, kind, cx)
        .child(icon(glyph, 13., ink))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            let id = id.clone();
            this.admin_action(done, cx, move |client| {
                if busy { client.stop_task(&id) } else { client.start_task(&id) }
            })
        }));

    // A phone: the state goes under the name, and the row grows with it.
    let compact = crate::views::shell::compact_now();
    let mut status = Some(status);
    div()
            .h(px(row_h))
            .py(px(ROW_PAD))
            .flex()
            .items_center()
            .gap(px(if compact { 10. } else { 20. }))
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
                            .line_clamp(name_lines)
                            .text_ellipsis()
                            .text_size(px(15.))
                            .line_height(px(NAME_LINE_H))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(colors.foreground)
                            .child(task.name.clone()),
                    )
                    .child({
                        // The rest of a long description is under the pointer.
                        let description = one_line(&task.description);
                        div()
                            .id(SharedString::from(format!("admin.tasks.description.{}", task.id)))
                            .line_clamp(description_lines)
                            .text_ellipsis()
                            .text_size(px(13.))
                            .line_height(px(DESCRIPTION_LINE_H))
                            .text_color(colors.muted_foreground)
                            .when(!compact && description.len() > 110, |el| {
                                el.tooltip(crate::ui::tip::tip(description.clone()))
                            })
                            .child(description)
                    })
                    .when(compact, |el| el.child(div().pt(px(4.)).children(status.take()))),
            )
            .when(!compact, |el| el.child(div().w(px(210.)).flex_shrink_0().children(status.take())))
            .when(show_schedule, |el| {
                el.child(
                    div()
                        .w(px(200.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(13.))
                        .text_color(colors.foreground.opacity(0.7))
                        .child(icon(LucideIcon::Clock, 14., colors.muted_foreground))
                        .child(div().min_w_0().truncate().child(schedule)),
                )
            })
            .child({
                // Run or Stop, and a "…" menu with the schedule of the task
                // (an unlabelled clock button before, 2026-10-23).
                let (id, name, description) = (task.id.clone(), task.name.clone(), task.description.clone());
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        super::more_button(SharedString::from(format!("admin.tasks.more.{}", task.id)), cx)
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                let handle = cx.weak_entity();
                                let (id, name, description) = (id.clone(), name.clone(), description.clone());
                                let items = vec![
                                    MenuItem::new("admin.tasks.menu.schedule", "Edit schedule…")
                                        .icon(LucideIcon::Clock)
                                        .detail("When this task runs")
                                        .on_click(move |_, _, cx| {
                                            let (id, name, description) = (id.clone(), name.clone(), description.clone());
                                            handle
                                                .update(cx, |this, cx| this.triggers_open(&id, &name, &description, cx))
                                                .ok();
                                        }),
                                ];
                                this.open_admin_menu(items, event.position(), window, cx);
                            })),
                    )
                    .child(div().when(!compact, |el| el.w(px(92.))).flex_shrink_0().flex().justify_end().child(action))
            })
}
