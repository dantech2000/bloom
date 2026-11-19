// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dashboard home: the server and its actions, item counts, who plays what
//! at this moment, tasks that run, recent activity, alerts and storage.
//! The part that changes by itself ([`Live`]) loads again every few seconds.

use crate::ui::tone::{self, State};
use std::rc::Rc;

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, ObjectFit,
    ParentElement as _, Rgba, SharedString, StatefulInteractiveElement as _, Styled, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px, relative, rgb, rgba,
};
use serde::Deserialize;

use super::{
    ButtonKind, Confirm,
    activity::{Entry, collapse},
    ago, badge, button, card, fact, panel, parse_date,
    plugin_config,
    users::{avatar, icon_disc},
};
use crate::{
    app::Bloom,
    jellyfin::{Client, Item, format_duration},
    ui::theme::UiTheme,
    icons::icon,
};

/// Space between two cards.
const GAP: f32 = 16.;
/// A device counts as active when the server heard from it in this time.
const ACTIVE_SECONDS: i64 = 960;
/// Height of a session card.
const SESSION_H: f32 = 196.;
/// Height of the card of a device that plays nothing.
const IDLE_H: f32 = 100.;

pub struct Data {
    info: Info,
    counts: Counts,
    storage: Storage,
    /// The newest items, for "Recently added".
    recent: Vec<Recent>,
    pub live: Live,
}

/// Days "Recently added" looks back.
const RECENT_DAYS: i64 = 7;
/// Items "Recently added" asks for; the count says "N+" past it.
const RECENT_LIMIT: usize = 50;
/// Items "Recently added" lists.
const RECENT_SHOWN: usize = 6;
/// Groups of alerts the overview shows.
const ALERT_GROUPS: usize = 7;

