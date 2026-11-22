// SPDX-License-Identifier: AGPL-3.0-or-later
//! Activity log of the server as a timeline, one group for each day. The
//! server filters, sorts and pages it (Jellyfin 12.0): the kind of event,
//! the level, the user, how far back, newest or oldest first
//! (`bloom_core::activity_query`); the page holds a hundred entries.

use crate::ui::rows::rows;
use crate::ui::tone::State;
use std::collections::HashMap;
use std::rc::Rc;

use bloom_core::activity_query::{Kind, Level, Period, Query, Who};

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled, div, px, rgba,
};
use serde::Deserialize;

use super::{
    ago, badge, card, parse_date,
    users::{avatar, content_width, icon_disc, local, stat},
};
use crate::{app::Bloom, jellyfin::Client, ui::theme::UiTheme};

/// Height of a row of the timeline: two lines of text and the padding.
const ROW_H: f32 = 58.;

#[derive(Clone, Default, Deserialize, PartialEq)]
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
    /// Icon and state for the kind of event; the severity wins when it is
    /// a warning or an error.
    pub(super) fn look(&self) -> (LucideIcon, Option<State>) {
        let (glyph, state) = match self.kind.as_str() {
            "VideoPlayback" | "AudioPlayback" | PLAYED => (LucideIcon::Play, Some(State::Ok)),
            "VideoPlaybackStopped" | "AudioPlaybackStopped" => (LucideIcon::Square, None),
            "SessionStarted" => (LucideIcon::LogIn, None),
            "SessionEnded" => (LucideIcon::LogOut, None),
            "AuthenticationSucceeded" => (LucideIcon::KeyRound, None),
            "AuthenticationFailed" | "UserLockedOut" => (LucideIcon::TriangleAlert, Some(State::Bad)),
            // A kind of event has an icon; a colour is for a state
            // (playback that runs, a sign-in that failed).
            "UserCreated" => (LucideIcon::UserPlus, None),
            "UserDeleted" => (LucideIcon::UserMinus, None),
            "UserPasswordChanged" | "UserPolicyUpdated" => (LucideIcon::Lock, None),
            kind if kind.starts_with("Plugin") || kind.starts_with("Package") => {
                (LucideIcon::Boxes, None)
            }
            kind if kind.contains("Task") => (LucideIcon::Clock, None),
            kind if kind.contains("Subtitle") => (LucideIcon::TriangleAlert, Some(State::Warn)),
            _ => (LucideIcon::Info, None),
        };
        match self.severity.as_str() {
            "Error" | "Critical" | "Fatal" => (LucideIcon::CircleAlert, Some(State::Bad)),
            "Warning" | "Warn" => (LucideIcon::TriangleAlert, Some(State::Warn)),
            _ => (glyph, state),
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
    /// Entries the server has in all that match the filters.
    pub total: usize,
    /// The page these entries are (its filters, sort and start).
    pub query: Query,
    /// Entries the server sent for the page, before `collapse` joined the
    /// starts and stops of a playback.
    pub fetched: usize,
    /// Names of the users, by id.
    pub users: HashMap<String, String>,
}

pub fn load(client: &Client, query: &Query) -> Result<Data> {
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
        let names = scope.spawn(|| client.users::<Vec<Name>>());
        let page: Result<Page> = client.activity_log(&query.pairs(jiff::Timestamp::now().as_second()));
        (page, names.join().expect("names thread"))
    });
    let page = page?;
    // The log is still useful without the names.
    let users = users
        .unwrap_or_default()
        .into_iter()
        .map(|u| (u.id, u.name))
        .collect();
    let fetched = page.items.len();
    // `collapse` reads the newest first.
    let entries = if query.oldest_first {
        let mut items = page.items;
        items.reverse();
        let mut joined = collapse(items);
        joined.reverse();
        joined
    } else {
        collapse(page.items)
    };
    Ok(Data {
        entries,
        total: page.total_record_count,
        query: query.clone(),
        fetched,
        users,
    })
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
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

    let filtered = data.query.filtered();
    let page = div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .child(stat(LucideIcon::Activity, "Entries shown", data.entries.len(), cx))
                .child(stat(LucideIcon::Server, if filtered { "Matching" } else { "In the log" }, data.total, cx))
                .child(stat(LucideIcon::Play, "Playbacks", plays, cx))
                .child(stat(LucideIcon::TriangleAlert, "Warnings and errors", alerts, cx)),
        )
        .child(toolbar(app, data, cx));

    if data.entries.is_empty() {
        let mut empty = card(cx).gap(px(14.)).child(
            div()
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child(if filtered { "No entries match these filters." } else { "The activity log is empty." }),
        );
        if filtered {
            empty = empty.child(div().flex().child(
                super::button("activity.clear", "Clear filters", super::ButtonKind::Plain, cx).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        set_query(this, |q| *q = Query { oldest_first: q.oldest_first, ..Query::default() }, false, cx)
                    }),
                ),
            ));
        }
        return page.child(empty);
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
    let pager = pager(data, cx);
    page.children(groups.into_iter().zip(ends).map(|((group, start), end)| {
        group.child(rows(cx, end - start, ROW_H, move |this, range, cx| {
            let Some(data) = this.admin_data().and_then(|d| d.activity.as_ref()) else {
                return Vec::new();
            };
            let show_kind = content_width(this) >= 760.;
            data.entries
                .get(start + range.start..start + range.end)
                .unwrap_or_default()
                .iter()
                .map(|entry| row(entry, data, show_kind, cx).into_any_element())
                .collect()
        }))
    }))
    .children(pager)
}

