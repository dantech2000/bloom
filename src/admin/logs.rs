// SPDX-License-Identifier: AGPL-3.0-or-later
//! Logs: the log files of the server in three groups (the server's own
//! log first, then the plugins', then the media conversions'), and the
//! last lines of the one that is open. The Mac shows the list beside the
//! viewer; a phone shows the list, and a file opens as a page of its own.
//! The Follow switch reads the lines the server adds every few seconds
//! while the viewer is on show (`Bloom::start_log_follow`) and keeps the
//! viewer at its end; on the Mac the lines are then a box of their own.

use crate::ui::tone::{self, State};
use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, Rgba,
    SharedString, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px,
    rgb, rgba,
};
use serde::Deserialize;

use crate::{
    admin::{ago, badge, panel, users::icon_disc},
    ui::rows::rows,
    app::{Bloom, Page},
    jellyfin::Client,
    ui::theme::UiTheme,
    icons::icon,
};

/// Lines of the log that the page keeps and shows.
const TAIL_LINES: usize = 300;
/// Lines the viewer keeps while it follows the log.
const FOLLOW_LINES: usize = 1000;
/// Time between two reads of a log that is followed.
const FOLLOW_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2500);
/// The most of a log file that is read, from its end. A log of a busy day
/// at the debug level is hundreds of megabytes; the page shows its last
/// lines.
const READ_BYTES: usize = 16 * 1024 * 1024;
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
    /// Lines in the whole file; in the part that was read when `cut`.
    pub total_lines: usize,
    /// The file is larger than [`READ_BYTES`]: its end alone was read.
    pub cut: bool,
    /// Whether any line has a time and a level: the viewer keeps a column
    /// for them then. An FFmpeg log has none.
    pub timed: bool,
    /// The byte after the last whole line read: a follow reads from there.
    pub end: u64,
}

/// `wanted` is the file the user chose; without one, the page opens the
/// newest server log.
pub fn load(client: &Client, wanted: Option<&str>) -> Result<Data> {
    let mut files: Vec<LogFile> = client.log_files()?;
    files.sort_by(|a, b| b.date_modified.cmp(&a.date_modified));
    // The server log is the one to read; plugins and FFmpeg write others.
    let open = files
        .iter()
        .find(|f| Some(f.name.as_str()) == wanted)
        .or(files.iter().find(|f| f.name.starts_with("log_")))
        .or(files.first())
        .map(|f| f.name.clone());
    let (lines, total_lines, cut, end) = match &open {
        Some(name) => {
            // Whole lines, and where they end, for a follow.
            let growth = client.log_since(name, 0, READ_BYTES)?;
            let (lines, total) = tail(&growth.text);
            (lines, total, growth.cut, growth.end)
        }
        None => (Vec::new(), 0, false, 0),
    };
    let timed = lines.iter().any(|line| !line.time.is_empty());
    Ok(Data {
        files,
        open,
        lines,
        total_lines,
        cut,
        timed,
        end,
    })
}

impl Data {
    /// Adds the lines of a follow; the viewer keeps the last
    /// [`FOLLOW_LINES`]. A file that started again replaces the lines.
    /// True when lines came.
    pub fn append(&mut self, growth: bloom_core::log_follow::LogGrowth) -> bool {
        self.end = growth.end;
        if growth.restarted {
            let (lines, total) = tail(&growth.text);
            self.lines = lines;
            self.total_lines = total;
            self.cut = growth.cut;
            self.timed = self.lines.iter().any(|line| !line.time.is_empty());
            return true;
        }
        if growth.text.is_empty() {
            return false;
        }
        let level = self.lines.last().map_or(Level::Info, |line| line.level);
        let all: Vec<&str> = growth.text.lines().collect();
        self.total_lines += all.len();
        self.cut |= growth.cut;
        self.lines.extend(parse(&all, level));
        let over = self.lines.len().saturating_sub(FOLLOW_LINES);
        self.lines.drain(..over);
        self.timed |= self.lines.iter().any(|line| !line.time.is_empty());
        true
    }
}