/// An item of "Recently added".
#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
struct Recent {
    id: String,
    name: String,
    #[serde(rename = "Type")]
    kind: String,
    series_name: Option<String>,
    index_number: Option<i32>,
    parent_index_number: Option<i32>,
    production_year: Option<i32>,
    date_created: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct RecentPage {
    items: Vec<Recent>,
}

#[easy_ext::ext(DashboardApi)]
impl Client {
    /// The newest items of the libraries: films, episodes, albums, books.
    fn recently_added(&self) -> Result<Vec<Recent>> {
        let page: RecentPage = self.get(
            "/Items",
            &[
                ("recursive", "true".to_string()),
                ("sortBy", "DateCreated".to_string()),
                ("sortOrder", "Descending".to_string()),
                ("includeItemTypes", "Movie,Episode,MusicAlbum,Book,MusicVideo".to_string()),
                ("isMissing", "false".to_string()),
                ("fields", "DateCreated".to_string()),
                ("enableImages", "false".to_string()),
                ("limit", RECENT_LIMIT.to_string()),
            ],
        )?;
        Ok(page.items)
    }
}

/// What the page loads again while it stays open.
#[derive(Default, PartialEq)]
pub struct Live {
    sessions: Vec<Session>,
    running: Vec<Task>,
    activity: Vec<Entry>,
    alerts: Vec<AlertGroup>,
}

/// Alerts with the same kind and text, newest first: eight failed sign-ins
/// of one name are one line, "× 8".
#[derive(Clone, Default, PartialEq)]
pub(super) struct AlertGroup {
    pub entries: Vec<Entry>,
}

impl AlertGroup {
    /// What tells this group from the others, also after a reload.
    pub fn key(&self) -> String {
        self.entries.first().map(|e| format!("{}|{}", e.kind, e.name)).unwrap_or_default()
    }
}

/// Puts alerts with the same kind and text together, in the order of their
/// newest entry, and keeps the first `max` groups.
pub(super) fn group_alerts(entries: Vec<Entry>, max: usize) -> Vec<AlertGroup> {
    let mut groups: Vec<AlertGroup> = Vec::new();
    for entry in entries {
        match groups
            .iter_mut()
            .find(|g| g.entries[0].kind == entry.kind && g.entries[0].name == entry.name)
        {
            Some(group) => group.entries.push(entry),
            None => groups.push(AlertGroup { entries: vec![entry] }),
        }
    }
    groups.truncate(max);
    groups
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Info {
    server_name: String,
    version: String,
    product_name: String,
    operating_system: String,
    operating_system_display_name: String,
    system_architecture: String,
    local_address: String,
    has_pending_restart: bool,
    has_update_available: bool,
    can_self_restart: bool,
    is_shutting_down: bool,
    program_data_path: String,
    cache_path: String,
    log_path: String,
    internal_metadata_path: String,
    transcoding_temp_path: String,
    web_path: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Counts {
    movie_count: u64,
    series_count: u64,
    episode_count: u64,
    box_set_count: u64,
    album_count: u64,
    song_count: u64,
    music_video_count: u64,
    book_count: u64,
    trailer_count: u64,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Folder {
    path: String,
    free_space: i64,
    used_space: i64,
    storage_type: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct LibraryFolders {
    name: String,
    folders: Vec<Folder>,
}

/// `/System/Info/Storage`. Servers before 10.11 do not have it.
#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Storage {
    program_data_folder: Option<Folder>,
    cache_folder: Option<Folder>,
    log_folder: Option<Folder>,
    internal_metadata_folder: Option<Folder>,
    transcoding_temp_folder: Option<Folder>,
    libraries: Vec<LibraryFolders>,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
struct PlayState {
    position_ticks: Option<i64>,
    is_paused: bool,
    play_method: Option<String>,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
struct Transcoding {
    video_codec: Option<String>,
    audio_codec: Option<String>,
    container: Option<String>,
    is_video_direct: bool,
    is_audio_direct: bool,
    bitrate: Option<i64>,
    height: Option<i64>,
    framerate: Option<f32>,
    completion_percentage: Option<f64>,
    hardware_acceleration_type: Option<String>,
    transcode_reasons: serde_json::Value,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
struct Session {
    id: String,
    device_id: String,
    user_id: Option<String>,
    user_name: Option<String>,
    user_primary_image_tag: Option<String>,
    client: String,
    device_name: String,
    application_version: String,
    last_activity_date: String,
    remote_end_point: String,
    now_playing_item: Option<Item>,
    play_state: PlayState,
    transcoding_info: Option<Transcoding>,
    /// Other idle sessions of the same device, folded into this row
    /// ([`one_row_per_device`]).
    #[serde(skip)]
    others: usize,
    /// The session of this app.
    #[serde(skip)]
    own: bool,
}

/// One row for each device. Rows with the same device name, app and user
/// that play nothing are one row: the newest, which counts the others.
/// They are sessions of one device: a second copy of the app, or a sign-in
/// the server still remembers. A session that plays keeps its own row.
/// `own_device` is the device id of this app; its row is marked.
fn one_row_per_device(sessions: Vec<Session>, own_device: &str) -> Vec<Session> {
    let mut rows: Vec<Session> = Vec::new();
    for mut session in sessions {
        session.own = session.device_id == own_device;
        let same = |row: &Session| {
            row.now_playing_item.is_none()
                && row.device_name == session.device_name
                && row.client == session.client
                && row.user_id == session.user_id
        };
        if session.now_playing_item.is_some() {
            rows.push(session);
            continue;
        }
        match rows.iter_mut().find(|row| same(row)) {
            Some(row) => {
                // The row shows this app when one of them is this app,
                // else the newest; the other counts.
                let (others, own) = (row.others + 1 + session.others, row.own || session.own);
                if session.own || (!row.own && session.last_activity_date > row.last_activity_date) {
                    *row = session;
                }
                row.others = others;
                row.own = own;
            }
            None => rows.push(session),
        }
    }
    rows
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Device {
    id: String,
    name: String,
    custom_name: Option<String>,
    app_name: String,
    app_version: String,
    last_user_id: Option<String>,
    last_user_name: Option<String>,
    date_last_activity: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Devices {
    items: Vec<Device>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct ActivityPage {
    items: Vec<Entry>,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
struct Task {
    name: String,
    state: super::tasks::TaskState,
    key: String,
    current_progress_percentage: Option<f64>,
}


pub fn load(client: &Client) -> Result<Data> {
    std::thread::scope(|scope| {
        let info = scope.spawn(|| client.system_info::<Info>());
        let recent = scope.spawn(|| client.recently_added());
        let counts = scope.spawn(|| client.item_counts::<Counts>());
        let storage = scope.spawn(|| client.storage_info::<Storage>());
        let live = refresh(client)?;
        Ok(Data {
            info: info.join().expect("info thread")?,
            counts: counts.join().expect("counts thread").unwrap_or_default(),
            storage: storage.join().expect("storage thread").unwrap_or_default(),
            recent: recent.join().expect("recent thread").unwrap_or_default(),
            live,
        })
    })
}

/// Loads only the part that changes while the page is open.
pub fn refresh(client: &Client) -> Result<Live> {
    // The last plays of the users, a start and its stop as one line.
    let played = || {
        client
            .activity_log::<ActivityPage>(&[("Limit", "30".to_string()), ("HasUserId", "true".to_string())])
            .map(|page| collapse(page.items).into_iter().take(7).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    // What the server did by itself: no user is named. Repeated alerts
    // are one line, so more are read than shown.
    let alerts = || {
        client
            .activity_log::<ActivityPage>(&[("Limit", "40".to_string()), ("HasUserId", "false".to_string())])
            .map(|e| group_alerts(e.items, ALERT_GROUPS))
            .unwrap_or_default()
    };
    std::thread::scope(|scope| {
        let tasks = scope.spawn(|| client.scheduled_tasks::<Vec<Task>>());
        let activity = scope.spawn(played);
        let alerts = scope.spawn(alerts);
        let devices = scope.spawn(|| client.devices::<Devices>());
        let mut sessions: Vec<Session> =
            client.sessions(&[("ActiveWithinSeconds", ACTIVE_SECONDS.to_string())])?;
        // The web shows only the sessions of a signed-in user.
        sessions.retain(|s| s.user_id.is_some());
        // A device that only asks the server for data (a TV that sits on its
        // home screen) has no session, but the Devices page shows it as
        // active. Show it here too, so the two pages agree.
        let now = jiff::Timestamp::now().as_second();
        let recent = |date: &str| {
            parse_date(date).is_some_and(|at| now - at.as_second() <= ACTIVE_SECONDS)
        };
        for device in devices
            .join()
            .expect("devices thread")
            .map(|d| d.items)
            .unwrap_or_default()
        {
            if !recent(&device.date_last_activity) {
                continue;
            }
            match sessions.iter_mut().find(|s| s.device_id == device.id) {
                Some(session) => {
                    if device.date_last_activity > session.last_activity_date {
                        session.last_activity_date = device.date_last_activity;
                    }
                }
                None => sessions.push(Session {
                    id: device.id.clone(),
                    device_id: device.id,
                    user_id: device.last_user_id,
                    user_name: device.last_user_name,
                    client: device.app_name,
                    device_name: device.custom_name.filter(|n| !n.is_empty()).unwrap_or(device.name),
                    application_version: device.app_version,
                    last_activity_date: device.date_last_activity,
                    ..Default::default()
                }),
            }
        }
        // Sessions that play come first, then the most recent ones.
        let mut sessions = one_row_per_device(sessions, &client.device_id);
        sessions.sort_by(|a, b| {
            (b.now_playing_item.is_some(), &b.last_activity_date)
                .cmp(&(a.now_playing_item.is_some(), &a.last_activity_date))
        });
        let mut running = tasks.join().expect("tasks thread").unwrap_or_default();
        running.retain(|t| t.state != super::tasks::TaskState::Idle);
        Ok(Live {
            sessions,
            running,
            activity: activity.join().expect("activity thread"),
            alerts: alerts.join().expect("alerts thread"),
        })
    })
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    // Width of the page content (`admin_content_w`).
    let width = app.admin_content_w();
    let wide = width >= 860.;
    let live = &data.live;
    let half = ((width - GAP) / 2.).floor();

    let pair = |left: Div, right: Div| {
        if wide {
            div()
                .flex()
                .items_start()
                .gap(px(GAP))
                // Set widths, not shares of the row: with shares the layout
                // engine measures each card several times, and that was
                // most of the cost of a frame of this page. The least
                // and the most width spare one more pass
                // (`Bloom::render_settings`).
                .child(left.w(px(half)).min_w(px(half)).max_w(px(half)).flex_none())
                .child(right.w(px(half)).min_w(px(half)).max_w(px(half)).flex_none())
        } else {
            div().flex().flex_col().gap(px(GAP)).child(left).child(right)
        }
    };

    div()
        .flex()
        .flex_col()
        .gap(px(GAP))
        .child(server_card(data, wide, cx))
        .child(count_tiles(data, width, cx))
        .child(pair(now(live, cx), recently_added(&data.recent, cx)))
        .child(sessions(app, &live.sessions, width, cx))
        .child(pair(
            entries("Activity", &live.activity, "No activity yet.", cx),
            alerts(app, &live.alerts, cx),
        ))
        .child(pair(storage(&data.storage, cx), paths(&data.info, cx)))
}

// ----- server -----------------------------------------------------------------

fn server_card(data: &Data, wide: bool, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let info = &data.info;
    let os = if info.operating_system_display_name.is_empty() {
        &info.operating_system
    } else {
        &info.operating_system_display_name
    };
    let state = if info.is_shutting_down {
        badge("Shutting down", Some(State::Bad))
    } else if info.has_pending_restart {
        badge("Restart needed", Some(State::Warn))
    } else if info.has_update_available {
        badge("Update available", None)
    } else {
        badge("Online", Some(State::Ok))
    };
    let chip = |text: String| {
        div()
            .px(px(10.))
            .py(px(3.))
            .rounded(px(8.))
            .bg(rgba(0xffffff1a))
            .text_size(px(13.))
            .text_color(t.colors.foreground.opacity(0.85))
            .child(text)
    };

    let scan = data.live.running.iter().find(|t| t.key == "RefreshLibrary");
    let scan_button = match scan {
        // A scan that runs shows its progress in place of the button.
        Some(task) => button(
            "admin.dash.scan",
            format!(
                "Scanning… {:.0}%",
                task.current_progress_percentage.unwrap_or(0.)
            ),
            ButtonKind::Plain,
            cx,
        ),
        None => button(
            "admin.dash.scan",
            "Scan All Libraries",
            ButtonKind::Primary,
            cx,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.admin_action("Library scan started", cx, |client| {
                client.scan_libraries()
            })
        })),
    };
    let power = super::more_button("admin.dash.power", "Restart or shut down the server", cx)
        .on_click(cx.listener({
            let (name, can_restart) = (info.server_name.clone(), info.can_self_restart);
            move |this, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                let items = power_menu(&name, can_restart, cx);
                this.open_admin_menu(items, event.position(), window, cx);
            }
        }));

    let identity = div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_w_0()
        .child(icon_disc(LucideIcon::Server, None, 56.))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        // A phone: the state goes under a long name.
                        .flex_wrap()
                        .items_center()
                        .gap(px(12.))
                        .child(
                            div()
                                .text_size(px(26.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child(info.server_name.clone()),
                        )
                        .child(state),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .child(chip(format!("{} {}", info.product_name, info.version)))
                        .when(!os.is_empty(), |el| el.child(chip(os.clone())))
                        .when(!info.system_architecture.is_empty(), |el| {
                            el.child(chip(info.system_architecture.clone()))
                        })
                        .when(!info.local_address.is_empty(), |el| {
                            el.child(chip(info.local_address.clone()))
                        }),
                ),
        );
    let actions = div()
        .flex()
        .flex_wrap()
        .flex_shrink_0()
        .gap(px(10.))
        .items_center()
        .child(scan_button)
        .child(power);

    card(cx)
        .p(px(22.))
        .child(
            div()
                .flex()
                .gap(px(18.))
                .when(wide, |el| el.items_center().justify_between())
                .when(!wide, |el| el.flex_col())
                .child(identity)
                .child(actions),
        )
}

/// The entries of the server's "…" menu: the power actions, each behind
/// its question.
pub(crate) fn power_menu(name: &str, can_restart: bool, cx: &Context<Bloom>) -> Vec<crate::ui::menu::MenuItem> {
    use crate::ui::menu::MenuItem;
    let handle = cx.weak_entity();
    let mut items = Vec::new();
    if can_restart {
        let (handle, name) = (handle.clone(), name.to_string());
        items.push(
            MenuItem::new("admin.dash.restart", "Restart server…")
                .icon(LucideIcon::RotateCw)
                .detail("Stops all playback for a minute")
                .on_click(move |_, _, cx| {
                    handle.update(cx, |this, cx| ask_power(this, &name, true, cx)).ok();
                }),
        );
    }
    let name = name.to_string();
    items.push(
        MenuItem::new("admin.dash.shutdown", "Shut down server…")
            .icon(LucideIcon::Power)
            .detail("Must be started again on the host")
            .on_click(move |_, _, cx| {
                handle.update(cx, |this, cx| ask_power(this, &name, false, cx)).ok();
            }),
    );
    items
}

/// Asks before the server restarts (`restart`) or shuts down.
fn ask_power(app: &mut Bloom, name: &str, restart: bool, cx: &mut Context<Bloom>) {
    let confirm = if restart {
        Confirm {
            title: "Restart the server?".to_string(),
            message: format!("{name} stops all playback and is not available until it starts again."),
            action: "Restart".to_string(),
            danger: true,
            run: Rc::new(|this, cx| {
                this.admin_action("The server restarts", cx, |client| client.restart_server())
            }),
        }
    } else {
        Confirm {
            title: "Shut down the server?".to_string(),
            message: format!("{name} stops all playback. You must start it again by hand on the host."),
            action: "Shut Down".to_string(),
            danger: true,
            run: Rc::new(|this, cx| {
                this.admin_action("The server shuts down", cx, |client| client.shutdown_server())
            }),
        }
    };
    app.ask_confirm(confirm, cx);
}

// ----- counts -----------------------------------------------------------------

fn count_tiles(data: &Data, width: f32, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let c = &data.counts;
    let streams = data
        .live
        .sessions
        .iter()
        .filter(|s| s.now_playing_item.is_some())
        .count() as u64;
    // The main kinds always show; the others only when the server has some.
    let all = [
        (LucideIcon::Film, "Movies", c.movie_count, true),
        (LucideIcon::Tv, "Series", c.series_count, true),
        (LucideIcon::Clapperboard, "Episodes", c.episode_count, true),
        (LucideIcon::Layers, "Collections", c.box_set_count, false),
        (LucideIcon::Disc3, "Albums", c.album_count, false),
        (LucideIcon::Music, "Songs", c.song_count, false),
        (LucideIcon::CirclePlay, "Music Videos", c.music_video_count, false),
        (LucideIcon::Library, "Books", c.book_count, false),
        (LucideIcon::MonitorPlay, "Trailers", c.trailer_count, false),
        (LucideIcon::Activity, "Active Streams", streams, true),
    ];
    let tiles: Vec<_> = all.into_iter().filter(|t| t.3 || t.2 > 0).collect();
    // A phone fits two tiles a row.
    let tile_min = if crate::views::shell::compact_now() { 150. } else { 190. };
    let columns = ((width + GAP) / (tile_min + GAP)).floor().clamp(1., tiles.len() as f32);
    let tile_w = (width - GAP * (columns - 1.)) / columns - 0.5;

    div()
        .flex()
        .flex_wrap()
        .gap(px(GAP))
        .children(tiles.into_iter().map(|(glyph, label, count, _)| {
            card(cx)
                .w(px(tile_w))
                // A set least and most width spares a layout pass (`Bloom::render_settings`).
                .min_w(px(tile_w))
                .max_w(px(tile_w))
                .flex_row()
                .items_center()
                .gap(px(14.))
                .child(icon_disc(glyph, None, 44.))
                .child(
                    div()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(28.))
                                .line_height(px(32.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child(bloom_core::subtitles::thousands(count as i64)),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(t.colors.foreground.opacity(0.6))
                                .child(label),
                        ),
                )
        }))
}

// ----- sessions ---------------------------------------------------------------

fn sessions(app: &Bloom, sessions: &[Session], width: f32, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let playing = sessions
        .iter()
        .filter(|s| s.now_playing_item.is_some())
        .count();
    let heading = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .text_size(px(18.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child("Active Devices"),
        )
        .when(playing > 0, |el| {
            el.child(badge(format!("{playing} playing"), Some(State::Ok)))
        });
    let Some(client) = app.session.as_ref().map(|s| s.client.clone()) else {
        return div();
    };
    if sessions.is_empty() {
        return div().flex().flex_col().gap(px(12.)).child(heading).child(
            card(cx)
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child("No device was active in the last 16 minutes."),
        );
    }
    let columns = ((width + GAP) / (380. + GAP)).floor().max(1.);
    let card_w = (width - GAP * (columns - 1.)) / columns - 0.5;
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(heading)
        // The sessions that play come first (`refresh` sorts them), in a grid
        // of their own, because their cards are taller.
        .children([true, false].into_iter().filter_map(|playing| {
            let cards: Vec<_> = sessions
                .iter()
                .filter(|s| s.now_playing_item.is_some() == playing)
                .map(|session| session_card(session, &client, card_w, cx))
                .collect();
            (!cards.is_empty()).then(|| div().flex().flex_wrap().gap(px(GAP)).children(cards))
        }))
}

fn session_card(
    session: &Session,
    client: &Client,
    width: f32,
    cx: &mut Context<Bloom>,
) -> gpui_kit::AnyElement {
    let t = UiTheme::read(cx).clone();
    let user = session.user_name.clone().unwrap_or_default();
    let image = match (&session.user_id, &session.user_primary_image_tag) {
        (Some(id), Some(tag)) => Some(client.user_image_url(id, tag)),
        _ => None,
    };
    let avatar = avatar(&user, image, 36.);
    let own = session.own;
    let device = format!(
        "{} {} · {}",
        session.client, session.application_version, session.device_name
    );
    let head = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(avatar)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground)
                        .truncate()
                        .child(user),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(t.colors.foreground.opacity(0.7))
                        .child(icon(
                            device_glyph(&session.client, &session.device_name),
                            13.,
                            t.colors.foreground.opacity(0.7),
                        ))
                        .child(div().min_w_0().truncate().child(device)),
                ),
        )
        // The session of the app that shows this page.
        .when(own, |el| el.child(div().flex_shrink_0().child(badge("This app", None))));
    let base = div()
        .id(SharedString::from(format!("admin.dash.session.{}", session.id)))
        .relative()
        .w(px(width))
        // A set least and most width spares a layout pass (`Bloom::render_settings`).
        .min_w(px(width))
        .max_w(px(width))
        .h(px(if session.now_playing_item.is_some() {
            SESSION_H
        } else {
            IDLE_H
        }))
        .map(super::surface)
        .overflow_hidden();

    let Some(item) = &session.now_playing_item else {
        // A device that is connected and plays nothing.
        return base
            .p(px(16.))
            .flex()
            .flex_col()
            .justify_between()
            .child(head)
            .child(
                div()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(t.colors.foreground.opacity(0.6))
                    .child(idle_line(session)),
            )
            .into_any_element();
    };

    let runtime = item.run_time_ticks.unwrap_or(0) / 10_000_000;
    let position = session.play_state.position_ticks.unwrap_or(0) / 10_000_000;
    let progress = if runtime > 0 {
        (position as f32 / runtime as f32).clamp(0., 1.)
    } else {
        0.
    };
    let transcode = session
        .transcoding_info
        .as_ref()
        .filter(|_| session.play_state.play_method.as_deref() == Some("Transcode"));
    let method = match (session.play_state.play_method.as_deref(), transcode) {
        (_, Some(info)) if info.is_video_direct => badge("Remux", None),
        (_, Some(_)) => badge("Transcode", Some(State::Warn)),
        (Some("DirectStream"), _) => badge("Direct Stream", None),
        _ => badge("Direct Play", Some(State::Ok)),
    };
    let title = item.series_name.clone().unwrap_or_else(|| item.name.clone());
    let subtitle = match (&item.series_name, item.episode_code()) {
        (Some(_), Some(code)) => format!("{code} · {}", item.name),
        (Some(_), None) => item.name.clone(),
        _ => item.production_year.map(|y| y.to_string()).unwrap_or_default(),
    };
    let item_id = item.id.clone();

    base.cursor_pointer()
        // The artwork at the size it is drawn on a 2x display.
        .when_some(item.wide_url(client, (width * 2.) as u32), |el, url| {
            el.child(
                crate::images::remote_with(url, px(23.), ObjectFit::Cover)
                    .absolute()
                    .inset_0(),
            )
        })
        // Dark from the bottom and a little from the top, so the text reads.
        .child(div().absolute().inset_0().rounded(px(23.)).bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x0a0a0ab3), 0.),
            linear_color_stop(rgba(0x0a0a0af2), 0.8),
        )))
        .child(
            div()
                .absolute()
                .inset_0()
                .p(px(16.))
                .flex()
                .flex_col()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(10.))
                        .child(head.flex_1().min_w_0())
                        .when(session.play_state.is_paused, |el| {
                            el.child(badge("Paused", None))
                        })
                        .child(method),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(18.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .truncate()
                                .child(title),
                        )
                        .when(!subtitle.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(t.colors.foreground.opacity(0.75))
                                    .truncate()
                                    .child(subtitle),
                            )
                        })
                        .when_some(transcode, |el, info| {
                            el.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(t.colors.foreground.opacity(0.6))
                                    .truncate()
                                    .child(transcode_line(info)),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .text_size(px(12.))
                                .text_color(t.colors.foreground.opacity(0.75))
                                .child(icon(
                                    if session.play_state.is_paused {
                                        LucideIcon::Pause
                                    } else {
                                        LucideIcon::Play
                                    },
                                    12.,
                                    t.colors.foreground,
                                ))
                                .child(format_duration(position))
                                .child(bar(progress, t.colors.foreground).flex_1())
                                .child(format_duration(runtime)),
                        ),
                ),
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.open_item_id(&item_id, cx)
        }))
        .into_any_element()
}

