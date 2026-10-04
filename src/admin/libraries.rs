// SPDX-License-Identifier: AGPL-3.0-or-later
//! Libraries: a card for each media library with its artwork, item counts,
//! folders and scan state. A library can be scanned alone, or all at once.

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, ObjectFit, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px,
    rgb, rgba,
};
use serde::Deserialize;

use crate::{
    admin::{ButtonKind, badge, button},
    app::Bloom,
    jellyfin::Client,
    ui::theme::UiTheme,
    views::cards::icon,
};

/// Smallest width of a library card; the grid fits as many as the page holds.
const CARD_MIN_W: f32 = 340.;
const CARD_GAP: f32 = 18.;
/// Width of the sidebar plus the page padding at both sides.
const PAGE_CHROME: f32 = 236. + 56.;

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Library {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub locations: Vec<String>,
    /// "movies", "tvshows", "boxsets", "music", ... Absent for mixed content.
    #[serde(default)]
    pub collection_type: Option<String>,
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub primary_image_item_id: Option<String>,
    /// "Idle", "Queued" or "Active".
    #[serde(default)]
    pub refresh_status: Option<String>,
    /// Percent of the scan that is done, while one runs.
    #[serde(default)]
    pub refresh_progress: Option<f64>,
    /// Item counts with their noun, such as (236, "movies").
    #[serde(skip)]
    pub counts: Vec<(u64, &'static str)>,
}

#[derive(PartialEq)]
pub struct Data {
    pub libraries: Vec<Library>,
}

/// The item types counted for a library kind, with the noun for each.
fn counted_types(kind: Option<&str>) -> &'static [(&'static str, &'static str)] {
    match kind {
        Some("movies") => &[("Movie", "movies")],
        Some("tvshows") => &[("Series", "shows"), ("Episode", "episodes")],
        Some("boxsets") => &[("BoxSet", "collections")],
        Some("music") => &[("MusicAlbum", "albums"), ("Audio", "songs")],
        Some("musicvideos") => &[("MusicVideo", "videos")],
        Some("books") => &[("Book", "books")],
        Some("homevideos") => &[("Video", "videos"), ("Photo", "photos")],
        _ => &[("", "items")],
    }
}

pub fn load(client: &Client) -> Result<Data> {
    load_with(client, &[])
}

/// Loads the page again while it is open. The counts of a library are
/// asked for again only while it scans, or when its scan has just ended;
/// the others keep the counts of `previous`.
pub fn refresh(client: &Client, previous: &[Library]) -> Result<Data> {
    load_with(client, previous)
}

fn load_with(client: &Client, previous: &[Library]) -> Result<Data> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Count {
        #[serde(default)]
        total_record_count: u64,
    }
    let idle = |library: &Library| library.refresh_status.as_deref().unwrap_or("Idle") == "Idle";
    let mut libraries: Vec<Library> = client.get("/Library/VirtualFolders", &[])?;
    // The counts are asked for together; one that fails is left out.
    std::thread::scope(|scope| {
        for library in &mut libraries {
            if let Some(known) = previous
                .iter()
                .find(|p| p.item_id == library.item_id && idle(p) && idle(library))
            {
                library.counts = known.counts.clone();
                continue;
            }
            scope.spawn(move || {
                let types = counted_types(library.collection_type.as_deref());
                library.counts = types
                    .iter()
                    .filter_map(|(kind, noun)| {
                        let count: Count = client
                            .get(
                                "/Items",
                                &[
                                    ("ParentId", library.item_id.clone()),
                                    ("Recursive", "true".to_string()),
                                    ("Limit", "0".to_string()),
                                    ("IncludeItemTypes", kind.to_string()),
                                ],
                            )
                            .ok()?;
                        Some((count.total_record_count, *noun))
                    })
                    .collect();
            });
        }
    });
    Ok(Data { libraries })
}

/// Name and glyph of a library kind.
fn kind_label(kind: Option<&str>) -> (&'static str, LucideIcon) {
    match kind {
        Some("movies") => ("Movies", LucideIcon::Clapperboard),
        Some("tvshows") => ("Shows", LucideIcon::Tv),
        Some("boxsets") => ("Collections", LucideIcon::GalleryVerticalEnd),
        Some("music") => ("Music", LucideIcon::Music),
        Some("musicvideos") => ("Music Videos", LucideIcon::Music),
        Some("books") => ("Books", LucideIcon::Library),
        Some("homevideos") => ("Home Videos & Photos", LucideIcon::Image),
        Some("playlists") => ("Playlists", LucideIcon::Library),
        _ => ("Mixed Content", LucideIcon::Folder),
    }
}

