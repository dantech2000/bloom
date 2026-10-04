// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dashboard home: the server and its actions, item counts, who plays what
//! at this moment, tasks that run, recent activity, alerts and storage.
//! The part that changes by itself ([`Live`]) loads again every few seconds.

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
    activity::{PLAYED, collapse},
    ago, badge, button, card, fact, panel, parse_date,
};
use crate::{
    app::Bloom,
    jellyfin::{Client, Item, format_duration},
    ui::theme::UiTheme,
    views::cards::icon,
};

/// Space between two cards.
const GAP: f32 = 16.;
/// A device counts as active when the server heard from it in this time.
const ACTIVE_SECONDS: i64 = 960;
/// Height of a session card.
const SESSION_H: f32 = 196.;
/// Height of the card of a device that plays nothing.
const IDLE_H: f32 = 100.;

const GREEN: u32 = 0x2e9d5b;
const AMBER: u32 = 0xc98a1b;
const RED: u32 = 0xc62828;
const BLUE: u32 = 0x3d7be0;

pub struct Data {
    info: Info,
    counts: Counts,
    storage: Storage,
    pub live: Live,
}

/// What the page loads again while it stays open.
#[derive(Default, PartialEq)]
pub struct Live {
    sessions: Vec<Session>,
    running: Vec<Task>,
    activity: Vec<Entry>,
    alerts: Vec<Entry>,
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
    items: Vec<super::activity::Entry>,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
struct Task {
    name: String,
    state: String,
    key: String,
    current_progress_percentage: Option<f64>,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
struct Entry {
    name: String,
    #[serde(rename = "Type")]
    kind: String,
    date: String,
    severity: String,
    short_overview: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Entries {
    items: Vec<Entry>,
}

pub fn load(client: &Client) -> Result<Data> {
    std::thread::scope(|scope| {
        let info = scope.spawn(|| client.get::<Info>("/System/Info", &[]));
        let counts = scope.spawn(|| client.get::<Counts>("/Items/Counts", &[]));
        let storage = scope.spawn(|| client.get::<Storage>("/System/Info/Storage", &[]));
        let live = refresh(client)?;
        Ok(Data {
            info: info.join().expect("info thread")?,
            counts: counts.join().expect("counts thread").unwrap_or_default(),
            storage: storage.join().expect("storage thread").unwrap_or_default(),
            live,
        })
    })
}

/// Loads only the part that changes while the page is open.
pub fn refresh(client: &Client) -> Result<Live> {
    // The last plays of the users, a start and its stop as one line.
    let played = || {
        client
            .get::<ActivityPage>(
                "/System/ActivityLog/Entries",
                &[("Limit", "30".to_string()), ("HasUserId", "true".to_string())],
            )
            .map(|page| {
                collapse(page.items)
                    .into_iter()
                    .take(7)
                    .map(|e| Entry {
                        name: e.name,
                        kind: e.kind,
                        date: e.date,
                        severity: e.severity,
                        short_overview: e.short_overview,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let entries = |with_user: bool| {
        client
            .get::<Entries>(
                "/System/ActivityLog/Entries",
                &[
                    ("Limit", "7".to_string()),
                    ("HasUserId", with_user.to_string()),
                ],
            )
            .map(|e| e.items)
            .unwrap_or_default()
    };
    std::thread::scope(|scope| {
        let tasks = scope.spawn(|| {
            client.get::<Vec<Task>>("/ScheduledTasks", &[("IsHidden", "false".to_string())])
        });
        let activity = scope.spawn(played);
        let alerts = scope.spawn(|| entries(false));
        let devices = scope.spawn(|| client.get::<Devices>("/Devices", &[]));
        let mut sessions: Vec<Session> = client.get(
            "/Sessions",
            &[("ActiveWithinSeconds", ACTIVE_SECONDS.to_string())],
        )?;
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
        sessions.sort_by(|a, b| {
            (b.now_playing_item.is_some(), &b.last_activity_date)
                .cmp(&(a.now_playing_item.is_some(), &a.last_activity_date))
        });
        let mut running = tasks.join().expect("tasks thread").unwrap_or_default();
        running.retain(|t| t.state != "Idle");
        Ok(Live {
            sessions,
            running,
            activity: activity.join().expect("activity thread"),
            alerts: alerts.join().expect("alerts thread"),
        })
    })
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    // Width of the page content: the window less the sidebar and the padding.
    let width = (app.viewport_w - super::SIDEBAR_W - 56.).max(320.);
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
                // most of the cost of a frame of this page.
                .child(left.w(px(half)).flex_none())
                .child(right.w(px(half)).flex_none())
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
        .when(!live.running.is_empty(), |el| {
            el.child(running_tasks(&live.running, cx))
        })
        .child(sessions(app, &live.sessions, width, cx))
        .child(pair(
            entries("Activity", &live.activity, "No activity yet.", cx),
            entries("Alerts", &live.alerts, "No alerts.", cx),
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
        badge("Shutting down", rgb(RED))
    } else if info.has_pending_restart {
        badge("Restart needed", rgb(AMBER))
    } else if info.has_update_available {
        badge("Update available", rgb(BLUE))
    } else {
        badge("Online", rgb(GREEN))
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
                client.call("POST", "/Library/Refresh", &[])
            })
        })),
    };
    let name = info.server_name.clone();
    let restart = button("admin.dash.restart", "Restart", ButtonKind::Plain, cx).on_click(
        cx.listener({
            let name = name.clone();
            move |this, _: &ClickEvent, _, cx| {
                this.ask_confirm(
                    Confirm {
                        title: "Restart the server?".to_string(),
                        message: format!(
                            "{name} stops all playback and is not available until it starts again."
                        ),
                        action: "Restart".to_string(),
                        danger: true,
                        run: Rc::new(|this, cx| {
                            this.admin_action("The server restarts", cx, |client| {
                                client.call("POST", "/System/Restart", &[])
                            })
                        }),
                    },
                    cx,
                )
            }
        }),
    );
    let shutdown = button("admin.dash.shutdown", "Shut Down", ButtonKind::Danger, cx).on_click(
        cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.ask_confirm(
                Confirm {
                    title: "Shut down the server?".to_string(),
                    message: format!(
                        "{name} stops all playback. You must start it again by hand on the host."
                    ),
                    action: "Shut Down".to_string(),
                    danger: true,
                    run: Rc::new(|this, cx| {
                        this.admin_action("The server shuts down", cx, |client| {
                            client.call("POST", "/System/Shutdown", &[])
                        })
                    }),
                },
                cx,
            )
        }),
    );

