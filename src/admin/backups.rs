// SPDX-License-Identifier: AGPL-3.0-or-later
//! Backups of the server (Jellyfin 10.11 and later): the archives the
//! server keeps, a new one with the parts the user picks, and a restore.
//! A restore replaces the server's data with the archive's and restarts
//! the server, so it asks first in red, after the app read the archive's
//! manifest again. The server sends no size of an archive.

use std::rc::Rc;

use anyhow::Result;
use bloom_core::backups::{Backup, Contents, library_scan_running};
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgba,
};

use super::{
    Confirm, ago, card,
    users::{absolute, action, icon_disc, primary, stat},
};
use crate::{
    app::{Bloom, Page},
    jellyfin::Client,
    ui::{form::checkbox, theme::UiTheme, tone::State},
};

pub struct Data {
    /// The newest first.
    pub backups: Vec<Backup>,
    /// The library scan runs: a backup now holds a library half scanned.
    pub scan_running: bool,
}

pub fn load(client: &Client) -> Result<Data> {
    let (backups, tasks) = std::thread::scope(|scope| {
        let tasks = scope.spawn(|| client.scheduled_tasks::<serde_json::Value>());
        (client.backups(), tasks.join().expect("tasks thread"))
    });
    Ok(Data {
        backups: backups?,
        // The page is useful without the tasks.
        scan_running: tasks.is_ok_and(|tasks| library_scan_running(&tasks)),
    })
}

/// The parts a new backup can add to the database, with what each holds.
const PARTS: [(&str, &str, fn(&mut Contents) -> &mut bool); 3] = [
    ("Metadata", "Pictures and information the server fetched for the library", |c| &mut c.metadata),
    ("Subtitles", "Subtitle files the server downloaded or extracted", |c| &mut c.subtitles),
    ("Trickplay", "The thumbnails of the seek bar; often large", |c| &mut c.trickplay),
];

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let compact = app.compact_w();
    let (contents, creating) = app
        .admin_data()
        .map_or((Contents::new_backup(), false), |d| (d.backup_contents, d.backup_creating));

    // The new backup: the database always, and the parts the user adds.
    let mut new = card(cx)
        .gap(px(12.))
        .child(
            div()
                .text_size(px(17.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child("New backup"),
        )
        .child(
            div()
                .text_size(px(14.))
                .line_height(px(20.))
                .text_color(t.colors.foreground.opacity(0.65))
                .child(
                    "A backup always holds the database: the users, the settings, what each user \
                     watched, and the library. Add the parts below to keep them as well.",
                ),
        );
    for (name, detail, field) in PARTS {
        let mut probe = contents;
        let on = *field(&mut probe);
        new = new.child(
            div()
                .id(SharedString::from(format!("backups.part.{name}")))
                .min_h(px(if compact { 50. } else { 44. }))
                .flex()
                .items_center()
                .gap(px(14.))
                .cursor_pointer()
                // The row takes the click, the box's too.
                .child(checkbox(format!("backups.check.{name}"), on, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(15.)).text_color(t.colors.foreground).child(name))
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(t.colors.foreground.opacity(0.55))
                                .child(detail),
                        ),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| toggle_part(this, field, cx))),
        );
    }
    if data.scan_running {
        new = new.child(warn_line(
            "A library scan is running. A backup made now holds the library as it is halfway through the scan.",
            cx,
        ));
    }
    new = new.child(
        div().flex().child(
            primary(
                "backups.create",
                LucideIcon::DatabaseBackup,
                if creating { "Creating backup…" } else { "Create backup" },
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| ask_create(this, cx))),
        ),
    );

    let mut list = card(cx).p(px(6.));
    if data.backups.is_empty() {
        list = list.child(
            div()
                .p(px(12.))
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child("The server has no backups yet."),
        );
    }
    for (index, backup) in data.backups.iter().enumerate() {
        if index > 0 {
            list = list.child(div().mx(px(12.)).h(px(1.)).bg(rgba(0xf5f5f70f)));
        }
        list = list.child(row(backup, index, compact, cx));
    }

    let folder = data
        .backups
        .first()
        .and_then(|b| b.path.rsplit_once(['/', '\\']).map(|(dir, _)| dir.to_string()));
    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .child(div().flex().flex_wrap().gap(px(12.)).child(stat(LucideIcon::Archive, "Backups", data.backups.len(), cx)))
        .child(new)
        .child(list)
        .child(
            div()
                .text_size(px(13.))
                .line_height(px(19.))
                .text_color(t.colors.foreground.opacity(0.5))
                .child(match folder {
                    Some(folder) => format!(
                        "The server keeps its backups in {folder}. It does not report their size. A \
                         restore replaces everything on the server with the backup and restarts it."
                    ),
                    None => "A restore replaces everything on the server with the backup and restarts it."
                        .to_string(),
                }),
        )
}