/// 4090 as "4,090".
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn render(app: &Bloom, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let t = UiTheme::read(cx).clone();
    let Some(client) = app.session.as_ref().map(|s| s.client.clone()) else {
        return div();
    };
    let content_w = (app.viewport_w - PAGE_CHROME).max(CARD_MIN_W);
    let columns = (((content_w + CARD_GAP) / (CARD_MIN_W + CARD_GAP)).floor() as usize)
        .clamp(1, data.libraries.len().max(1));
    let card_w = ((content_w - CARD_GAP * (columns - 1) as f32) / columns as f32).floor();
    let image_h = (card_w * 9. / 16.).round();

    let folders: usize = data.libraries.iter().map(|l| l.locations.len()).sum();
    let scanning = data
        .libraries
        .iter()
        .filter(|l| l.refresh_status.as_deref().is_some_and(|s| s != "Idle"))
        .count();
    let mut summary = format!(
        "{} {} · {} {}",
        data.libraries.len(),
        if data.libraries.len() == 1 { "library" } else { "libraries" },
        folders,
        if folders == 1 { "folder" } else { "folders" },
    );
    if scanning > 0 {
        summary.push_str(&format!(" · {scanning} scanning"));
    }

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .child(
            div()
                .text_size(px(15.))
                .text_color(t.colors.muted_foreground)
                .child(summary),
        )
        .child(
            button("admin.libraries.scan-all", "Scan All Libraries", ButtonKind::Primary, cx)
                .child(icon(LucideIcon::RotateCw, 16., t.colors.primary_foreground))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.admin_action("Library scan started", cx, |client| {
                        client.call("POST", "/Library/Refresh", &[])
                    })
                })),
        );

    let mut grid = div().flex().flex_wrap().gap(px(CARD_GAP));
    for library in &data.libraries {
        let (kind, glyph) = kind_label(library.collection_type.as_deref());
        let image = library
            .primary_image_item_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| client.image_url(id, "Primary", None, 900));
        let status = library.refresh_status.as_deref().unwrap_or("Idle");
        let busy = status != "Idle";
        let progress = library.refresh_progress.map(|p| (p as f32 / 100.).clamp(0., 1.));

        let counts = library
            .counts
            .iter()
            .map(|(n, noun)| format!("{} {noun}", grouped(*n)))
            .collect::<Vec<_>>()
            .join(" · ");

        let mut paths = div().flex().flex_col().gap(px(6.));
        for path in &library.locations {
            paths = paths.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon(LucideIcon::Folder, 15., t.colors.muted_foreground))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family("Menlo")
                            .text_size(px(13.))
                            .text_color(t.colors.foreground.opacity(0.8))
                            .child(path.clone()),
                    ),
            );
        }

        let item_id = library.item_id.clone();
        let done: &'static str = "Scan started";
        grid = grid.child(
            div()
                .w(px(card_w))
                .rounded(px(24.))
                .border_1()
                .border_color(rgba(0xf5f5f733))
                .bg(rgba(0x2a2a2ab0))
                .overflow_hidden()
                .flex()
                .flex_col()
                // Artwork; the server draws the name of the library into it.
                .child(
                    div()
                        .relative()
                        .w(px(card_w))
                        .h(px(image_h))
                        .bg(t.colors.muted)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(glyph, 44., t.colors.muted_foreground))
                        .when_some(image, |el, url| {
                            el.child(
                                crate::images::remote_with(url, px(0.), ObjectFit::Cover)
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .w(px(card_w))
                                    .h(px(image_h)),
                            )
                        })
                        // Over the artwork the label needs a dark back.
                        .child(div().absolute().top(px(10.)).right(px(10.)).child(
                            badge(
                                if !busy {
                                    "Idle"
                                } else if status == "Queued" {
                                    "Queued"
                                } else {
                                    "Scanning"
                                },
                                rgba(0xffffff2e),
                            )
                            .bg(rgba(0x000000a6)),
                        )),
                )
                // Scan progress, as a line under the artwork.
                .when_some(progress.filter(|_| busy), |el, value| {
                    el.child(
                        div().h(px(4.)).w_full().bg(rgba(0xffffff1f)).child(
                            div()
                                .h_full()
                                .w(gpui_kit::relative(value))
                                .bg(rgb(0x42a5f5)),
                        ),
                    )
                })
                .child(
                    div()
                        .p(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(19.))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(t.colors.foreground)
                                        .child(library.name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .text_size(px(13.))
                                        .text_color(t.colors.muted_foreground)
                                        .child(icon(glyph, 14., t.colors.muted_foreground))
                                        .child(kind),
                                ),
                        )
                        .when(!counts.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_size(px(15.))
                                    .text_color(t.colors.foreground.opacity(0.8))
                                    .child(counts),
                            )
                        })
                        .child(paths)
                        .child(
                            div().flex().child(
                                button(
                                    SharedString::from(format!(
                                        "admin.libraries.scan.{}",
                                        library.item_id
                                    )),
                                    "Scan Library",
                                    ButtonKind::Plain,
                                    cx,
                                )
                                .child(icon(LucideIcon::RotateCw, 15., t.colors.foreground))
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        let item_id = item_id.clone();
                                        this.admin_action(done, cx, move |client| {
                                            client.call(
                                                "POST",
                                                &format!("/Items/{item_id}/Refresh"),
                                                &[
                                                    ("Recursive", "true".to_string()),
                                                    ("ImageRefreshMode", "Default".to_string()),
                                                    (
                                                        "MetadataRefreshMode",
                                                        "Default".to_string(),
                                                    ),
                                                ],
                                            )
                                        })
                                    },
                                )),
                            ),
                        ),
                ),
        );
    }

    div().flex().flex_col().gap(px(18.)).child(header).child(grid)
}