    let identity = div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_w_0()
        .child(
            div()
                .size(px(56.))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgba(super::DISC))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(LucideIcon::Server, 28., t.colors.foreground)),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
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
        .child(scan_button)
        .when(info.can_self_restart, |el| el.child(restart))
        .child(shutdown);

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
        (LucideIcon::Film, "Movies", c.movie_count, 0x6ea8ff, true),
        (LucideIcon::Tv, "Series", c.series_count, 0xb38cff, true),
        (LucideIcon::Clapperboard, "Episodes", c.episode_count, 0xff9e6e, true),
        (LucideIcon::Layers, "Collections", c.box_set_count, 0x5fd3a8, false),
        (LucideIcon::Disc3, "Albums", c.album_count, 0xf2c14e, false),
        (LucideIcon::Music, "Songs", c.song_count, 0xf2c14e, false),
        (LucideIcon::CirclePlay, "Music Videos", c.music_video_count, 0xff7eb6, false),
        (LucideIcon::Library, "Books", c.book_count, 0x8fd3ff, false),
        (LucideIcon::MonitorPlay, "Trailers", c.trailer_count, 0xff7eb6, false),
        (LucideIcon::Activity, "Active Streams", streams, 0x5fd38a, true),
    ];
    let tiles: Vec<_> = all.into_iter().filter(|t| t.4 || t.2 > 0).collect();
    let columns = ((width + GAP) / (190. + GAP)).floor().clamp(1., tiles.len() as f32);
    let tile_w = (width - GAP * (columns - 1.)) / columns - 0.5;

