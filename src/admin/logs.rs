// SPDX-License-Identifier: AGPL-3.0-or-later
//! Logs: the log files of the server in three groups (the server's own
//! log first, then the plugins', then the media conversions'), and the
//! last lines of the one that is open. The Mac shows the list beside the
//! viewer; a phone shows the list, and a file opens as a page of its own.

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, Rgba,
    SharedString, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px,
    rgb, rgba,
};
use serde::Deserialize;

use crate::{
    admin::{ago, badge, panel, rows::rows, users::icon_disc},
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
/// Width of the list of files beside the viewer on the Mac.
const LIST_W: f32 = 340.;
/// The list and the viewer sit side by side from this content width; a
/// narrower window stacks them.
const TWO_COLUMNS_W: f32 = 820.;

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
    /// Whether any line has a time and a level: the viewer keeps a column
    /// for them then. An FFmpeg log has none.
    pub timed: bool,
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
    let timed = lines.iter().any(|line| !line.time.is_empty());
    Ok(Data {
        files,
        open,
        lines,
        total_lines,
        timed,
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
            // (the server), "[2026-10-23 00:21:52] [INFO] message" (a plugin).
            let (time, rest) = match raw.strip_prefix('[').and_then(|r| r.split_once("] ")) {
                Some((stamp, rest)) if stamp.len() >= 19 && stamp.is_char_boundary(19) => {
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
                ("[ERROR] ", Level::Error),
                ("[FATAL] ", Level::Error),
                ("[WARN] ", Level::Warning),
                ("[WARNING] ", Level::Warning),
                ("[INFO] ", Level::Info),
                ("[DEBUG] ", Level::Debug),
                ("[TRACE] ", Level::Debug),
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

/// One line of the log viewer. `timed` keeps the column of the time and
/// the level, which a line that continues an entry leaves empty.
fn log_line(line: &Line, timed: bool) -> Div {
    let (tag, tag_color, text_color) = level_style(line.level);
    div()
        .h(px(LINE_H))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(10.))
        .when(line.level == Level::Error, |el| el.bg(rgba(0xef535024)))
        .when(line.level == Level::Warning, |el| el.bg(rgba(0xffb30014)))
        .when(timed, |el| {
            el.child(
                div()
                    .w(px(96.))
                    .flex_shrink_0()
                    .text_color(tag_color)
                    .when(!line.time.is_empty(), |el| el.child(format!("{} {tag}", line.time))),
            )
        })
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

/// The group of a log file, from its name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Server,
    Plugin,
    Ffmpeg,
}

impl Kind {
    fn of(name: &str) -> Self {
        if name.starts_with("log_") {
            Self::Server
        } else if name.starts_with("FFmpeg") {
            Self::Ffmpeg
        } else {
            Self::Plugin
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Server => "Server",
            Self::Plugin => "Plugins",
            Self::Ffmpeg => "Media conversion",
        }
    }

    fn glyph(self) -> LucideIcon {
        match self {
            Self::Server => LucideIcon::Server,
            Self::Plugin => LucideIcon::Boxes,
            Self::Ffmpeg => LucideIcon::Clapperboard,
        }
    }
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let compact = app.compact_w();
    // The file the user chose, as opposed to the one the page opened by
    // itself; on a phone it is a page of its own.
    let chosen = app.admin_data().and_then(|d| d.log_open.clone());
    if compact {
        return if chosen.is_some() && chosen == data.open {
            viewer(data, true, cx)
        } else {
            file_list(app, data, true, cx)
        };
    }

    let two_columns = app.admin_content_w() >= TWO_COLUMNS_W;
    let list = file_list(app, data, false, cx);
    let viewer = data
        .open
        .clone()
        .map(|name| panel(name, cx).child(viewer(data, false, cx)));
    if two_columns {
        div()
            .flex()
            .items_start()
            .gap(px(20.))
            .child(list.w(px(LIST_W)).flex_shrink_0())
            .child(div().flex_1().min_w_0().children(viewer))
    } else {
        div().flex().flex_col().gap(px(20.)).children(viewer).child(list)
    }
}

/// The files in their groups; each group is a card of rows. A field finds
/// one by its name once there are enough to look for one.
fn file_list(app: &Bloom, data: &Data, compact: bool, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let mut groups = div().flex().flex_col().gap(px(if compact { 14. } else { 20. }));
    if data.files.is_empty() {
        return groups.child(
            super::card(cx)
                .text_size(px(15.))
                .text_color(t.colors.foreground.opacity(0.6))
                .child("The server has no log files."),
        );
    }
    let query = app.admin_logs_filter.read(cx).value().trim().to_lowercase();
    if data.files.len() >= super::FIND_FROM {
        groups = groups.child(super::find_field(&app.admin_logs_filter, "Find a log file", compact));
    }
    let mut any = false;
    for kind in [Kind::Server, Kind::Plugin, Kind::Ffmpeg] {
        let files: Vec<&LogFile> = data
            .files
            .iter()
            .filter(|f| Kind::of(&f.name) == kind)
            .filter(|f| query.is_empty() || f.name.to_lowercase().contains(&query))
            .collect();
        if files.is_empty() {
            continue;
        }
        any = true;
        let mut rows = div().flex().flex_col().child(
            div()
                .px(px(16.))
                .pt(px(14.))
                .pb(px(6.))
                .text_size(px(12.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(t.colors.foreground.opacity(0.6))
                .child(format!("{} ({})", kind.label(), files.len())),
        );
        for file in files {
            let open = !compact && data.open.as_deref() == Some(file.name.as_str());
            rows = rows.child(file_row(file, kind, open, compact, &t, cx));
        }
        groups = groups.child(super::surface(div()).overflow_hidden().child(rows));
    }
    if !any {
        groups = groups.child(super::surface(div()).child(super::nothing_found("log file", &query, cx)));
    }
    groups
}

/// The title of a media conversion's log, from its name: the kind of
/// stream and the time ("Direct stream \u{b7} 21 Oct 2026 21:05" for
/// "FFmpeg.DirectStream-2026-10-21_21-05-05_<hash>.log"). None for a
/// name of another shape.
fn ffmpeg_title(name: &str) -> Option<String> {
    let rest = name.strip_prefix("FFmpeg.")?;
    let (kind, rest) = rest.split_once('-')?;
    let kind = match kind {
        "DirectStream" => "Direct stream",
        "Transcode" => "Transcode",
        "Remux" => "Remux",
        other => other,
    };
    // "2026-10-21_21-05-05"
    let stamp = rest.get(..19)?;
    let (date, time) = stamp.split_once('_')?;
    let mut date = date.split('-');
    let (year, month, day) = (date.next()?, date.next()?, date.next()?);
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .get(month.parse::<usize>().ok()?.checked_sub(1)?)?;
    let time = time.get(..5)?.replace('-', ":");
    Some(format!(
        "{kind} \u{b7} {} {month} {year} {time}",
        day.trim_start_matches('0')
    ))
}

/// A row of the list: the glyph of the group on a disc, the name, the
/// size and the age. A media conversion's row has the kind and the time
/// as its title and the whole name in the detail, which wraps; the other
/// names are the title (a long one wraps). On the Mac the open file is
/// lit; on a phone a chevron says the row opens a page.
fn file_row(
    file: &LogFile,
    kind: Kind,
    open: bool,
    compact: bool,
    t: &UiTheme,
    cx: &mut Context<Bloom>,
) -> gpui_kit::Stateful<Div> {
    let name = file.name.clone();
    let title = match kind {
        Kind::Ffmpeg => ffmpeg_title(&file.name),
        _ => None,
    };
    let mut detail = Vec::new();
    if title.is_some() {
        detail.push(file.name.clone());
    }
    detail.push(file_size(file.size));
    detail.push(ago(&file.date_modified));
    let detail = detail.join(" \u{b7} ");
    div()
        .id(SharedString::from(format!("admin.log.{}", file.name)))
        .role(gpui_kit::Role::Button)
        .aria_label(file.name.clone())
        .min_h(px(if compact { 56. } else { 50. }))
        .px(px(16.))
        .py(px(8.))
        .flex()
        .items_center()
        .gap(px(14.))
        .cursor_pointer()
        .when(open, |el| el.bg(rgba(0xffffff14)))
        .hover(|s| s.bg(rgba(0xf5f5f714)))
        .child(icon_disc(kind.glyph(), t.colors.foreground, if compact { 36. } else { 32. }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        // 15 px on the phone too: a name is an identifier,
                        // and at 17 px "JellyfinEnhanced_2026-10-23.log"
                        // wrapped its last letter.
                        .text_size(px(15.))
                        .line_height(px(20.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(t.colors.foreground)
                        .child(title.unwrap_or_else(|| file.name.clone())),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .text_color(t.colors.foreground.opacity(0.6))
                        .child(detail),
                ),
        )
        .when(compact, |el| {
            el.child(icon(LucideIcon::ChevronRight, 16., t.colors.foreground.opacity(0.35)))
        })
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if let Page::Admin(data) = &mut this.page {
                data.log_open = Some(name.clone());
            }
            this.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            this.load_page(cx);
        }))
}

/// The last lines of the open file under a line with the count and the
/// warnings and errors among them. On a phone the lines take the page's
/// width, with no card around them.
fn viewer(data: &Data, compact: bool, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let warnings = data.lines.iter().filter(|l| l.level == Level::Warning).count();
    let errors = data.lines.iter().filter(|l| l.level == Level::Error).count();
    let mut lines = div()
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
                .map(|line| log_line(line, data.timed).into_any_element())
                .collect()
        }));
    if data.lines.is_empty() {
        lines = lines.child(
            div()
                .px(px(14.))
                .text_color(t.colors.muted_foreground)
                .child("The log is empty."),
        );
    }

    let mut heading = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(10.))
        .text_size(px(14.))
        .text_color(t.colors.muted_foreground)
        .child(format!("Last {} of {} lines", data.lines.len(), data.total_lines));
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
        .gap(px(12.))
        .when(compact, |el| el.pt(px(4.)))
        .child(heading)
        .child(lines)
}