/// The line of a device that plays nothing: when it was last seen, its
/// address, and the other sessions of the same device that the row holds.
fn idle_line(session: &Session) -> String {
    let mut parts = vec![format!("Idle · last seen {}", ago(&session.last_activity_date))];
    // A device without a session has no address.
    if !session.remote_end_point.is_empty() {
        parts.push(session.remote_end_point.clone());
    }
    match session.others {
        0 => {}
        1 => parts.push("1 more session on this device".to_string()),
        n => parts.push(format!("{n} more sessions on this device")),
    }
    parts.join(" · ")
}

/// What a transcode does and why: "Video h264 1080p · Audio aac · 8.0 Mbps · …".
fn transcode_line(info: &Transcoding) -> String {
    let reasons = transcode_reasons(info);
    let what = transcode_what(info);
    if reasons.is_empty() { what } else { format!("{what} · {}", reasons.join(", ")) }
}

/// What a transcode makes: "Video H264 1080p · Audio AAC · TS · 8.0 Mbps".
fn transcode_what(info: &Transcoding) -> String {
    let mut parts: Vec<String> = Vec::new();
    let codec = |direct: bool, codec: &Option<String>| match (direct, codec) {
        (true, _) => "direct".to_string(),
        (false, Some(codec)) => codec.to_uppercase(),
        (false, None) => "convert".to_string(),
    };
    let mut video = format!("Video {}", codec(info.is_video_direct, &info.video_codec));
    if let (false, Some(height)) = (info.is_video_direct, info.height) {
        video.push_str(&format!(" {height}p"));
    }
    parts.push(video);
    parts.push(format!("Audio {}", codec(info.is_audio_direct, &info.audio_codec)));
    if let Some(container) = &info.container {
        parts.push(container.to_uppercase());
    }
    if let Some(bitrate) = info.bitrate.filter(|b| *b > 0) {
        parts.push(format!("{:.1} Mbps", bitrate as f64 / 1_000_000.));
    }
    if let Some(fps) = info.framerate.filter(|f| *f > 0.) {
        parts.push(format!("{fps:.0} fps"));
    }
    if let Some(hw) = info.hardware_acceleration_type.as_deref()
        && !hw.is_empty()
        && hw != "none"
    {
        parts.push(format!("{hw} hardware"));
    }
    if let Some(done) = info.completion_percentage.filter(|d| *d > 0.) {
        parts.push(format!("{done:.0}% converted"));
    }
    parts.join(" · ")
}