/// Changes the query of the page and loads it. A change of a filter or
/// of the sort starts at the first page; a step of the pager keeps the
/// start it sets (`keep_page`).
fn set_query(app: &mut Bloom, change: impl FnOnce(&mut Query), keep_page: bool, cx: &mut Context<Bloom>) {
    let Some(data) = app.admin_data_mut() else {
        return;
    };
    change(&mut data.activity_query);
    if !keep_page {
        data.activity_query.start = 0;
    }
    app.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
    app.load_page(cx);
    cx.notify();
}

/// One select of the toolbar: the value on show, and a click opens the
/// list of the options (the shared list of the selects, one popup).
fn filter(
    id: &'static str,
    shown: String,
    options: Vec<(String, bool, Rc<dyn Fn(&mut Query)>)>,
    cx: &mut Context<Bloom>,
) -> gpui_kit::base::Button {
    use crate::{settings::Choice, ui::form::select};
    let longest = options
        .iter()
        .map(|(label, _, _)| label.as_str())
        .max_by_key(|label| label.chars().count())
        .unwrap_or_default()
        .to_string();
    let options = Rc::new(options);
    select(SharedString::from(id), shown, &longest, cx)
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            let choices = options
                .iter()
                .map(|(label, selected, change)| {
                    let change = change.clone();
                    Choice::new(label.clone(), *selected, move |this, cx| set_query(this, |q| change(q), false, cx))
                })
                .collect();
            this.open_choices(choices, event.position(), window, cx);
        }))
}

/// The filters and the sort, as selects on one line (they wrap).
fn toolbar(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let q = app.admin_data().map_or_else(|| data.query.clone(), |d| d.activity_query.clone());
    let kinds = Kind::ALL
        .iter()
        .map(|&kind| (kind.label().to_string(), q.kind == kind, Rc::new(move |q: &mut Query| q.kind = kind) as Rc<dyn Fn(&mut Query)>))
        .collect();
    let levels = Level::ALL
        .iter()
        .map(|&level| (level.label().to_string(), q.level == level, Rc::new(move |q: &mut Query| q.level = level) as Rc<dyn Fn(&mut Query)>))
        .collect();
    let mut names: Vec<String> = data.users.values().cloned().collect();
    names.sort_by_key(|name| name.to_lowercase());
    let whos = [Who::Anyone, Who::Server]
        .into_iter()
        .chain(names.into_iter().map(Who::User))
        .map(|who| {
            let (label, on) = (who.label(), q.who == who);
            (label, on, Rc::new(move |q: &mut Query| q.who = who.clone()) as Rc<dyn Fn(&mut Query)>)
        })
        .collect();
    let periods = Period::ALL
        .iter()
        .map(|&period| (period.label().to_string(), q.period == period, Rc::new(move |q: &mut Query| q.period = period) as Rc<dyn Fn(&mut Query)>))
        .collect();
    let sorts = [("Newest first", false), ("Oldest first", true)]
        .into_iter()
        .map(|(label, oldest)| {
            (label.to_string(), q.oldest_first == oldest, Rc::new(move |q: &mut Query| q.oldest_first = oldest) as Rc<dyn Fn(&mut Query)>)
        })
        .collect();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(10.))
        .child(filter("activity.filter.kind", q.kind.label().to_string(), kinds, cx))
        .child(filter("activity.filter.level", q.level.label().to_string(), levels, cx))
        .child(filter("activity.filter.user", q.who.label(), whos, cx))
        .child(filter("activity.filter.period", q.period.label().to_string(), periods, cx))
        .child(filter(
            "activity.filter.sort",
            if q.oldest_first { "Oldest first" } else { "Newest first" }.to_string(),
            sorts,
            cx,
        ))
}

