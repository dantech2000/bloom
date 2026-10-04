// SPDX-License-Identifier: AGPL-3.0-or-later
//! Activity log of the server as a timeline, one group for each day.

use std::collections::HashMap;

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, Rgba, Styled,
    div, px, rgba,
};
use serde::Deserialize;

use super::{
    ago, badge, card, parse_date,
    rows::rows,
    users::{AMBER, BLUE, GREEN, GREY, RED, avatar, content_width, icon_disc, local, stat},
};
use crate::{app::Bloom, jellyfin::Client, ui::theme::UiTheme};

/// Entries the page asks for.
const LIMIT: usize = 100;
/// Height of a row of the timeline: two lines of text and the padding.
const ROW_H: f32 = 58.;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Entry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub short_overview: Option<String>,
    #[serde(default, rename = "Type")]
    pub kind: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub severity: String,
}

/// Kind of an entry that [`collapse`] makes from a start and a stop.
pub(super) const PLAYED: &str = "Played";

/// Joins the start and the stop of one playback into one entry, such as
/// "alex played a film on the television" with the time it took.
/// `entries` come newest first, as the server sends them. A start that has
/// no stop yet (it plays now) and a stop whose start is not in the list stay
/// as they are.
pub(super) fn collapse(entries: Vec<Entry>) -> Vec<Entry> {
    // What ties a stop to its start: the user, the item and the device. The
    // device is in the text only, after the last " on ".
    fn key(entry: &Entry) -> (Option<&str>, Option<&str>, &str) {
        let device = entry.name.rsplit_once(" on ").map_or("", |(_, device)| device);
        (entry.user_id.as_deref(), entry.item_id.as_deref(), device)
    }
    let is_start = |e: &Entry| matches!(e.kind.as_str(), "VideoPlayback" | "AudioPlayback");
    let is_stop =
        |e: &Entry| matches!(e.kind.as_str(), "VideoPlaybackStopped" | "AudioPlaybackStopped");

    let mut used = vec![false; entries.len()];
    let mut joined: Vec<Option<Entry>> = vec![None; entries.len()];
    for (index, stop) in entries.iter().enumerate() {
        if !is_stop(stop) {
            continue;
        }
        // The start is the next older entry of the same playback.
        let start = entries[index + 1..]
            .iter()
            .position(|e| (is_start(e) || is_stop(e)) && key(e) == key(stop))
            .map(|offset| index + 1 + offset)
            .filter(|&at| is_start(&entries[at]) && !used[at]);
        let Some(at) = start else {
            continue;
        };
        used[at] = true;
        let length = match (parse_date(&entries[at].date), parse_date(&stop.date)) {
            (Some(from), Some(to)) => Some(length_label(to.as_second() - from.as_second())),
            _ => None,
        };
        joined[index] = Some(Entry {
            // The server text is English here; another language keeps the
            // text of the stop.
            name: stop.name.replacen(" has finished playing ", " played ", 1),
            short_overview: length,
            kind: PLAYED.to_string(),
            ..stop.clone()
        });
    }
    entries
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !used[*index])
        .map(|(index, entry)| joined[index].take().unwrap_or(entry))
        .collect()
}