/// Why the server transcodes, in words: "Video codec not supported".
fn transcode_reasons(info: &Transcoding) -> Vec<String> {
    // The reasons are a list of names on new servers, one text on old ones.
    match &info.transcode_reasons {
        serde_json::Value::Array(list) => list
            .iter()
            .filter_map(|r| r.as_str())
            .map(plugin_config::label)
            .collect(),
        serde_json::Value::String(text) => text.split(',').map(|r| plugin_config::label(r.trim())).collect(),
        _ => Vec::new(),
    }
}

fn device_glyph(client: &str, device: &str) -> LucideIcon {
    let name = format!("{client} {device}").to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["android tv", "tvos", "apple tv", "roku", "webos", "tizen", "kodi", "fire", "shield"]) {
        LucideIcon::Tv
    } else if has(&["ipad", "tablet"]) {
        LucideIcon::Tablet
    } else if has(&["iphone", "ios", "android", "phone", "findroid", "streamyfin"]) {
        LucideIcon::Smartphone
    } else if has(&["web", "chrome", "firefox", "safari", "edge"]) {
        LucideIcon::Globe
    } else {
        LucideIcon::Monitor
    }
}

// ----- tasks, activity --------------------------------------------------------

/// What the server does at this moment: the tasks that run (a library
/// scan among them) and the transcodes, with what, for whom and why.
fn now(live: &Live, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let quiet = t.colors.foreground.opacity(0.6);
    let mut list = panel("Now", cx).gap(px(12.));
    let transcodes: Vec<(&Session, &Item, &Transcoding)> = live
        .sessions
        .iter()
        .filter(|s| s.play_state.play_method.as_deref() == Some("Transcode"))
        .filter_map(|s| Some((s, s.now_playing_item.as_ref()?, s.transcoding_info.as_ref()?)))
        .collect();
    if live.running.is_empty() && transcodes.is_empty() {
        return list.child(
            div()
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child("The server is idle. No scan, task or transcode runs."),
        );
    }
    for task in &live.running {
        let percent = task.current_progress_percentage.unwrap_or(0.);
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .text_size(px(14.))
                .text_color(t.colors.foreground)
                .child(icon_disc(LucideIcon::RefreshCw, None, 30.))
                .child(div().flex_1().min_w_0().truncate().child(task.name.clone()))
                .child(bar(percent as f32 / 100., tone::INK).w(px(110.)).flex_shrink_0())
                .child(
                    div()
                        .w(px(64.))
                        .flex_shrink_0()
                        .text_color(quiet)
                        .child(if task.state == super::tasks::TaskState::Cancelling {
                            "Stopping…".to_string()
                        } else {
                            format!("{percent:.0}%")
                        }),
                ),
        );
    }
    for (session, item, info) in transcodes {
        let title = match (&item.series_name, item.episode_code()) {
            (Some(series), Some(code)) => format!("{series} {code}"),
            (Some(series), None) => format!("{series}: {}", item.name),
            _ => item.name.clone(),
        };
        let user = session.user_name.clone().unwrap_or_default();
        let reasons = transcode_reasons(info);
        let why = if reasons.is_empty() {
            "The server gave no reason.".to_string()
        } else {
            format!("Why: {}", reasons.join(", "))
        };
        let line = |text: String, color| div().text_size(px(12.)).text_color(color).child(text);
        list = list.child(
            div()
                .flex()
                .items_start()
                .gap(px(12.))
                .child(icon_disc(LucideIcon::Cpu, Some(State::Warn), 30.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(14.))
                                .text_color(t.colors.foreground)
                                .child(format!("Transcoding {title} for {user}")),
                        )
                        .child(line(format!("On {} · {}", session.device_name, transcode_what(info)), quiet))
                        .child(line(why, quiet)),
                ),
        );
    }
    list
}

