// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the user sees of the downloads: the button on a detail page, the
//! entries of the card menu, the chip in the top bar, the Downloads page
//! and the settings section. One ink; a colour only for a state.

use crate::ui::tone::{self, State};
use std::{
    collections::{BTreeMap, HashMap},
    rc::Rc,
};

use gpui_icons::LucideIcon;
use gpui_kit::{
    Bounds, Canvas, ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ObjectFit,
    ParentElement as _, Path, Pixels, Point, Rgba, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Window, canvas, div, point, prelude::FluentBuilder as _,
    px, rgba,
};

use super::{Entry, EntryState, engine, engine::Key, format_bytes};
use crate::{
    admin::Confirm,
    ui::rows::rows,
    app::{Bloom, Page},
    jellyfin::{Item, ItemKind},
    settings::Choice,
    ui::{button::raised, form::{field, group, row_icon as icon_button, select}},
    ui::{menu::MenuItem, scroll_area::ScrollArea, theme::UiTheme, tip::tip},
    views::cards::{CARD_RADIUS, icon},
};

/// Green for a download under way, amber for one that waits, red for one
/// that failed: the state colours of the dashboard.
fn state_dot(state: EntryState) -> Option<State> {
    match state {
        EntryState::Downloading => Some(State::Ok),
        EntryState::Queued | EntryState::Paused => Some(State::Warn),
        EntryState::Failed => Some(State::Bad),
        EntryState::Done => None,
    }
}

/// Paints a part of a ring, from `from` to `to` of a turn, clockwise from
/// the top. The path is a fan of quads along the arc.
fn paint_arc(
    window: &mut Window,
    center: Point<Pixels>,
    inner: f32,
    outer: f32,
    from: f32,
    to: f32,
    color: Rgba,
) {
    if to <= from {
        return;
    }
    let steps = ((to - from) * 48.).ceil().max(1.) as usize;
    let at = |r: f32, t: f32| {
        let angle = -std::f32::consts::FRAC_PI_2 + t * std::f32::consts::TAU;
        point(center.x + px(r * angle.cos()), center.y + px(r * angle.sin()))
    };
    let mut path = Path::new(center);
    for i in 0..steps {
        let t0 = from + (to - from) * i as f32 / steps as f32;
        let t1 = from + (to - from) * (i + 1) as f32 / steps as f32;
        path.move_to(at(outer, t0));
        path.line_to(at(outer, t1));
        path.line_to(at(inner, t1));
        path.line_to(at(inner, t0));
    }
    let color: gpui_kit::Hsla = color.into();
    window.paint_path(path, color);
}

/// A progress ring: a quiet track and the ink over it up to `progress`.
fn ring(progress: f32, size: f32, stroke: f32, ink: Rgba) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
            let center = bounds.center();
            let outer = size / 2.;
            paint_arc(window, center, outer - stroke, outer, 0., 1., ink.opacity(0.25));
            paint_arc(window, center, outer - stroke, outer, 0., progress.clamp(0., 1.), ink);
        },
    )
    .size(px(size))
    .flex_shrink_0()
}