    div()
        .flex()
        .flex_wrap()
        .gap(px(GAP))
        .children(tiles.into_iter().map(|(glyph, label, count, _, _)| {
            card(cx)
                .w(px(tile_w))
                .flex_row()
                .items_center()
                .gap(px(14.))
                .child(
                    div()
                        .size(px(44.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(rgba(super::DISC))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(glyph, 21., t.colors.foreground)),
                )
                .child(
                    div()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(28.))
                                .line_height(px(32.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child(grouped(count)),
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
            el.child(badge(format!("{playing} playing"), rgb(GREEN)))
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
    let avatar = match (&session.user_id, &session.user_primary_image_tag) {
        (Some(id), Some(tag)) => div().size(px(36.)).flex_shrink_0().relative().child(
            crate::images::remote_with(client.user_image_url(id, tag), px(18.), ObjectFit::Cover)
                .absolute()
                .inset_0(),
        ),
        _ => div()
            .size(px(36.))
            .flex_shrink_0()
            .rounded_full()
            .bg(rgba(0xffffff2e))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(15.))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(t.colors.foreground)
            .child(user.chars().next().unwrap_or('?').to_uppercase().to_string()),
    };
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
        );
    let base = div()
        .id(SharedString::from(format!("admin.dash.session.{}", session.id)))
        .relative()
        .w(px(width))
        .h(px(if session.now_playing_item.is_some() {
            SESSION_H
        } else {
            IDLE_H
        }))
        .rounded(px(24.))
        .border_1()
        .border_color(rgba(0xf5f5f733))
        .bg(rgba(0x2a2a2ab0))
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
                    // A device without a session has no address.
                    .child(if session.remote_end_point.is_empty() {
                        format!("Idle · last seen {}", ago(&session.last_activity_date))
                    } else {
                        format!(
                            "Idle · last seen {} · {}",
                            ago(&session.last_activity_date),
                            session.remote_end_point
                        )
                    }),
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
        (_, Some(info)) if info.is_video_direct => badge("Remux", rgb(BLUE)),
        (_, Some(_)) => badge("Transcode", rgb(AMBER)),
        (Some("DirectStream"), _) => badge("Direct Stream", rgb(BLUE)),
        _ => badge("Direct Play", rgb(GREEN)),
    };
    let title = match (&item.series_name, item.episode_code()) {
        (Some(series), _) => series.clone(),
        _ => item.name.clone(),
    };
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
                crate::images::remote_with(url, px(16.), ObjectFit::Cover)
                    .absolute()
                    .inset_0(),
            )
        })
        // Dark from the bottom and a little from the top, so the text reads.
        .child(div().absolute().inset_0().rounded(px(16.)).bg(linear_gradient(
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
                            el.child(badge("Paused", rgba(0xffffff40)))
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

/// What a transcode does and why: "Video h264 1080p · Audio aac · 8.0 Mbps · …".
fn transcode_line(info: &Transcoding) -> String {
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
    // The reasons are a list of names on new servers, one text on old ones.
    let reasons: Vec<String> = match &info.transcode_reasons {
        serde_json::Value::Array(list) => list
            .iter()
            .filter_map(|r| r.as_str())
            .map(spaced)
            .collect(),
        serde_json::Value::String(text) => text.split(',').map(|r| spaced(r.trim())).collect(),
        _ => Vec::new(),
    };
    if !reasons.is_empty() {
        parts.push(reasons.join(", "));
    }
    parts.join(" · ")
}

/// "VideoCodecNotSupported" as "Video codec not supported".
fn spaced(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
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

fn running_tasks(tasks: &[Task], cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    panel("Running Tasks", cx)
        .gap(px(10.))
        .children(tasks.iter().map(|task| {
            let percent = task.current_progress_percentage.unwrap_or(0.);
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .text_size(px(14.))
                .text_color(t.colors.foreground)
                .child(icon(LucideIcon::RefreshCw, 15., t.colors.foreground.opacity(0.7)))
                .child(div().w(px(260.)).flex_shrink_0().truncate().child(task.name.clone()))
                .child(bar(percent as f32 / 100., rgb(0xf5f5f7)).flex_1())
                .child(
                    div()
                        .w(px(96.))
                        .flex_shrink_0()
                        .text_color(t.colors.foreground.opacity(0.7))
                        .child(if task.state == "Cancelling" {
                            "Stopping…".to_string()
                        } else {
                            format!("{percent:.0}%")
                        }),
                )
        }))
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
        let (glyph, tint) = entry_glyph(entry);
        list = list.child(
            div()
                .py(px(9.))
                .when(i > 0, |el| el.border_t_1().border_color(rgba(0xffffff14)))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .size(px(30.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(rgba(super::DISC))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(
                            glyph,
                            15.,
                            super::state_color(tint).unwrap_or(t.colors.foreground),
                        )),
                )
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

fn entry_glyph(entry: &Entry) -> (LucideIcon, Rgba) {
    match entry.severity.as_str() {
        "Error" | "Critical" | "Fatal" => return (LucideIcon::TriangleAlert, rgb(0xff6b6b)),
        "Warning" | "Warn" => return (LucideIcon::TriangleAlert, rgb(0xf2c14e)),
        _ => {}
    }
    let kind = entry.kind.as_str();
    if kind == PLAYED {
        (LucideIcon::Play, rgb(0xb0b0b8))
    } else if kind.contains("PlaybackStopped") {
        (LucideIcon::Pause, rgb(0xb0b0b8))
    } else if kind.contains("Playback") {
        (LucideIcon::Play, rgb(0xb0b0b8))
    } else if kind.contains("Failed") || kind.contains("LockedOut") {
        (LucideIcon::ShieldAlert, rgb(0xff6b6b))
    } else if kind.contains("Session") || kind.contains("Authentication") || kind.contains("User") {
        (LucideIcon::User, rgb(0xb0b0b8))
    } else if kind.contains("Task") {
        (LucideIcon::Clock, rgb(0xb0b0b8))
    } else if kind.contains("Plugin") || kind.contains("Package") {
        (LucideIcon::Zap, rgb(0xb0b0b8))
    } else {
        (LucideIcon::Info, rgb(0xb0b0b8))
    }
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
            if !folder.path.starts_with(
                storage
                    .program_data_folder
                    .as_ref()
                    .map_or("\0", |f| f.path.as_str()),
            ) {
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
            _ => rgb(0xf5f5f7),
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
                            el.child(badge("Network", rgba(0xffffff26)))
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

/// 4090 as "4,090".
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
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