/// The last lines of a log, with the level of each, and the line count.
fn tail(text: &str) -> (Vec<Line>, usize) {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(TAIL_LINES);
    (parse(&all[start..], Level::Info), all.len())
}

/// Lines with their time and level; a line without a level continues
/// the entry above it, and `level` is the level of the line before.
fn parse(raw_lines: &[&str], mut level: Level) -> Vec<Line> {
    raw_lines
        .iter()
        .map(|raw| {
            // "[2026-10-03 03:43:28.781 -07:00] [INF] [105] Source: message"
            // (the server), "[2026-10-23 00:21:52] [INFO] message" (a plugin).
            // A bracket that is not a date stays in the text: FFmpeg starts
            // its lines with "[libx264 @ 0x5581e0a3b2c0]".
            let stamped = raw
                .strip_prefix('[')
                .and_then(|r| r.split_once("] "))
                .and_then(|(stamp, rest)| Some((stamp_time(stamp)?, rest)));
            let (time, rest) = match stamped {
                Some((time, rest)) => (time.to_string(), rest),
                None => (String::new(), *raw),
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
        .collect()
}

/// The time of day in the stamp of a log line ("2026-10-03 03:43:28.781
/// -07:00" or "2026-10-23 00:21:52"). None for a text of another shape.
fn stamp_time(stamp: &str) -> Option<&str> {
    let bytes = stamp.as_bytes();
    let shape = bytes.len() >= 19
        && bytes[..19].iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            10 => *b == b' ' || *b == b'T',
            13 | 16 => *b == b':',
            _ => b.is_ascii_digit(),
        });
    if shape { stamp.get(11..19) } else { None }
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
        // A level that needs no look has the ink, not a colour of its own.
        Level::Info => ("INF", tone::INK_60, tone::INK_80),
        Level::Debug => ("DBG", tone::INK_40, tone::INK_50),
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
            viewer(app, data, true, cx)
        } else {
            file_list(app, data, true, cx)
        };
    }

    let two_columns = app.admin_content_w() >= TWO_COLUMNS_W;
    let list = file_list(app, data, false, cx);
    let viewer = data
        .open
        .clone()
        .map(|name| panel(name, cx).child(viewer(app, data, false, cx)));
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
    detail.push(crate::downloads::format_bytes(file.size));
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
        .child(icon_disc(kind.glyph(), None, if compact { 36. } else { 32. }))
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
            if let Page::Admin(data) = &mut this.nav.page {
                data.log_open = Some(name.clone());
            }
            this.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            this.load_page(cx);
        }))
}