/// A ring with the percent inside it, for a button.
fn ring_with_label(progress: f32, ink: Rgba) -> Div {
    div()
        .relative()
        .size(px(32.))
        .flex()
        .items_center()
        .justify_center()
        .child(ring(progress, 32., 3., ink).absolute().inset_0())
        .child(
            div()
                .text_size(px(9.5))
                .line_height(px(10.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(ink)
                .child(format!("{}", (progress * 100.).floor() as u32)),
        )
}

/// How far a set of downloads is, each file counting the same: a file
/// that is done counts whole, and one that waits (its size is not known
/// until its worker starts) counts as not begun. By bytes, the waiting
/// files were left out, and the ring ran from 0 to 100 for each file.
fn sum_progress(entries: &[&Entry]) -> f32 {
    if entries.is_empty() {
        return 0.;
    }
    let part = |entry: &&Entry| match entry.state {
        EntryState::Done => 1.,
        _ => entry.progress().unwrap_or(0.).clamp(0., 1.),
    };
    entries.iter().map(part).sum::<f32>() / entries.len() as f32
}

/// Height of a row of the Downloads page: the artwork (a still of 50
/// points, a poster of 66) and 6 points over and under it. The text
/// beside it is 48 points at most: a title line of 20, a detail line of
/// 16, and the progress bar with its gaps (3 + 3 + 2 + 4).
const ROW_EPISODE: f32 = 62.;
const ROW_MOVIE: f32 = 78.;

/// One row of the Downloads page.
struct Row {
    entry: Entry,
    /// Address of the poster in the folder of the item, for the image
    /// loader, when the file is there.
    poster: Option<String>,
}

/// The rows under one heading: a series, or "Movies".
struct Group {
    name: String,
    /// Bytes of its files, as far as the sizes are known.
    size: u64,
    row_h: f32,
    rows: Vec<Row>,
}

/// What the Downloads page lists, read from the engine and the disk once
/// for each change of the engine (`Engine::version`), not at each frame:
/// the entries by series, the poster of each, and the free space. Before,
/// a frame of the page asked the disk once for each row and once more
/// for the space, and a download draws four frames a second.
#[derive(Default)]
pub(super) struct Listing {
    version: u64,
    groups: Vec<Group>,
    free: Option<u64>,
}

impl Listing {
    fn refresh(&mut self) {
        let engine = engine();
        let version = engine.version();
        if self.version == version {
            return;
        }
        self.version = version;
        self.free = engine.free_bytes();
        let dir = engine.dir();
        // A poster that was found stays for the life of its entry (the
        // time it was added tells an entry from one made again); only
        // one that is not there yet is looked for again.
        let mut found: HashMap<(String, String, i64), String> = std::mem::take(&mut self.groups)
            .into_iter()
            .flat_map(|group| group.rows)
            .filter_map(|row| Some(((row.entry.server_id, row.entry.item_id, row.entry.added), row.poster?)))
            .collect();
        // By series, then by season and episode.
        let mut groups: BTreeMap<String, Vec<Row>> = BTreeMap::new();
        for entry in engine.entries() {
            let poster = found
                .remove(&(entry.server_id.clone(), entry.item_id.clone(), entry.added))
                .or_else(|| {
                    let path = dir.join(&entry.server_id).join(&entry.item_id).join(super::engine::POSTER_FILE);
                    path.is_file().then(|| format!("file://{}", path.display()))
                });
            groups.entry(entry.group()).or_default().push(Row { entry, poster });
        }
        self.groups = groups
            .into_iter()
            .map(|(name, mut rows)| {
                rows.sort_by_key(|row| (row.entry.season.unwrap_or(0), row.entry.episode.unwrap_or(0), row.entry.name.clone()));
                let size = rows.iter().map(|row| row.entry.total.unwrap_or(row.entry.done)).sum();
                let row_h = match rows.iter().all(|row| row.entry.kind == "Episode") {
                    true => ROW_EPISODE,
                    false => ROW_MOVIE,
                };
                Group { name, size, row_h, rows }
            })
            .collect();
    }
}

impl Bloom {
    /// The download button of a detail page: download, a ring with the
    /// percent, or downloaded (a click removes the file). On a season it
    /// stands for the episodes of the season.
    pub fn render_download_action(&self, item: &Item, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !self.downloads_allowed() {
            return None;
        }
        let fg = rgba(0xffffffde);
        let button = || {
            div()
                .id("detail.download")
                .size(px(46.))
                .rounded(px(CARD_RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0x00000066)))
        };
        let engine = engine();
        if item.kind == ItemKind::Season {
            let episodes: Vec<Item> = match &self.page {
                Page::Detail(data) => data.episodes.iter().filter(|e| e.is_playable()).cloned().collect(),
                _ => Vec::new(),
            };
            if episodes.is_empty() {
                return None;
            }
            let entries: Vec<Option<Entry>> = episodes.iter().map(|e| engine.entry(&e.id)).collect();
            let active: Vec<&Entry> = entries.iter().flatten().filter(|e| e.is_active()).collect();
            if !active.is_empty() {
                // The episodes of the season that are done count too.
                let batch: Vec<&Entry> = entries
                    .iter()
                    .flatten()
                    .filter(|e| e.is_active() || e.state == EntryState::Done)
                    .collect();
                let progress = sum_progress(&batch);
                let ids: Vec<String> = active.iter().map(|e| e.item_id.clone()).collect();
                return Some(
                    button()
                        .tooltip(tip(format!(
                            "Downloading the season · {} of {} · Click to pause",
                            active.len(),
                            episodes.len()
                        )))
                        .child(ring_with_label(progress, fg))
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            super::engine().cancel_many(&ids);
                            cx.notify();
                        })),
                );
            }
            let missing: Vec<Item> = episodes
                .iter()
                .zip(&entries)
                .filter(|(_, entry)| entry.as_ref().map(|e| e.state) != Some(EntryState::Done))
                .map(|(episode, _)| episode.clone())
                .collect();
            if !missing.is_empty() {
                let text = format!("Download the season · {} episode(s)", missing.len());
                return Some(
                    button()
                        .tooltip(tip(text))
                        .child(icon(LucideIcon::Download, 25., fg))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.download_episodes(&missing, cx)
                        })),
                );
            }
            let ids: Vec<String> = episodes.iter().map(|e| e.id.clone()).collect();
            return Some(
                button()
                    .bg(tone::EDGE)
                    .tooltip(tip("Season downloaded · Click to remove the files"))
                    .child(icon(LucideIcon::CircleCheck, 25., fg))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        super::engine().remove_many(&ids);
                        cx.notify();
                    })),
            );
        }
        if !item.is_playable() {
            return None;
        }
        let id = item.id.clone();
        let target = item.clone();
        Some(match engine.entry(&item.id) {
            None => button()
                .tooltip(tip("Download for offline use"))
                .child(icon(LucideIcon::Download, 25., fg))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.download_item(&target, cx)
                })),
            Some(entry) => match entry.state {
                EntryState::Queued | EntryState::Downloading => button()
                    .tooltip(tip(format!("{} · Click to pause", entry.state_text())))
                    .child(ring_with_label(entry.progress().unwrap_or(0.), fg))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        super::engine().cancel(&id);
                        cx.notify();
                    })),
                EntryState::Paused => button()
                    .tooltip(tip(format!("{} · Click to resume", entry.state_text())))
                    .child(ring_with_label(entry.progress().unwrap_or(0.), fg.opacity(0.6)))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        super::engine().resume(&id);
                        cx.notify();
                    })),
                EntryState::Failed => button()
                    .relative()
                    .tooltip(tip(format!("Download failed: {} · Click to try again", entry.state_text())))
                    .child(icon(LucideIcon::RefreshCw, 24., fg))
                    .child(
                        div()
                            .absolute()
                            .top(px(8.))
                            .right(px(8.))
                            .size(px(8.))
                            .rounded_full()
                            .bg(State::Bad.color()),
                    )
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        super::engine().resume(&id);
                        cx.notify();
                    })),
                EntryState::Done => button()
                    .bg(tone::EDGE)
                    .tooltip(tip("Downloaded · Click to remove the file"))
                    .child(icon(LucideIcon::CircleCheck, 25., fg))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        super::engine().remove(&id);
                        cx.notify();
                    })),
            },
        })
    }

    /// The entries of the card menu: download, pause, resume or remove.
    pub fn download_menu_items(&self, item: &Item, cx: &mut Context<Self>) -> Vec<MenuItem> {
        if !self.downloads_allowed() || self.downloads.offline {
            return Vec::new();
        }
        let this = cx.weak_entity();
        let mut items = Vec::new();
        if item.kind == ItemKind::Season {
            let (Some(series_id), season_id) = (item.series_id.clone(), item.id.clone()) else {
                return items;
            };
            items.push(MenuItem::separator());
            items.push(
                MenuItem::new("card.menu.download-season", "Download season")
                    .icon(LucideIcon::Download)
                    .on_click(move |_, _, cx| {
                        let (series_id, season_id) = (series_id.clone(), season_id.clone());
                        this.update(cx, |this, cx| {
                            this.fetch(
                                cx,
                                move |client| client.episodes(&series_id, &season_id),
                                |this, result, cx| match result {
                                    Ok(episodes) => this.download_episodes(&episodes, cx),
                                    Err(err) => this.toast("Download", format!("{err:#}"), cx),
                                },
                            )
                        })
                        .ok();
                    }),
            );
            return items;
        }
        if !item.is_playable() {
            return items;
        }
        let id = item.id.clone();
        let target = item.clone();
        let (label, glyph): (&str, LucideIcon) = match engine().state_of(&item.id) {
            None => ("Download", LucideIcon::Download),
            Some(EntryState::Queued | EntryState::Downloading) => ("Pause download", LucideIcon::Pause),
            Some(EntryState::Paused | EntryState::Failed) => ("Resume download", LucideIcon::RefreshCw),
            Some(EntryState::Done) => ("Remove download", LucideIcon::Trash),
        };
        items.push(MenuItem::separator());
        items.push(
            MenuItem::new("card.menu.download", label)
                .icon(glyph)
                .on_click(move |_, _, cx| {
                    let (id, target) = (id.clone(), target.clone());
                    this.update(cx, |this, cx| {
                        let engine = engine();
                        match engine.state_of(&id) {
                            None => this.download_item(&target, cx),
                            Some(EntryState::Queued | EntryState::Downloading) => engine.cancel(&id),
                            Some(EntryState::Paused | EntryState::Failed) => engine.resume(&id),
                            Some(EntryState::Done) => engine.remove(&id),
                        }
                        cx.notify();
                    })
                    .ok();
                }),
        );
        items
    }

    /// A small chip in the top bar while something downloads: a ring and
    /// the percent. A click opens the Downloads page.
    pub fn render_downloads_chip(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let all = engine().summary();
        if all.active == 0 {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let fg = t.colors.foreground;
        let progress = all.progress;
        let label = if all.downloading > 0 {
            format!("{}%", (progress * 100.).floor() as u32)
        } else {
            "Queued".to_string()
        };
        let text = format!("Downloads · {} active · {}", all.active, label);
        Some(
            div()
                .id("top.downloads")
                .h(px(36.))
                .px(px(12.))
                .rounded_full()
                .bg(tone::HOVER)
                .hover(|s| s.bg(rgba(0xffffff29)))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(13.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(fg)
                .tooltip(tip(text))
                .child(ring(progress, 16., 2.5, fg))
                .child(label)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_downloads(cx))),
        )
    }

    /// The Downloads page: the items on this Mac by series, the space they
    /// take, and what each one does.
    pub fn render_downloads(&self, cx: &mut Context<Self>) -> Div {
        let t = UiTheme::read(cx).clone();
        let quiet = t.colors.foreground.opacity(0.6);
        let engine = engine();
        self.downloads.listing.borrow_mut().refresh();
        let listing = self.downloads.listing.borrow();
        let all = engine.summary();
        let narrow = self.viewport_w < 860.;

        // Summary: count, space in use, space free.
        let (used, free, active) = (all.used, listing.free, all.active);
        let mut facts = vec![format!("{} item{}", all.entries, if all.entries == 1 { "" } else { "s" })];
        if active > 0 {
            facts.push(format!("{active} downloading"));
        }
        facts.push(format!("{} on {}", format_bytes(used), crate::brand::THIS_DEVICE));
        if let Some(free) = free {
            facts.push(format!("{} free", format_bytes(free)));
        }
        if let Some(limit) = self.config.download_limit_gb {
            facts.push(format!("limit {limit} GB"));
        }

        let mut actions = div().flex().flex_wrap().gap(px(8.));
        if self.downloads.offline {
            actions = actions.child(
                raised("downloads.retry", "Try the server again", cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.retry_connection(cx))),
            );
        }
        if all.entries > 0 {
            actions = actions.child(
                raised("downloads.remove-all", "Remove all", cx).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.ask_confirm(
                            Confirm {
                                title: "Remove all downloads?".to_string(),
                                message: format!(
                                    "The files take {} on {}. They can be downloaded again.",
                                    format_bytes(super::engine().used_bytes()),
                                    crate::brand::THIS_DEVICE
                                ),
                                action: "Remove all".to_string(),
                                danger: true,
                                run: Rc::new(|_, cx| {
                                    super::engine().remove_all();
                                    cx.notify();
                                }),
                            },
                            cx,
                        )
                    },
                )),
            );
        }
        // A phone has no Finder; the files are the app's own.
        if cfg!(target_os = "macos") {
            let dir = engine.dir();
            actions = actions.child(
                raised("downloads.reveal", "Show in Finder", cx)
                    .on_click(move |_: &ClickEvent, _, cx| cx.reveal_path(&dir)),
            );
        }

        // A phone has the title in the top bar.
        let compact = self.compact_w();
        let mut page = div()
            .max_w(px(980.))
            .px(px(if compact { 16. } else if narrow { 20. } else { 28. }))
            .pt(px(8.))
            .pb(px(72.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .when(!compact, |el| {
                el.child(
                    div()
                        .text_size(px(30.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground)
                        .child("Downloads"),
                )
            });
        if self.downloads.offline {
            page = page.child(
                div()
                    .rounded(px(24.))
                    .border_1()
                    .border_color(tone::EDGE)
                    .bg(tone::CARD)
                    .px(px(22.))
                    .py(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(icon(LucideIcon::WifiOff, 20., t.colors.foreground))
                    .child(
                        div()
                            .text_size(px(15.))
                            .text_color(t.colors.foreground)
                            .child(format!("The server cannot be reached. Your downloads play from {}.", crate::brand::THIS_DEVICE)),
                    ),
            );
        }
        page = page.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .child(div().text_size(px(14.)).text_color(quiet).child(facts.join(" · ")))
                .child(actions),
        );

        if all.entries == 0 {
            page = page.child(
                div()
                    .py(px(40.))
                    .text_size(px(15.))
                    .text_color(quiet)
                    .child("Nothing is downloaded. Use the download button on a movie, an episode or a season."),
            );
        }

        // A card for each series. Only the rows in view are built (see
        // `rows`); they read the listing again when they are.
        for (place, set) in listing.groups.iter().enumerate() {
            let title = SharedString::from(format!("{} · {}", set.name, format_bytes(set.size)));
            let list = rows(cx, set.rows.len(), set.row_h, move |this, range, cx| {
                let listing = this.downloads.listing.borrow();
                let Some(set) = listing.groups.get(place) else {
                    return Vec::new();
                };
                range
                    .filter_map(|at| set.rows.get(at))
                    .map(|row| this.render_download_row(row, cx).into_any_element())
                    .collect()
            });
            page = page.child(group(title, cx).w_full().child(list));
        }

        div()
            .relative()
            .size_full()
            .child(
                ScrollArea::new("downloads.scroll")
                    .track(&self.page_scroll)
                    .size_full()
                    .child(page),
            )
    }

    /// One item of the Downloads page: artwork, title, state and size, a
    /// progress bar while it is not done, and its buttons.
    fn render_download_row(&self, row: &Row, cx: &mut Context<Self>) -> Stateful<Div> {
        let entry = &row.entry;
        let t = UiTheme::read(cx).clone();
        let fg = t.colors.foreground;
        let quiet = fg.opacity(0.6);
        let id = entry.item_id.clone();
        let episode = entry.kind == "Episode";
        let (w, h) = if episode { (88., 50.) } else { (44., 66.) };
        let art = div()
            .relative()
            .w(px(w))
            .h(px(h))
            .flex_shrink_0()
            .rounded(px(8.))
            .bg(t.colors.muted)
            .flex()
            .items_center()
            .justify_center()
            .child(icon(if episode { LucideIcon::Tv } else { LucideIcon::Clapperboard }, 20., t.colors.muted_foreground))
            .when_some(row.poster.clone(), |el, url| {
                el.child(
                    crate::images::remote_with(url, px(8.), ObjectFit::Cover)
                        .absolute()
                        .inset_0(),
                )
            });
        let mut detail = entry.state_text();
        match (entry.state, entry.total) {
            (EntryState::Done, Some(total)) => detail.push_str(&format!(" · {}", format_bytes(total))),
            (_, Some(total)) => detail.push_str(&format!(" · {} of {}", format_bytes(entry.done), format_bytes(total))),
            _ => {}
        }
        let dot = state_dot(entry.state);
        let mut text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(3.))
            .child(
                div()
                    .truncate()
                    .text_size(px(15.))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(fg)
                    .child(entry.title()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    // With the line of the page (20 points) a row with a
                    // progress bar is 52 points, and a row has 50.
                    .line_height(px(16.))
                    .text_color(quiet)
                    .children(dot.map(|state| div().size(px(6.)).rounded_full().flex_shrink_0().bg(state.color())))
                    .child(div().truncate().child(detail)),
            );
        if entry.state != EntryState::Done {
            text = text.child(
                div()
                    .mt(px(2.))
                    .h(px(4.))
                    .w_full()
                    .rounded_full()
                    .overflow_hidden()
                    .bg(tone::HOVER)
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .w(gpui_kit::relative(entry.progress().unwrap_or(0.)))
                            .bg(fg.opacity(0.9)),
                    ),
            );
        }

        let mut buttons = div().flex().items_center().gap(px(2.)).flex_shrink_0();
        // The row is of one server: its ids and its actions name it. Two
        // servers can hold an item under one id, and two elements with
        // one id stop the accessibility tree.
        let button_id =
            |what: &str| SharedString::from(format!("downloads.{what}.{}.{}", entry.server_id, entry.item_id));
        let server = entry.server_id.clone();
        match entry.state {
            EntryState::Done => {
                let (play, server) = (id.clone(), server.clone());
                buttons = buttons.child(
                    icon_button(button_id("play"), LucideIcon::Play, true, cx)
                        .tooltip(tip(format!("Play from {}", crate::brand::THIS_DEVICE)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.play_download(Some(&server), &play, window, cx);
                        })),
                );
            }
            EntryState::Queued | EntryState::Downloading => {
                let (pause, server) = (id.clone(), server.clone());
                buttons = buttons.child(
                    icon_button(button_id("pause"), LucideIcon::Pause, true, cx)
                        .tooltip(tip("Pause"))
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            super::engine().cancel(Key::of(Some(&server), &pause));
                            cx.notify();
                        })),
                );
            }
            EntryState::Paused | EntryState::Failed => {
                let (resume, server) = (id.clone(), server.clone());
                buttons = buttons.child(
                    icon_button(button_id("resume"), LucideIcon::RefreshCw, !self.downloads.offline, cx)
                        .tooltip(tip(if entry.state == EntryState::Failed { "Try again" } else { "Resume" }))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            if this.downloads.offline {
                                return;
                            }
                            super::engine().resume(Key::of(Some(&server), &resume));
                            cx.notify();
                        })),
                );
            }
        }
        let remove = id.clone();
        buttons = buttons.child(
            icon_button(button_id("remove"), LucideIcon::Trash, true, cx)
                .tooltip(tip(format!("Remove from {}", crate::brand::THIS_DEVICE)))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    super::engine().remove(Key::of(Some(&server), &remove));
                    cx.notify();
                })),
        );

        let (open, open_server) = (id.clone(), entry.server_id.clone());
        div()
            .id(button_id("row"))
            .h_full()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .cursor_pointer()
            .hover(|s| s.bg(tone::HOVER))
            .child(art)
            .child(text)
            .child(buttons)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_download(Some(&open_server), &open, cx)))
    }

    /// The Downloads section of the settings: where the files are, the
    /// storage limit, and how many download at once.
    pub(crate) fn render_settings_downloads(&self, cx: &mut Context<Self>) -> Div {
        let t = UiTheme::read(cx).clone();
        let engine = engine();
        let dir = engine.dir();
        let dir_text = dir.display().to_string();
        let limit = self.config.download_limit_gb;
        let limit_text = limit.map_or("No limit".to_string(), |gb| format!("{gb} GB"));
        let parallel = self.config.download_parallel.unwrap_or(1).clamp(1, 2);
        let used = engine.used_bytes();
        let free = engine.free_bytes();
        let space = match free {
            Some(free) => format!("{} in use · {} free", format_bytes(used), format_bytes(free)),
            None => format!("{} in use", format_bytes(used)),
        };
        let reveal = dir.clone();
        let mut card = group("Downloads", cx)
            // A phone has no Finder, and the folder is the app's own.
            .when(cfg!(target_os = "macos"), |card| {
                card.child(field(
                    "Location",
                    dir_text,
                    raised("settings.downloads.reveal", "Show in Finder", cx)
                        .on_click(move |_: &ClickEvent, _, cx| cx.reveal_path(&reveal)),
                    cx,
                ))
            })
            .child(field(
                "Storage limit",
                format!("Space all downloads may take. A download stops before it goes over. {} always stay free.", format_bytes(super::engine::MIN_FREE_BYTES)),
                select("settings.downloads.limit", limit_text, "No limit", cx).on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        let mut choices = vec![Choice::new("No limit", limit.is_none(), |this, cx| {
                            this.set_download_limit(None, cx)
                        })];
                        for gb in [10u64, 25, 50, 100, 250, 500] {
                            choices.push(Choice::new(format!("{gb} GB"), limit == Some(gb), move |this, cx| {
                                this.set_download_limit(Some(gb), cx)
                            }));
                        }
                        this.open_choices(choices, event.position(), window, cx);
                    },
                )),
                cx,
            ))
            .child(field(
                "At the same time",
                "Downloads that run at once.",
                select("settings.downloads.parallel", parallel.to_string(), "2", cx).on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        let choices = [1u8, 2]
                            .into_iter()
                            .map(|n| {
                                Choice::new(n.to_string(), parallel == n, move |this, cx| {
                                    this.set_download_parallel(n, cx)
                                })
                            })
                            .collect();
                        this.open_choices(choices, event.position(), window, cx);
                    },
                )),
                cx,
            ))
            .child(field("Space", space, div(), cx));
        if self.downloads.allowed == Some(false) {
            card = card.child(
                div()
                    .py(px(11.))
                    .text_size(px(13.))
                    .text_color(t.colors.foreground.opacity(0.6))
                    .child("The server does not allow this account to download files."),
            );
        }
        let open = raised("settings.downloads.open", "Open the Downloads page", cx)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_downloads(cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(card)
            .child(div().flex().child(open))
    }

    pub fn set_download_limit(&mut self, gb: Option<u64>, cx: &mut Context<Self>) {
        self.config.download_limit_gb = gb;
        self.save_config(cx);
        engine().set_options(move |o| o.limit_bytes = gb.map(|gb| gb.saturating_mul(1_000_000_000)));
        cx.notify();
    }

    pub fn set_download_parallel(&mut self, n: u8, cx: &mut Context<Self>) {
        self.config.download_parallel = Some(n.clamp(1, 2));
        self.save_config(cx);
        engine().set_options(move |o| o.parallel = n.clamp(1, 2) as usize);
        cx.notify();
    }
}
