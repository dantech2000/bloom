// SPDX-License-Identifier: AGPL-3.0-or-later
//! Logs: the log files of the server, newest first, and the last lines of
//! the newest server log.

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, Rgba,
    SharedString, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px,
    rgb, rgba,
};
use serde::Deserialize;

use crate::{
    admin::{ago, badge, panel, rows::rows},
    app::{Bloom, Page},
    jellyfin::Client,
    ui::theme::UiTheme,
    views::cards::icon,
};

/// Lines of the log that the page keeps and shows.
const TAIL_LINES: usize = 300;
/// A longer line is cut here; the page shows one row for each line.
const LINE_CHARS: usize = 400;
/// Height of a line of the viewer.
const LINE_H: f32 = 20.;
/// Smallest width of a file tile; the grid fits as many as the page holds.
const FILE_MIN_W: f32 = 320.;
const FILE_GAP: f32 = 10.;
/// Width of the sidebar plus the page padding at both sides, and the panel's.
const PAGE_CHROME: f32 = 236. + 56. + 38.;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LogFile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub date_modified: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
}

pub struct Line {
    pub level: Level,
    /// Time of day of the entry; empty for a line that continues an entry.
    pub time: String,
    pub text: String,
}

pub struct Data {
    pub files: Vec<LogFile>,
    /// Name of the file the lines come from.
    pub open: Option<String>,
    pub lines: Vec<Line>,
    /// Lines in the whole file.
    pub total_lines: usize,
}

/// `wanted` is the file the user chose; without one, the page opens the
/// newest server log.
pub fn load(client: &Client, wanted: Option<&str>) -> Result<Data> {
    let mut files: Vec<LogFile> = client.get("/System/Logs", &[])?;
    files.sort_by(|a, b| b.date_modified.cmp(&a.date_modified));
    // The server log is the one to read; plugins and FFmpeg write others.
    let open = files
        .iter()
        .find(|f| Some(f.name.as_str()) == wanted)
        .or(files.iter().find(|f| f.name.starts_with("log_")))
        .or(files.first())
        .map(|f| f.name.clone());
    let (lines, total_lines) = match &open {
        Some(name) => {
            let text = client.get_text("/System/Logs/Log", &[("name", name.clone())])?;
            tail(&text)
        }
        None => (Vec::new(), 0),
    };
    Ok(Data {
        files,
        open,
        lines,
        total_lines,
    })
}

/// The last lines of a log, with the level of each, and the line count.
fn tail(text: &str) -> (Vec<Line>, usize) {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(TAIL_LINES);
    let mut level = Level::Info;
    let lines = all[start..]
        .iter()
        .map(|raw| {
            // "[2026-10-03 03:43:28.781 -07:00] [INF] [105] Source: message"
            let (time, rest) = match raw.strip_prefix('[').and_then(|r| r.split_once("] ")) {
                Some((stamp, rest)) if stamp.len() > 19 && stamp.is_char_boundary(19) => {
                    (stamp[11..19].to_string(), rest)
                }
                _ => (String::new(), *raw),
            };
            let (found, rest) = [
                ("[ERR] ", Level::Error),
                ("[FTL] ", Level::Error),
                ("[WRN] ", Level::Warning),
                ("[INF] ", Level::Info),
                ("[DBG] ", Level::Debug),
                ("[VRB] ", Level::Debug),
            ]
            .iter()
            .find_map(|(tag, level)| Some((*level, rest.strip_prefix(tag)?)))
            // A line without a level continues the entry above it.
            .unwrap_or((level, rest));
            level = found;
            Line {
                level,
                time,
                text: rest.chars().take(LINE_CHARS).collect(),
            }
        })
        .collect();
    (lines, all.len())
}

/// 108985 as "106 KB".
fn file_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.0} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