/// The last lines of the open file under a line with the count and the
/// warnings and errors among them. On a phone the lines take the page's
/// width, with no card around them.
fn viewer(app: &Bloom, data: &Data, compact: bool, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let (follow, scroll) = app
        .admin_data()
        .map_or((false, None), |d| (d.log_follow, Some(d.log_scroll.clone())));
    let warnings = data.lines.iter().filter(|l| l.level == Level::Warning).count();
    let errors = data.lines.iter().filter(|l| l.level == Level::Error).count();
    let lines = div()
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
            data.lines
                .get(range)
                .unwrap_or_default()
                .iter()
                .map(|line| log_line(line, data.timed).into_any_element())
                .collect()
        }));
    // Following on the Mac: the lines are a box of their own that stays at
    // its end, beside the list of files. On a phone the page is the box.
    let lines = match scroll.filter(|_| follow && !compact) {
        Some(scroll) => div()
            .id("admin.log.follow")
            .h(px((app.viewport_h - 300.).max(260.)))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .rounded(px(12.))
            .child(lines)
            .into_any_element(),
        None => lines.into_any_element(),
    };
    let mut lines = div().child(lines);
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
        .child(if data.cut {
            // The start of a large file was not read: its lines are not counted.
            format!("Last {} lines of a large log (its last {} MB were read)", data.lines.len(), READ_BYTES >> 20)
        } else {
            format!("Last {} of {} lines", data.lines.len(), data.total_lines)
        })
        .child(follow_switch(follow, cx));
    if warnings > 0 {
        heading = heading.child(badge(
            format!("{warnings} {}", if warnings == 1 { "warning" } else { "warnings" }),
            Some(State::Warn),
        ));
    }
    if errors > 0 {
        heading = heading.child(badge(
            format!("{errors} {}", if errors == 1 { "error" } else { "errors" }),
            Some(State::Bad),
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

/// The Follow switch: a quiet pill; on, a green dot says the view is live.
fn follow_switch(on: bool, cx: &mut Context<Bloom>) -> gpui_kit::Stateful<Div> {
    let t = UiTheme::read(cx).clone();
    div()
        .id("admin.log.followswitch")
        .role(gpui_kit::Role::CheckBox)
        .aria_label("Follow")
        .aria_toggled(if on { gpui_kit::accesskit::Toggled::True } else { gpui_kit::accesskit::Toggled::False })
        .h(px(30.))
        .px(px(12.))
        .rounded_full()
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .bg(if on { rgba(0xffffff24) } else { rgba(0xffffff14) })
        .hover(|s| s.bg(rgba(0xffffff2e)))
        .text_size(px(13.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(t.colors.foreground)
        .tooltip(crate::ui::tip::tip("Read the new lines every few seconds and stay at the end"))
        .child(if on {
            div().size(px(8.)).rounded_full().bg(State::Ok.color())
        } else {
            div().size(px(8.)).rounded_full().border_1().border_color(tone::INK_40)
        })
        .child(if on { "Following" } else { "Follow" })
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            let on = this.admin_data().is_some_and(|d| d.log_follow);
            this.set_log_follow(!on, cx);
        }))
}

impl Bloom {
    /// Turns the follow of the open log on or off.
    pub(crate) fn set_log_follow(&mut self, on: bool, cx: &mut Context<Self>) {
        let compact = self.compact_w();
        let Some(data) = self.admin_data_mut() else {
            return;
        };
        data.log_follow = on;
        if on {
            data.log_scroll.scroll_to_bottom();
            if compact {
                self.nav.scroll.scroll_to_bottom();
            }
            self.start_log_follow(cx);
        }
        cx.notify();
    }

    /// The timer of the follow. It reads only while the log viewer is on
    /// show, and ends when the dashboard is left or the switch is off.
    pub(crate) fn start_log_follow(&mut self, cx: &mut Context<Self>) {
        if self.admin_live.follow.is_some() {
            return;
        }
        self.admin_live.follow = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FOLLOW_INTERVAL).await;
                let go = this.update(cx, |this, cx| {
                    let go = this.follow_log(cx);
                    if !go {
                        this.admin_live.follow = None;
                    }
                    go
                });
                if !matches!(go, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// One read of the followed log. False when the follow ends.
    fn follow_log(&mut self, cx: &mut Context<Self>) -> bool {
        let compact = self.compact_w();
        let Page::Admin(data) = &self.nav.page else {
            return false;
        };
        if data.section != super::Section::Logs || !data.log_follow {
            return false;
        }
        let Some(logs) = &data.logs else {
            return true;
        };
        let Some(name) = logs.open.clone() else {
            return true;
        };
        // On a phone the viewer is a page of its own: the list reads nothing.
        let shown = !compact || data.log_open.as_ref() == Some(&name);
        if !shown
            || data.loading
            || self.admin_live.follow_busy
            || crate::connection::is_offline()
            || self.session.is_none()
            || self.playback.playing.is_some()
        {
            return true;
        }
        let (end, generation) = (logs.end, self.nav.generation);
        self.admin_live.follow_busy = true;
        self.fetch(
            cx,
            move |client| client.log_follow(&name, end, READ_BYTES),
            move |this, result, cx| {
                this.admin_live.follow_busy = false;
                if this.nav.generation != generation {
                    return;
                }
                let Page::Admin(data) = &mut this.nav.page else {
                    return;
                };
                let Some(logs) = data.logs.as_mut() else {
                    return;
                };
                // The answer is for the file and the part on show.
                if logs.end != end {
                    return;
                }
                if let Ok(Some(growth)) = result
                    && logs.append(growth)
                {
                    data.log_scroll.scroll_to_bottom();
                    if this.compact_w() {
                        this.nav.scroll.scroll_to_bottom();
                    }
                    cx.notify();
                }
            },
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a date is the stamp of a line. Before, any bracket of 19 bytes
    /// or more was one: an FFmpeg line showed "0x5581e0" as its time and
    /// lost its prefix, and a bracket with a letter of two bytes across
    /// byte 11 was a panic.
    #[test]
    fn only_a_date_is_the_time_of_a_line() {
        let text = "[2026-10-03 03:43:28.781 -07:00] [INF] [105] Main: started\n\
                    [2026-10-23 00:21:52] [WARN] slow\n\
                    [libx264 @ 0x5581e0a3b2c0] using SAR=1/1\n\
                    [1234567890\u{e9}abcdefghijk] text\n\
                    plain";
        let (lines, total) = tail(text);
        assert_eq!(total, 5);
        assert_eq!((lines[0].time.as_str(), lines[0].text.as_str()), ("03:43:28", "[105] Main: started"));
        assert!(lines[0].level == Level::Info);
        assert_eq!((lines[1].time.as_str(), lines[1].text.as_str()), ("00:21:52", "slow"));
        assert!(lines[1].level == Level::Warning);
        assert_eq!((lines[2].time.as_str(), lines[2].text.as_str()), ("", "[libx264 @ 0x5581e0a3b2c0] using SAR=1/1"));
        assert_eq!((lines[3].time.as_str(), lines[3].text.as_str()), ("", "[1234567890\u{e9}abcdefghijk] text"));
        assert_eq!(lines[4].text, "plain");
    }

    fn data(text: &str) -> Data {
        let (lines, total_lines) = tail(text);
        Data { files: Vec::new(), open: None, lines, total_lines, cut: false, timed: true, end: text.len() as u64 }
    }

    /// The lines of a follow come after the ones on show; a line without a
    /// level keeps the level of the line before it, from the last read.
    #[test]
    fn a_follow_appends_its_lines() {
        use bloom_core::log_follow::LogGrowth;
        let mut log = data("[2026-10-03 03:43:28.781 -07:00] [ERR] [1] Boom\n");
        assert!(!log.append(LogGrowth::nothing(48)));
        let came = log.append(LogGrowth {
            text: "   at Frame.Line()\n[2026-10-03 03:43:29.001 -07:00] [INF] [1] Fine\n".into(),
            end: 120,
            ..LogGrowth::default()
        });
        assert!(came);
        assert_eq!(log.end, 120);
        assert_eq!(log.total_lines, 3);
        assert!(log.lines[1].level == Level::Error && log.lines[1].time.is_empty());
        assert!(log.lines[2].level == Level::Info);
        // A file that started again replaces the lines.
        log.append(LogGrowth { text: "fresh\n".into(), end: 6, restarted: true, cut: false });
        assert_eq!((log.lines.len(), log.total_lines, log.end), (1, 1, 6));
    }

    /// A long follow keeps the last lines only.
    #[test]
    fn a_follow_keeps_the_last_lines() {
        use bloom_core::log_follow::LogGrowth;
        let mut log = data("");
        let text: String = (0..FOLLOW_LINES + 50).map(|n| format!("line {n}\n")).collect();
        log.append(LogGrowth { text, end: 1, ..LogGrowth::default() });
        assert_eq!(log.lines.len(), FOLLOW_LINES);
        assert_eq!(log.lines.last().map(|l| l.text.as_str()), Some(format!("line {}", FOLLOW_LINES + 49).as_str()));
        assert_eq!(log.total_lines, FOLLOW_LINES + 50);
    }
}