/// The items added in the last days: the count, and the newest few.
fn recently_added(recent: &[Recent], cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let now = jiff::Timestamp::now().as_second();
    let fresh = recent
        .iter()
        .take_while(|item| {
            parse_date(&item.date_created).is_some_and(|at| now - at.as_second() <= RECENT_DAYS * 86_400)
        })
        .count();
    let count = if fresh >= RECENT_LIMIT { format!("{RECENT_LIMIT}+") } else { fresh.to_string() };
    let summary = match fresh {
        0 if recent.is_empty() => "The libraries have no items yet.".to_string(),
        0 => format!("Nothing new in the last {RECENT_DAYS} days. The newest:"),
        1 => format!("1 item in the last {RECENT_DAYS} days."),
        _ => format!("{count} items in the last {RECENT_DAYS} days."),
    };
    let shown = if fresh == 0 { 3 } else { RECENT_SHOWN.min(fresh) };
    let mut list = panel("Recently added", cx).child(
        div()
            .pb(px(4.))
            .text_size(px(14.))
            .text_color(t.colors.foreground.opacity(0.7))
            .child(summary),
    );
    for (at, item) in recent.iter().take(shown).enumerate() {
        let (glyph, detail) = match item.kind.as_str() {
            "Episode" => {
                let code = match (item.parent_index_number, item.index_number) {
                    (Some(season), Some(episode)) => format!("S{season:02}E{episode:02} · "),
                    _ => String::new(),
                };
                (LucideIcon::Tv, format!("{code}{}", item.name))
            }
            "MusicAlbum" => (LucideIcon::Disc3, "Album".to_string()),
            "Book" => (LucideIcon::Library, "Book".to_string()),
            "MusicVideo" => (LucideIcon::CirclePlay, "Music video".to_string()),
            _ => (
                LucideIcon::Film,
                item.production_year.map_or_else(|| "Film".to_string(), |year| format!("Film · {year}")),
            ),
        };
        let title = item.series_name.clone().unwrap_or_else(|| item.name.clone());
        let id = item.id.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("admin.dash.recent.{at}")))
                .py(px(7.))
                .px(px(8.))
                .mx(px(-8.))
                .rounded(px(12.))
                .cursor_pointer()
                .hover(|s| s.bg(tone::HOVER))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(icon_disc(glyph, None, 30.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_size(px(14.)).text_color(t.colors.foreground).child(title))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(t.colors.foreground.opacity(0.6))
                                .child(detail),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(t.colors.foreground.opacity(0.55))
                        .child(ago(&item.date_created)),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_item_id(&id, cx))),
        );
    }
    list
}