/// One line of the log viewer.
fn log_line(line: &Line) -> Div {
    let (tag, tag_color, text_color) = level_style(line.level);
    div()
        .h(px(LINE_H))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(10.))
        .when(line.level == Level::Error, |el| el.bg(rgba(0xef535024)))
        .when(line.level == Level::Warning, |el| el.bg(rgba(0xffb30014)))
        .child(
            div()
                .w(px(96.))
                .flex_shrink_0()
                .text_color(tag_color)
                // A line that continues an entry has no time and tag.
                .when(!line.time.is_empty(), |el| {
                    el.child(format!("{} {tag}", line.time))
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(text_color)
                .child(line.text.clone()),
        )
}

/// Short tag and colours (tag, text) of a level.
fn level_style(level: Level) -> (&'static str, Rgba, Rgba) {
    match level {
        Level::Error => ("ERR", rgb(0xef5350), rgb(0xffb4ab)),
        Level::Warning => ("WRN", rgb(0xffb300), rgb(0xffe0a3)),
        Level::Info => ("INF", rgb(0x64b5f6), rgba(0xf5f5f7cc)),
        Level::Debug => ("DBG", rgba(0xf5f5f766), rgba(0xf5f5f780)),
    }
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let content_w = (app.viewport_w - PAGE_CHROME).max(FILE_MIN_W);
    let columns = (((content_w + FILE_GAP) / (FILE_MIN_W + FILE_GAP)).floor() as usize).max(1);
    let file_w = ((content_w - FILE_GAP * (columns - 1) as f32) / columns as f32).floor();

    let mut files = div().flex().flex_wrap().gap(px(FILE_GAP));
    for file in &data.files {
        let open = data.open.as_deref() == Some(file.name.as_str());
        // The server log, a media conversion, or the log of a plugin.
        let glyph = if file.name.starts_with("log_") {
            LucideIcon::Server
        } else if file.name.starts_with("FFmpeg") {
            LucideIcon::Clapperboard
        } else {
            LucideIcon::Boxes
        };
        let name = file.name.clone();
        files = files.child(
            div()
                .id(SharedString::from(format!("admin.log.{}", file.name)))
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xffffff1f)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Page::Admin(data) = &mut this.page {
                        data.log_open = Some(name.clone());
                    }
                    this.load_page(cx);
                }))
                .w(px(file_w))
                .h(px(52.))
                .px(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(if open { rgba(0xf5f5f766) } else { rgba(0xf5f5f714) })
                .bg(if open { rgba(0xffffff1f) } else { rgba(0xffffff08) })
                .flex()
                .items_center()
                .gap(px(12.))
                .child(icon(glyph, 18., t.colors.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_ellipsis_middle()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_size(px(14.))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(t.colors.foreground)
                                .child(file.name.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(t.colors.muted_foreground)
                                .child(format!(
                                    "{} · {}",
                                    file_size(file.size),
                                    ago(&file.date_modified)
                                )),
                        ),
                ),
        );
    }

    let warnings = data.lines.iter().filter(|l| l.level == Level::Warning).count();
    let errors = data.lines.iter().filter(|l| l.level == Level::Error).count();
    let mut viewer = div()
        .rounded(px(12.))
        .bg(rgba(0x00000066))
        .py(px(10.))
        .flex()
        .flex_col()
        .font_family("Menlo")
        .text_size(px(12.))
        // Only the lines in view are built; see `rows`.
        .child(rows(cx, data.lines.len(), LINE_H, |this, range, _| {
            let Some(data) = this.admin_data().and_then(|d| d.logs.as_ref()) else {
                return Vec::new();
            };
            data.lines[range]
                .iter()
                .map(|line| log_line(line).into_any_element())
                .collect()
        }));
    if data.lines.is_empty() {
        viewer = viewer.child(
            div()
                .px(px(14.))
                .text_color(t.colors.muted_foreground)
                .child("The log is empty."),
        );
    }

    let mut heading = div()
        .mb(px(12.))
        .flex()
        .items_center()
        .gap(px(10.))
        .text_size(px(14.))
        .text_color(t.colors.muted_foreground)
        .child(format!(
            "Last {} of {} lines",
            data.lines.len(),
            data.total_lines
        ));
    if warnings > 0 {
        heading = heading.child(badge(
            format!("{warnings} {}", if warnings == 1 { "warning" } else { "warnings" }),
            rgb(0xb26a00),
        ));
    }
    if errors > 0 {
        heading = heading.child(badge(
            format!("{errors} {}", if errors == 1 { "error" } else { "errors" }),
            rgb(0xc62828),
        ));
    }

    div()
        .flex()
        .flex_col()
        .gap(px(20.))
        .child(panel(format!("Log Files ({})", data.files.len()), cx).child(files))
        .when_some(data.open.clone(), |el, name| {
            el.child(panel(name, cx).child(heading).child(viewer))
        })
}