/// "40 s", "12 min", "1 h 5 min".
fn length_label(seconds: i64) -> String {
    match seconds.max(0) {
        s @ 0..60 => format!("{s} s"),
        s @ 60..3600 => format!("{} min", s / 60),
        s => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

impl Entry {
    /// Icon and colour for the kind of event; the severity wins when it is
    /// a warning or an error.
    fn look(&self) -> (LucideIcon, u32) {
        let (glyph, color) = match self.kind.as_str() {
            "VideoPlayback" | "AudioPlayback" | PLAYED => (LucideIcon::Play, GREEN),
            "VideoPlaybackStopped" | "AudioPlaybackStopped" => (LucideIcon::Square, GREY),
            "SessionStarted" => (LucideIcon::LogIn, BLUE),
            "SessionEnded" => (LucideIcon::LogOut, GREY),
            "AuthenticationSucceeded" => (LucideIcon::KeyRound, BLUE),
            "AuthenticationFailed" | "UserLockedOut" => (LucideIcon::TriangleAlert, RED),
            "UserCreated" => (LucideIcon::UserPlus, GREEN),
            "UserDeleted" => (LucideIcon::UserMinus, RED),
            "UserPasswordChanged" | "UserPolicyUpdated" => (LucideIcon::Lock, AMBER),
            kind if kind.starts_with("Plugin") || kind.starts_with("Package") => {
                (LucideIcon::Boxes, AMBER)
            }
            kind if kind.contains("Task") => (LucideIcon::Clock, GREY),
            kind if kind.contains("Subtitle") => (LucideIcon::TriangleAlert, AMBER),
            _ => (LucideIcon::Info, GREY),
        };
        match self.severity.as_str() {
            "Error" | "Critical" | "Fatal" => (LucideIcon::CircleAlert, RED),
            "Warning" | "Warn" => (LucideIcon::TriangleAlert, AMBER),
            _ => (glyph, color),
        }
    }

    /// "VideoPlaybackStopped" as "Video Playback Stopped".
    fn kind_label(&self) -> String {
        let mut label = String::new();
        for c in self.kind.chars() {
            if c.is_uppercase() && !label.is_empty() {
                label.push(' ');
            }
            label.push(c);
        }
        label
    }
}

pub struct Data {
    pub entries: Vec<Entry>,
    /// Entries the server has in all.
    pub total: usize,
    /// Names of the users, by id.
    pub users: HashMap<String, String>,
}

pub fn load(client: &Client) -> Result<Data> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Page {
        #[serde(default)]
        items: Vec<Entry>,
        #[serde(default)]
        total_record_count: usize,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Name {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
    }
    // The two requests run at the same time.
    let (page, users) = std::thread::scope(|scope| {
        let names = scope.spawn(|| client.get::<Vec<Name>>("/Users", &[]));
        let page: Result<Page> = client.get(
            "/System/ActivityLog/Entries",
            &[("Limit", LIMIT.to_string())],
        );
        (page, names.join().expect("names thread"))
    });
    let page = page?;
    // The log is still useful without the names.
    let users = users
        .unwrap_or_default()
        .into_iter()
        .map(|u| (u.id, u.name))
        .collect();
    Ok(Data {
        entries: collapse(page.items),
        total: page.total_record_count,
        users,
    })
}

pub fn render(_app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let today = jiff::Zoned::now().date();
    let yesterday = today.yesterday().ok();

    let alerts = data
        .entries
        .iter()
        .filter(|e| !matches!(e.severity.as_str(), "Information" | "Info" | "Debug" | ""))
        .count();
    let plays = data
        .entries
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "VideoPlayback" | "AudioPlayback" | PLAYED))
        .count();

    let page = div().flex().flex_col().gap(px(18.)).child(
        div()
            .flex()
            .flex_wrap()
            .gap(px(12.))
            .child(stat(LucideIcon::Activity, "Entries shown", data.entries.len(), cx))
            .child(stat(LucideIcon::Server, "In the log", data.total, cx))
            .child(stat(LucideIcon::Play, "Playbacks", plays, cx))
            .child(stat(LucideIcon::TriangleAlert, "Warnings and errors", alerts, cx)),
    );

    if data.entries.is_empty() {
        return page.child(
            card(cx)
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child("The activity log is empty."),
        );
    }

    // Entries come newest first; a new group starts when the local day changes.
    // A group holds the range of its entries and builds only the rows in
    // view; see `rows`.
    let mut day = None;
    let mut groups: Vec<(Div, usize)> = Vec::new();
    for (index, entry) in data.entries.iter().enumerate() {
        let date = local(&entry.date).map(|z| z.date());
        if groups.is_empty() || date != day {
            day = date;
            let heading = match date {
                Some(d) if d == today => "Today".to_string(),
                Some(d) if Some(d) == yesterday => "Yesterday".to_string(),
                Some(d) => d.strftime("%A, %B %-d, %Y").to_string(),
                None => "Unknown date".to_string(),
            };
            groups.push((
                card(cx).p(px(6.)).child(
                    div()
                        .px(px(12.))
                        .pt(px(10.))
                        .pb(px(6.))
                        .text_size(px(14.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground.opacity(0.6))
                        .child(heading),
                ),
                index,
            ));
        }
    }
    let ends = groups
        .iter()
        .skip(1)
        .map(|(_, start)| *start)
        .chain([data.entries.len()])
        .collect::<Vec<_>>();
    page.children(groups.into_iter().zip(ends).map(|((group, start), end)| {
        group.child(rows(cx, end - start, ROW_H, move |this, range, cx| {
            let Some(data) = this.admin_data().and_then(|d| d.activity.as_ref()) else {
                return Vec::new();
            };
            let show_kind = content_width(this) >= 760.;
            data.entries[start + range.start..start + range.end]
                .iter()
                .map(|entry| row(entry, data, show_kind, cx).into_any_element())
                .collect()
        }))
    }))
}

/// One entry of the timeline.
fn row(entry: &Entry, data: &Data, show_kind: bool, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let (glyph, color) = entry.look();
    let user = entry
        .user_id
        .as_ref()
        .and_then(|id| data.users.get(id))
        .cloned();
    let time = local(&entry.date)
        .map(|z| z.strftime("%-I:%M %p").to_string())
        .unwrap_or_default();
    let overview = entry
        .short_overview
        .clone()
        .filter(|text| !text.is_empty());

    let mut row = div()
        .h(px(ROW_H))
        .px(px(12.))
        .rounded(px(12.))
        .flex()
        .items_center()
        .gap(px(14.))
        .hover(|s| s.bg(rgba(0xffffff0a)))
        .child(
            div()
                .w(px(72.))
                .flex_shrink_0()
                .text_size(px(13.))
                .text_color(t.colors.foreground.opacity(0.55))
                .child(time),
        )
        .child(icon_disc(glyph, tint(color), 32.))
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
                        .text_color(t.colors.foreground)
                        .child(entry.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(13.))
                        .text_color(t.colors.foreground.opacity(0.55))
                        .child(match overview {
                            Some(text) => format!("{text} · {}", ago(&entry.date)),
                            None => ago(&entry.date),
                        }),
                ),
        );
    if show_kind {
        row = row.child(
            div()
                .flex_shrink_0()
                .child(badge(entry.kind_label(), Rgba { a: 0.55, ..tint(color) })),
        );
    }
    if let Some(user) = user {
        row = row.child(avatar(&user, None, 26.));
    }
    row
}

/// The grey of quiet events is too faint for an icon; lift it.
fn tint(color: u32) -> Rgba {
    if color == GREY {
        rgba(0xb8b8c0ff)
    } else {
        rgba(color)
    }
}