/// A line in the amber of "needs a look": the dot carries the colour.
fn warn_line(text: &'static str, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .child(div().mt(px(7.)).size(px(8.)).flex_shrink_0().rounded_full().bg(State::Warn.color()))
        .child(
            div()
                .flex_1()
                .text_size(px(14.))
                .line_height(px(20.))
                .text_color(t.colors.foreground.opacity(0.8))
                .child(text),
        )
}

/// One archive: its date, its server version and its parts, and Restore.
fn row(backup: &Backup, index: usize, compact: bool, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let name = backup.file_name().to_string();
    let detail = format!("Jellyfin {} \u{b7} {}", backup.version(), backup.options.summary());
    div()
        .min_h(px(if compact { 64. } else { 60. }))
        .px(px(12.))
        .py(px(8.))
        .rounded(px(12.))
        .flex()
        .items_center()
        .gap(px(14.))
        .hover(|s| s.bg(rgba(0xffffff0a)))
        .child(icon_disc(LucideIcon::Archive, None, 40.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(t.colors.foreground)
                        .child(format!("{} \u{b7} {}", absolute(&backup.date_created), ago(&backup.date_created))),
                )
                .child(div().text_size(px(13.)).text_color(t.colors.foreground.opacity(0.6)).child(detail))
                .when(!compact, |el| {
                    el.child(
                        div()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(t.colors.foreground.opacity(0.4))
                            .child(name.clone()),
                    )
                }),
        )
        .child(
            action(format!("backups.restore.{index}"), LucideIcon::ArchiveRestore, "Restore", true, cx)
                .role(gpui_kit::Role::Button)
                .aria_label(format!("Restore {name}"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| ask_restore(this, name.clone(), cx))),
        )
}

fn toggle_part(app: &mut Bloom, field: fn(&mut Contents) -> &mut bool, cx: &mut Context<Bloom>) {
    if let Some(data) = app.admin_data_mut() {
        let part = field(&mut data.backup_contents);
        *part = !*part;
        cx.notify();
    }
}

/// Sets a part of the new backup by its name, for the debug channel.
pub(crate) fn set_part(app: &mut Bloom, name: &str, on: bool) -> bool {
    let Some((_, _, field)) = PARTS.iter().find(|(part, _, _)| part.eq_ignore_ascii_case(name)) else {
        return false;
    };
    match app.admin_data_mut() {
        Some(data) => {
            *field(&mut data.backup_contents) = on;
            true
        }
        None => false,
    }
}

/// Makes the backup, after a question when the library scan runs (as
/// Jellyfin Web warns).
pub(crate) fn ask_create(app: &mut Bloom, cx: &mut Context<Bloom>) {
    let Some(data) = app.admin_data() else {
        return;
    };
    if data.backup_creating {
        return;
    }
    let contents = data.backup_contents;
    let scanning = data.backups.as_ref().is_some_and(|d| d.scan_running);
    if scanning {
        app.ask_confirm(
            Confirm {
                title: "Back up during a library scan?".to_string(),
                message: "A library scan is running. The backup will hold the library as it is halfway \
                          through the scan. Wait for the scan to end for a complete backup."
                    .to_string(),
                action: "Back up now".to_string(),
                danger: false,
                run: Rc::new(move |this, cx| create(this, contents, cx)),
            },
            cx,
        );
    } else {
        create(app, contents, cx);
    }
}