/// The alerts, repeated ones as one line ("× 8") that opens to the list.
fn alerts(app: &Bloom, groups: &[AlertGroup], cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let mut list = panel("Alerts", cx);
    if groups.is_empty() {
        return list.child(div().text_size(px(14.)).text_color(t.colors.muted_foreground).child("No alerts."));
    }
    let opened = app.admin_data().map(|data| &data.alerts_open);
    for (i, group) in groups.iter().enumerate() {
        let first = &group.entries[0];
        let times = group.entries.len();
        let key = group.key();
        let open = times > 1 && opened.is_some_and(|set| set.contains(&key));
        let (glyph, state) = first.look();
        let overview = if times > 1 {
            let oldest = group.entries.last().map(|e| ago(&e.date)).unwrap_or_default();
            Some(format!("{times} times, the first {oldest}"))
        } else {
            first.short_overview.clone().filter(|o| !o.is_empty())
        };
        let mut row = div()
            .id(SharedString::from(format!("admin.dash.alert.{i}")))
            .py(px(9.))
            .flex()
            .items_center()
            .gap(px(12.))
            .child(icon_disc(glyph, state, 30.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .text_size(px(14.))
                            .text_color(t.colors.foreground)
                            .child(div().min_w_0().child(first.name.clone()))
                            .when(times > 1, |el| {
                                el.child(div().flex_shrink_0().child(badge(format!("× {times}"), None)))
                            }),
                    )
                    .when_some(overview, |el, overview| {
                        el.child(
                            div()
                                .text_size(px(12.))
                                .text_color(t.colors.foreground.opacity(0.6))
                                .child(overview),
                        )
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .text_color(t.colors.foreground.opacity(0.55))
                    .child(ago(&first.date)),
            );
        if times > 1 {
            row = row
                .cursor_pointer()
                .child(icon(
                    if open { LucideIcon::ChevronUp } else { LucideIcon::ChevronDown },
                    16.,
                    t.colors.foreground.opacity(0.7),
                ))
                .tooltip(crate::ui::tip::tip(if open { "Hide the list" } else { "Show each one" }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(data) = this.admin_data_mut()
                        && !data.alerts_open.remove(&key)
                    {
                        data.alerts_open.insert(key.clone());
                    }
                    cx.notify();
                }));
        }
        let mut item = div()
            .when(i > 0, |el| el.border_t_1().border_color(rgba(0xffffff14)))
            .child(row);
        if open {
            item = item.child(div().pl(px(42.)).pb(px(8.)).flex().flex_col().gap(px(4.)).children(
                group.entries.iter().map(|entry| {
                    div()
                        .flex()
                        .gap(px(12.))
                        .text_size(px(12.))
                        .text_color(t.colors.foreground.opacity(0.7))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(entry.short_overview.clone().filter(|o| !o.is_empty()).unwrap_or_else(|| entry.name.clone())),
                        )
                        .child(div().flex_shrink_0().text_color(t.colors.foreground.opacity(0.55)).child(ago(&entry.date)))
                }),
            ));
        }
        list = list.child(item);
    }
    list
}