/// Previous, the range on show, Next; none when one page holds all.
fn pager(data: &Data, cx: &mut Context<Bloom>) -> Option<Div> {
    let q = &data.query;
    let (previous, next) = (q.previous(), q.next(data.total));
    if previous.is_none() && next.is_none() {
        return None;
    }
    let t = UiTheme::read(cx).clone();
    let step = |id: &'static str, label: &'static str, to: Option<usize>, cx: &mut Context<Bloom>| {
        let button = super::button(id, label, super::ButtonKind::Plain, cx);
        match to {
            Some(start) => button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                set_query(this, |q| q.start = start, true, cx)
            })),
            None => button.opacity(0.35),
        }
    };
    Some(
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .child(step("activity.page.previous", "Previous", previous, cx))
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(t.colors.foreground.opacity(0.6))
                    .child(q.range_label(data.fetched, data.total)),
            )
            .child(step("activity.page.next", "Next", next, cx)),
    )
}

/// Sets the activity log's query from words, for the debug channel:
/// `kind=session level=error who=server|<name>|anyone period=week
/// sort=oldest page=next|previous|first`. The page loads again.
pub(crate) fn debug_query(app: &mut Bloom, words: &str, cx: &mut Context<Bloom>) -> String {
    let Some(data) = app.admin_data() else {
        return "error: the dashboard is not open".into();
    };
    let mut q = data.activity_query.clone();
    let total = data.activity.as_ref().map_or(0, |d| d.total);
    let start = q.start;
    q.start = 0;
    for word in words.split_whitespace() {
        let (key, value) = word.split_once('=').unwrap_or((word, ""));
        let pick = |labels: &[&str]| labels.iter().position(|l| l.to_lowercase().replace([' ', '-'], "").contains(value));
        match key {
            "kind" => match pick(&Kind::ALL.map(Kind::label)) {
                Some(i) => q.kind = Kind::ALL[i],
                None => return format!("error: no kind {value:?}"),
            },
            "level" => match pick(&Level::ALL.map(Level::label)) {
                Some(i) => q.level = Level::ALL[i],
                None => return format!("error: no level {value:?}"),
            },
            "period" => match value {
                "any" => q.period = Period::Any,
                "day" => q.period = Period::Day,
                "week" => q.period = Period::Week,
                "month" => q.period = Period::Month,
                _ => return format!("error: no period {value:?}"),
            },
            "who" => {
                q.who = match value {
                    "anyone" | "" => Who::Anyone,
                    "server" => Who::Server,
                    name => Who::User(name.to_string()),
                }
            }
            "sort" => q.oldest_first = value == "oldest",
            "page" => {
                let at = Query { start, ..q.clone() };
                q.start = match value {
                    "next" => at.next(total).unwrap_or(start),
                    "previous" => at.previous().unwrap_or(0),
                    _ => 0,
                }
            }
            "reset" => q = Query::default(),
            _ => return format!("error: activity kind= level= who= period= sort= page= | reset ({key:?})"),
        }
    }
    let wanted = q.clone();
    set_query(app, move |query| *query = wanted, true, cx);
    format!("ok: {:?}", q.pairs(jiff::Timestamp::now().as_second()))
}

/// One entry of the timeline.
fn row(entry: &Entry, data: &Data, show_kind: bool, cx: &mut Context<Bloom>) -> Div {
    // The colours alone: a row is built for every frame of a scroll.
    let colors = UiTheme::read(cx).colors;
    let (glyph, state) = entry.look();
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
                .text_color(colors.foreground.opacity(0.55))
                .child(time),
        )
        .child(icon_disc(glyph, state, 32.))
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
                        .text_color(colors.foreground)
                        .child(entry.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(13.))
                        .text_color(colors.foreground.opacity(0.55))
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
                .child(badge(entry.kind_label(), state)),
        );
    }
    if let Some(user) = user {
        row = row.child(avatar(&user, None, 26.));
    }
    row
}