fn create(app: &mut Bloom, contents: Contents, cx: &mut Context<Bloom>) {
    if let Some(data) = app.admin_data_mut() {
        data.backup_creating = true;
    }
    cx.notify();
    app.fetch_in_session(
        cx,
        move |client| client.create_backup(&contents),
        |this, result, cx| {
            for data in this.admin_pages_mut() {
                data.backup_creating = false;
            }
            match result {
                Ok(backup) => this.toast("Backup created", backup.options.summary(), cx),
                Err(err) => this.toast("The server could not make the backup", format!("{err:#}"), cx),
            }
            if matches!(this.nav.page, Page::Admin(_)) {
                this.load_page(cx);
            }
            cx.notify();
        },
    );
}

/// Reads the archive's manifest again (it may be gone), then asks in red.
pub(crate) fn ask_restore(app: &mut Bloom, file_name: String, cx: &mut Context<Bloom>) {
    let scanning = app
        .admin_data()
        .and_then(|d| d.backups.as_ref())
        .is_some_and(|d| d.scan_running);
    let name = file_name.clone();
    app.fetch_in_session(
        cx,
        move |client| client.backup_manifest(&name),
        move |this, result, cx| {
            let backup = match result {
                Ok(backup) => backup,
                Err(err) => {
                    this.toast("The server cannot read this backup", format!("{err:#}"), cx);
                    this.load_page(cx);
                    return;
                }
            };
            if !matches!(&this.nav.page, Page::Admin(data) if data.section == super::Section::Backups) {
                return;
            }
            let mut message = format!(
                "The server restarts and replaces all its data with this backup of {} (Jellyfin {}, {}). \
                 Everything that changed since then is lost: users, settings, what each user watched, and \
                 the library. The server is offline until the restore ends. You cannot undo this.",
                absolute(&backup.date_created),
                backup.version(),
                backup.options.summary(),
            );
            if scanning {
                message.push_str(" A library scan is running now; the restart stops it.");
            }
            let name = file_name.clone();
            this.ask_confirm(
                Confirm {
                    title: "Restore this backup?".to_string(),
                    message,
                    action: "Restore and restart".to_string(),
                    danger: true,
                    run: Rc::new(move |this, cx| {
                        let name = name.clone();
                        this.admin_action("The server restarts to restore the backup", cx, move |client| {
                            client.restore_backup(&name)
                        });
                    }),
                },
                cx,
            );
        },
    );
}

/// The backups from the debug channel: `part <name> on|off`, `create`,
/// `restore <index>` (the question only), `restore-now <index>`. A create
/// runs on a local server only (the mock, the container), a restore
/// without the question on the mock only: never on Daniel's server.
pub(crate) fn debug(app: &mut Bloom, words: &str, cx: &mut Context<Bloom>) -> String {
    let mut words = words.split_whitespace();
    let (verb, arg, more) = (words.next().unwrap_or(""), words.next().unwrap_or(""), words.next().unwrap_or(""));
    let Some(session) = app.session.as_ref() else {
        return "error: signed out".into();
    };
    let mock = session.server_id == "mock";
    let base = session.client.base.clone();
    let local = mock || base.contains("127.0.0.1") || base.contains("localhost");
    let file = |app: &Bloom, index: &str| {
        let index: usize = index.parse().ok()?;
        app.admin_data()?.backups.as_ref()?.backups.get(index).map(|b| b.file_name().to_string())
    };
    match verb {
        "part" => {
            if !set_part(app, arg, more == "on") {
                return "error: backups part metadata|subtitles|trickplay on|off (Backups open?)".into();
            }
            cx.notify();
        }
        "create" if !local => return "error: a create from the debug channel runs on a local server only".into(),
        "create" => ask_create(app, cx),
        "restore" => match file(app, arg) {
            Some(name) => ask_restore(app, name, cx),
            None => return "error: no backup with that index (Backups loaded?)".into(),
        },
        "restore-now" if !mock => return "error: restore-now runs on the mock only".into(),
        "restore-now" => match file(app, arg) {
            Some(name) => app.admin_action("The server restarts to restore the backup", cx, move |client| {
                client.restore_backup(&name)
            }),
            None => return "error: no backup with that index".into(),
        },
        _ => return "error: backups part <name> on|off | create | restore <i> | restore-now <i>".into(),
    }
    "ok".into()
}