fn entries(title: &'static str, entries: &[Entry], empty: &'static str, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let mut list = panel(title, cx);
    if entries.is_empty() {
        return list.child(
            div()
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child(empty),
        );
    }
    for (i, entry) in entries.iter().enumerate() {
        let (glyph, state) = entry.look();
        list = list.child(
            div()
                .py(px(9.))
                .when(i > 0, |el| el.border_t_1().border_color(rgba(0xffffff14)))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(icon_disc(glyph, state, 30.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(14.))
                                .text_color(t.colors.foreground)
                                .child(entry.name.clone()),
                        )
                        .when_some(
                            entry.short_overview.clone().filter(|o| !o.is_empty()),
                            |el, overview| {
                                el.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(t.colors.foreground.opacity(0.6))
                                        .child(overview),
                                )
                            },
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(t.colors.foreground.opacity(0.55))
                        .child(ago(&entry.date)),
                ),
        );
    }
    list
}

// ----- storage, paths ---------------------------------------------------------

/// One disk, with the folders of the server that live on it.
struct Disk {
    labels: Vec<String>,
    free: i64,
    used: i64,
    kind: String,
}

fn storage(storage: &Storage, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    // Folders with the same free and used space are on the same disk.
    let mut disks: Vec<Disk> = Vec::new();
    let mut add = |label: &str, folder: &Folder| {
        if folder.free_space + folder.used_space <= 0 {
            return;
        }
        match disks
            .iter_mut()
            .find(|d| d.free == folder.free_space && d.used == folder.used_space)
        {
            Some(disk) => {
                if !disk.labels.iter().any(|l| l == label) {
                    disk.labels.push(label.to_string());
                }
            }
            None => disks.push(Disk {
                labels: vec![label.to_string()],
                free: folder.free_space,
                used: folder.used_space,
                kind: folder.storage_type.clone(),
            }),
        }
    };
    // Media first: it is the disk that fills up.
    for library in &storage.libraries {
        for folder in &library.folders {
            // The collections library is a folder of the server's own data.
            // By whole folder names: `/var/lib/jellyfin-media` is not
            // inside `/var/lib/jellyfin`.
            let inside_data = storage
                .program_data_folder
                .as_ref()
                .is_some_and(|data| std::path::Path::new(&folder.path).starts_with(&data.path));
            if !inside_data {
                add(&library.name, folder);
            }
        }
    }
    for (label, folder) in [
        ("Server data", &storage.program_data_folder),
        ("Metadata", &storage.internal_metadata_folder),
        ("Cache", &storage.cache_folder),
        ("Transcodes", &storage.transcoding_temp_folder),
        ("Logs", &storage.log_folder),
    ] {
        if let Some(folder) = folder {
            add(label, folder);
        }
    }

    let mut list = panel("Storage", cx).gap(px(16.));
    if disks.is_empty() {
        return list.child(
            div()
                .text_size(px(14.))
                .text_color(t.colors.muted_foreground)
                .child("This server does not report disk space."),
        );
    }
    for disk in disks {
        let total = (disk.free + disk.used) as f64;
        let full = (disk.used as f64 / total) as f32;
        let tint = match full {
            // Colour only when the disk is nearly full.
            f if f >= 0.95 => rgb(0xff7b72),
            f if f >= 0.85 => rgb(0xe3b341),
            _ => tone::INK,
        };
        list = list.child(
            div()
                .flex()
                .flex_col()
                .gap(px(7.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(icon(LucideIcon::HardDrive, 16., t.colors.foreground.opacity(0.8)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(15.))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(t.colors.foreground)
                                .child(disk.labels.join(", ")),
                        )
                        .when(disk.kind == "Network", |el| {
                            el.child(badge("Network", None))
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(13.))
                                .text_color(t.colors.foreground.opacity(0.7))
                                .child(format!("{:.0}%", full * 100.)),
                        ),
                )
                .child(bar(full, tint).h(px(8.)))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(t.colors.foreground.opacity(0.6))
                        .child(format!(
                            "{} used of {} · {} free",
                            bytes(disk.used),
                            bytes(disk.free + disk.used),
                            bytes(disk.free)
                        )),
                ),
        );
    }
    list
}

fn paths(info: &Info, cx: &mut Context<Bloom>) -> Div {
    let mut list = panel("Paths", cx);
    for (label, path) in [
        ("Server data", &info.program_data_path),
        ("Metadata", &info.internal_metadata_path),
        ("Cache", &info.cache_path),
        ("Transcodes", &info.transcoding_temp_path),
        ("Logs", &info.log_path),
        ("Web client", &info.web_path),
    ] {
        if !path.is_empty() {
            list = list.child(fact(label, path.clone(), cx));
        }
    }
    list
}

// ----- small parts ------------------------------------------------------------

/// A rounded bar filled to `value` (0 to 1).
fn bar(value: f32, color: Rgba) -> Div {
    div()
        .h(px(5.))
        .rounded_full()
        .overflow_hidden()
        .bg(rgba(0xffffff26))
        .child(
            div()
                .h_full()
                .rounded_full()
                .w(relative(value.clamp(0., 1.)))
                .bg(color),
        )
}

/// A size in the unit that fits: "194 GB", "11.7 TB".
fn bytes(n: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n.max(0) as f64;
    let mut unit = 0;
    while value >= 1024. && unit < UNITS.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    if value >= 100. || unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Opens every group of repeated alerts (the debug channel).
pub(crate) fn open_alert_groups(app: &mut Bloom, cx: &mut Context<Bloom>) {
    let Some(data) = app.admin_data_mut() else {
        return;
    };
    let keys: Vec<String> = data
        .dashboard
        .as_ref()
        .map(|d| d.live.alerts.iter().filter(|g| g.entries.len() > 1).map(AlertGroup::key).collect())
        .unwrap_or_default();
    data.alerts_open.extend(keys);
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(kind: &str, name: &str, date: &str) -> Entry {
        Entry { kind: kind.into(), name: name.into(), date: date.into(), ..Default::default() }
    }

    #[test]
    fn repeated_alerts_are_one_group() {
        let failed = "Failed login attempt from bob";
        let mut entries: Vec<Entry> =
            (0..8).map(|i| alert("AuthenticationFailed", failed, &format!("2026-11-18T10:0{i}:00Z"))).collect();
        entries.insert(3, alert("PluginUpdated", "Plugin X was updated", "2026-11-18T10:02:30Z"));
        entries.push(alert("AuthenticationFailed", "Failed login attempt from eve", "2026-11-18T09:00:00Z"));
        let groups = group_alerts(entries, 7);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].entries.len(), 8);
        // The group is where its newest entry is, and keeps the order.
        assert_eq!(groups[0].entries[0].date, "2026-11-18T10:00:00Z");
        assert_eq!(groups[1].entries[0].kind, "PluginUpdated");
        assert_eq!(groups[2].entries[0].name, "Failed login attempt from eve");
        assert_ne!(groups[0].key(), groups[2].key());
        assert_eq!(group_alerts(vec![alert("A", "a", ""), alert("B", "b", "")], 1).len(), 1);
    }

    fn session(device_id: &str, name: &str, date: &str) -> Session {
        Session {
            id: device_id.into(),
            device_id: device_id.into(),
            user_id: Some("u".into()),
            client: "Bloom".into(),
            device_name: name.into(),
            last_activity_date: date.into(),
            ..Default::default()
        }
    }

    #[test]
    fn one_row_for_the_sessions_of_a_device() {
        let sessions = vec![
            session("test-copy", "Mac", "2026-11-18T10:05:00Z"),
            session("own", "Mac", "2026-11-18T10:00:00Z"),
            session("old", "Mac", "2026-11-18T09:00:00Z"),
            session("tv", "Living Room", "2026-11-18T10:00:00Z"),
        ];
        let rows = one_row_per_device(sessions, "own");
        assert_eq!(rows.len(), 2);
        // This app's row stands for the device, newer or not, and counts
        // the other two.
        assert_eq!(rows[0].device_id, "own");
        assert!(rows[0].own);
        assert_eq!(rows[0].others, 2);
        assert_eq!(rows[1].device_name, "Living Room");
        assert!(!rows[1].own);
    }

    #[test]
    fn a_device_that_plays_keeps_its_row() {
        let mut playing = session("b", "Mac", "2026-11-18T10:00:00Z");
        playing.now_playing_item = Some(Item { id: "i".into(), ..Default::default() });
        let mut other_user = session("c", "Mac", "2026-11-18T10:00:00Z");
        other_user.user_id = Some("v".into());
        let rows = one_row_per_device(vec![session("a", "Mac", "2026-11-18T09:00:00Z"), playing, other_user], "x");
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.others == 0));
        // Without this app among them, the newest idle row stands.
        let rows = one_row_per_device(
            vec![session("a", "Mac", "2026-11-18T09:00:00Z"), session("b", "Mac", "2026-11-18T10:00:00Z")],
            "x",
        );
        assert_eq!((rows.len(), rows[0].device_id.as_str(), rows[0].others), (1, "b", 1));
    }
}
