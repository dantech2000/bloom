// SPDX-License-Identifier: AGPL-3.0-or-later
//! "Request more seasons" of the Jellyfin Enhanced plugin. On a series page
//! the plugin offers a Seerr request for the seasons that are not on the
//! server. The app reads the series from Seerr through the plugin's proxy
//! (`/JellyfinEnhanced/jellyseerr/tv/{tmdbId}`), compares it with the seasons
//! in the library, and lists them with a status each. The rules are those of
//! `updateSeasonList` and `checkForUnrequestedSeasons` of the plugin.
//!
//! A request starts downloads on the server. Only the button in the dialog
//! sends it (`POST /JellyfinEnhanced/jellyseerr/request`); no debug command
//! does.

use std::collections::HashSet;

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgba,
};
use serde_json::{Value, json};

use crate::{
    app::{Bloom, Page},
    jellyfin::Client,
    ui::{glass::glass, scroll_area::ScrollArea, theme::UiTheme},
    views::cards::icon,
};

/// What Seerr says of a season, as the dialog shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Available,
    Partial,
    Processing,
    Pending,
    Blocked,
    /// Never requested, or a request that was removed: it can be requested.
    Missing,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Available => "Available",
            Status::Partial => "Partly available",
            Status::Processing => "Requested",
            Status::Pending => "Pending approval",
            Status::Blocked => "Blocked",
            Status::Missing => "Missing",
        }
    }
}

/// One season in the dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub number: i64,
    pub name: String,
    pub year: String,
    pub episodes: i64,
    pub status: Status,
}

impl Row {
    /// `isRequestable`: the season has no status, or the status is "unknown"
    /// or "deleted".
    pub fn requestable(&self) -> bool {
        self.status == Status::Missing
    }
}

/// Everything the dialog needs about one series.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    pub tmdb_id: i64,
    pub title: String,
    pub rows: Vec<Row>,
    /// The Seerr server lets a request name some seasons (not all or none).
    pub partial: bool,
}

impl Data {
    /// "Request more" shows when some season can be requested.
    pub fn has_requestable(&self) -> bool {
        self.rows.iter().any(Row::requestable)
    }

    pub fn requestable(&self) -> Vec<i64> {
        self.rows.iter().filter(|r| r.requestable()).map(|r| r.number).collect()
    }
}

/// The list of seasons from the details of Seerr (`/tv/{id}`).
///
/// `library` holds the season numbers the server has, or `None` when the
/// server could not tell. A season that Seerr calls available but the server
/// does not have is "deleted" and can be requested again (`effectiveMediaStatus`).
/// `specials` includes season 0 (a setting of Seerr).
pub fn rows(tv: &Value, library: Option<&HashSet<i64>>, has_library_id: bool, specials: bool) -> Vec<Row> {
    // The status of each season: the media's seasons, then the requests'
    // seasons over them (not the 4K ones).
    let mut status: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let info = tv.get("mediaInfo");
    for season in info.and_then(|m| m.get("seasons")).and_then(Value::as_array).into_iter().flatten() {
        if let (Some(n), Some(s)) = (
            season.get("seasonNumber").and_then(Value::as_i64),
            season.get("status").and_then(Value::as_i64),
        ) {
            status.insert(n, s);
        }
    }
    for request in info.and_then(|m| m.get("requests")).and_then(Value::as_array).into_iter().flatten() {
        if request.get("is4k").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        for season in request.get("seasons").and_then(Value::as_array).into_iter().flatten() {
            if let (Some(n), Some(s)) = (
                season.get("seasonNumber").and_then(Value::as_i64),
                season.get("status").and_then(Value::as_i64),
            ) {
                status.insert(n, s);
            }
        }
    }

    let mut seasons: Vec<&Value> = tv
        .get("seasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.get("episodeCount").and_then(Value::as_i64).unwrap_or(0) > 0)
        .filter(|s| s.get("seasonNumber").and_then(Value::as_i64) != Some(0) || specials)
        .collect();
    seasons.sort_by_key(|s| s.get("seasonNumber").and_then(Value::as_i64).unwrap_or(0));

    seasons
        .into_iter()
        .map(|season| {
            let number = season.get("seasonNumber").and_then(Value::as_i64).unwrap_or(0);
            let raw = status.get(&number).copied();
            // "Available" is stale when the server does not have the season.
            let effective = match raw {
                Some(5) => match library {
                    Some(have) if have.contains(&number) => 5,
                    Some(_) => 7,
                    None if has_library_id => 5,
                    None => 7,
                },
                other => other.unwrap_or(1),
            };
            let name = season.get("name").and_then(Value::as_str).unwrap_or("").trim();
            // A name that is a bare number (TheTVDB) is no name.
            let bare = name.chars().all(|c| c.is_ascii_digit());
            Row {
                number,
                name: if !name.is_empty() && !bare {
                    name.to_string()
                } else if number == 0 {
                    "Specials".to_string()
                } else {
                    format!("Season {number}")
                },
                year: season
                    .get("airDate")
                    .and_then(Value::as_str)
                    .and_then(|d| d.get(..4))
                    .unwrap_or("")
                    .to_string(),
                episodes: season.get("episodeCount").and_then(Value::as_i64).unwrap_or(0),
                status: match effective {
                    5 => Status::Available,
                    4 => Status::Partial,
                    3 => Status::Processing,
                    2 => Status::Pending,
                    6 => Status::Blocked,
                    _ => Status::Missing,
                },
            }
        })
        .collect()
}

/// The body of the request of seasons (`requestTvSeasons` of the plugin):
/// `{ mediaType: "tv", mediaId, seasons: [...] }`.
pub fn request_body(tmdb_id: i64, seasons: &[i64]) -> Value {
    json!({ "mediaType": "tv", "mediaId": tmdb_id, "seasons": seasons })
}

impl Client {
    /// The seasons of a library series against Seerr. `None` when Seerr has
    /// no series for it, or the user has no Seerr account.
    pub fn season_data(&self, series_id: &str) -> anyhow::Result<Option<Data>> {
        if !self.seerr_active() {
            return Ok(None);
        }
        let item = self.item(series_id)?;
        let Some(tmdb) = item.provider_ids.get("Tmdb").cloned().flatten().and_then(|t| t.parse::<i64>().ok())
        else {
            return Ok(None);
        };
        let (tv, library, settings) = std::thread::scope(|scope| {
            let tv = scope.spawn(|| self.get::<Value>(&format!("/JellyfinEnhanced/jellyseerr/tv/{tmdb}"), &[]));
            let library = scope.spawn(|| self.seasons(series_id));
            let settings = self.get::<Value>("/JellyfinEnhanced/jellyseerr/settings/partial-requests", &[]);
            (tv.join().expect("tv thread"), library.join().expect("seasons thread"), settings)
        });
        let tv = tv?;
        let library: Option<HashSet<i64>> = library
            .ok()
            .map(|list| list.iter().filter_map(|s| s.index_number.map(i64::from)).collect());
        let settings = settings.unwrap_or(Value::Null);
        let has_id = tv
            .get("mediaInfo")
            .and_then(|m| m.get("jellyfinMediaId"))
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty());
        Ok(Some(Data {
            tmdb_id: tmdb,
            title: tv
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or(item.name),
            rows: rows(
                &tv,
                library.as_ref(),
                has_id,
                settings.get("enableSpecialEpisodes").and_then(Value::as_bool).unwrap_or(false),
            ),
            partial: settings.get("partialRequestsEnabled").and_then(Value::as_bool).unwrap_or(true),
        }))
    }

    /// Asks Seerr for seasons of a series. This starts downloads.
    pub fn request_seasons(&self, tmdb_id: i64, seasons: &[i64]) -> anyhow::Result<()> {
        self.post("/JellyfinEnhanced/jellyseerr/request", &request_body(tmdb_id, seasons))
            .map(drop)
    }
}

/// State of the dialog and of the check on the open series page.
#[derive(Default)]
pub struct SeasonsState {
    /// Series page whose seasons were checked: item id and data.
    pub probe: Option<(String, Data)>,
    probing: Option<String>,
    pub open: bool,
    pub loading: bool,
    pub series_id: String,
    pub data: Option<Data>,
    pub selected: HashSet<i64>,
    pub sending: bool,
    pub error: Option<String>,
}

impl Bloom {
    /// True when the plugin offers "request more" on this server and the user
    /// has it on.
    pub fn request_more_on(&self) -> bool {
        self.enhanced_feature("request_more")
    }

    /// Checks the open series page for seasons that can be requested.
    pub fn load_request_more(&mut self, cx: &mut Context<Self>) {
        if !self.request_more_on() {
            return;
        }
        let Page::Detail(data) = &self.page else {
            return;
        };
        if !data.item.is_series() {
            return;
        }
        let id = data.item.id.clone();
        let state = &mut self.enhanced.seasons;
        if state.probe.as_ref().is_some_and(|(open, _)| *open == id) || state.probing.as_ref() == Some(&id) {
            return;
        }
        state.probing = Some(id.clone());
        let work_id = id.clone();
        self.fetch(
            cx,
            move |client| client.season_data(&work_id),
            move |this, result, cx| {
                this.enhanced.seasons.probing = None;
                match result {
                    Ok(Some(data)) => {
                        this.enhanced.seasons.probe = Some((id, data));
                        cx.notify();
                    }
                    Ok(None) => {}
                    Err(err) => log::warn!("request more: {err:#}"),
                }
            },
        );
    }

    /// True when the page of this series shows "Request more seasons".
    pub fn can_request_more(&self, series_id: &str) -> bool {
        self.request_more_on()
            && self
                .enhanced
                .seasons
                .probe
                .as_ref()
                .is_some_and(|(id, data)| id == series_id && data.has_requestable())
    }

    /// Opens the dialog for a series of the library.
    pub fn open_seasons_dialog(&mut self, series_id: String, cx: &mut Context<Self>) {
        if !self.request_more_on() {
            return;
        }
        let state = &mut self.enhanced.seasons;
        state.open = true;
        state.loading = true;
        state.series_id = series_id.clone();
        state.error = None;
        state.data = None;
        state.selected.clear();
        cx.notify();
        self.fetch(
            cx,
            {
                let id = series_id.clone();
                move |client| client.season_data(&id)
            },
            move |this, result, cx| {
                let state = &mut this.enhanced.seasons;
                if state.series_id != series_id {
                    return;
                }
                state.loading = false;
                match result {
                    Ok(Some(data)) => {
                        // All seasons that can be requested are ticked.
                        state.selected = data.requestable().into_iter().collect();
                        state.data = Some(data);
                    }
                    Ok(None) => state.error = Some("Seerr has no series for this title.".into()),
                    Err(err) => state.error = Some(format!("{err:#}")),
                }
                cx.notify();
            },
        );
    }

    pub fn close_seasons_dialog(&mut self, cx: &mut Context<Self>) {
        self.enhanced.seasons.open = false;
        cx.notify();
    }

    /// The Request button of the dialog.
    fn send_season_request(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.enhanced.seasons;
        let Some(data) = state.data.clone() else { return };
        let mut seasons: Vec<i64> = state.selected.iter().copied().collect();
        seasons.sort_unstable();
        if seasons.is_empty() || state.sending {
            return;
        }
        state.sending = true;
        let series_id = state.series_id.clone();
        let title = data.title.clone();
        let tmdb = data.tmdb_id;
        let count = seasons.len();
        self.fetch(
            cx,
            move |client| client.request_seasons(tmdb, &seasons),
            move |this, result, cx| {
                this.enhanced.seasons.sending = false;
                match result {
                    Ok(()) => {
                        this.toast("Requested", format!("{title}: {count} season(s)"), cx);
                        this.enhanced.seasons.open = false;
                        // The page and the dialog show the new status.
                        this.enhanced.seasons.probe = None;
                        this.load_request_more(cx);
                    }
                    Err(err) => {
                        this.enhanced.seasons.error = Some(format!("{err:#}"));
                        let _ = series_id;
                    }
                }
                cx.notify();
            },
        );
    }

    /// The dialog over the page.
    pub fn render_seasons_dialog(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let state = &self.enhanced.seasons;
        if !state.open {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let soft = rgba(0xf5f5f7b3);
        let mut list = div().flex().flex_col().gap(px(2.));
        let mut footer_note = state.error.clone();
        if state.loading {
            list = list.child(div().px(px(12.)).py(px(10.)).text_size(px(14.)).text_color(soft).child("Loading…"));
        }
        let partial = state.data.as_ref().is_none_or(|d| d.partial);
        for row in state.data.iter().flat_map(|d| d.rows.iter()) {
            let number = row.number;
            let requestable = row.requestable();
            let checked = state.selected.contains(&number);
            let mut line = div()
                .id(SharedString::from(format!("seasons.row.{number}")))
                .min_h(px(44.))
                .px(px(12.))
                .py(px(6.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(if requestable {
                    let mut tick = crate::settings::checkbox(
                        SharedString::from(format!("seasons.check.{number}")),
                        checked,
                        cx,
                    );
                    if partial {
                        tick = tick.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let selected = &mut this.enhanced.seasons.selected;
                            if !selected.remove(&number) {
                                selected.insert(number);
                            }
                            cx.notify();
                        }));
                    }
                    tick.into_any_element_compat()
                } else {
                    div().size(px(26.)).into_any_element_compat()
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(15.))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(t.colors.foreground)
                                .child(row.name.clone()),
                        )
                        .child(div().text_size(px(12.)).text_color(soft).child(format!(
                            "{} episodes{}",
                            row.episodes,
                            if row.year.is_empty() { String::new() } else { format!(" · {}", row.year) }
                        ))),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(13.))
                        .text_color(if requestable { t.colors.foreground } else { soft })
                        // A colour only for a state: green for what is on the server.
                        .child(div().size(px(8.)).rounded_full().bg(match row.status {
                            Status::Available => rgba(0x7ee787ff),
                            Status::Partial | Status::Processing | Status::Pending => rgba(0xe3b341ff),
                            Status::Blocked => rgba(0xf85149ff),
                            Status::Missing => rgba(0xf5f5f74d),
                        }))
                        .child(row.status.label()),
                );
            if !requestable {
                line = line.opacity(0.75);
            }
            list = list.child(line);
        }
        if state.data.is_some() && state.data.as_ref().is_some_and(|d| d.rows.is_empty()) {
            footer_note = footer_note.or(Some("Seerr lists no seasons for this title.".into()));
        }
        let count = state.selected.len();
        let can_send = count > 0 && !state.sending && state.data.is_some();
        let title = state.data.as_ref().map(|d| d.title.clone()).unwrap_or_default();
        let height = (state.data.as_ref().map_or(1, |d| d.rows.len().max(1)) as f32 * 56.).min(392.);

        Some(
            div()
                .id("seasons.dialog")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_seasons_dialog(cx)))
                .child(
                    div()
                        .id("seasons.card")
                        .relative()
                        .w(px(520.))
                        .max_w_full()
                        .rounded(px(24.))
                        .border_1()
                        .border_color(rgba(0xf5f5f733))
                        // A click in the card must not close the dialog.
                        .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
                        .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
                        .p(px(20.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .text_size(px(20.))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(t.colors.foreground)
                                        .child("Request more seasons"),
                                )
                                .child(div().text_size(px(13.)).text_color(soft).truncate().child(title)),
                        )
                        .child(div().h(px(height)).child(ScrollArea::new("seasons.scroll").size_full().child(list)))
                        .children(footer_note.map(|note| {
                            div().px(px(4.)).text_size(px(13.)).text_color(rgba(0xf85149ff)).child(note)
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .id("seasons.cancel")
                                        .h(px(38.))
                                        .px(px(16.))
                                        .rounded(px(12.))
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0xffffff1f)))
                                        .text_size(px(14.))
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .text_color(t.colors.foreground)
                                        .child("Cancel")
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.close_seasons_dialog(cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .id("seasons.request")
                                        .h(px(38.))
                                        .px(px(16.))
                                        .rounded(px(12.))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .bg(if can_send { t.colors.primary } else { rgba(0xffffff1f).into() })
                                        .when(can_send, |el| el.cursor_pointer())
                                        .text_size(px(14.))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(if can_send { gpui_kit::rgb(0x121212) } else { gpui_kit::Rgba::from(soft) })
                                        .child(icon(
                                            LucideIcon::Download,
                                            16.,
                                            if can_send { gpui_kit::rgb(0x121212) } else { soft },
                                        ))
                                        .child(if state.sending {
                                            "Requesting…".to_string()
                                        } else {
                                            format!("Request {count} season{}", if count == 1 { "" } else { "s" })
                                        })
                                        .when(can_send, |el| {
                                            el.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                this.send_season_request(cx)
                                            }))
                                        }),
                                ),
                        ),
                ),
        )
    }

    /// For the debug channel: the status of each season of a series.
    pub fn seasons_debug(&self, series_id: &str) -> String {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return "error: not signed in".into();
        };
        match client.season_data(series_id) {
            Err(err) => format!("error: {err:#}"),
            Ok(None) => "no Seerr data (no Seerr account, or no TMDB id)".into(),
            Ok(Some(data)) => {
                let rows: Vec<String> = data
                    .rows
                    .iter()
                    .map(|r| {
                        format!(
                            "S{}={}{}",
                            r.number,
                            r.status.label(),
                            if r.requestable() { "*" } else { "" }
                        )
                    })
                    .collect();
                format!(
                    "{} tmdb={} partial={} request_more_shown={} body_for_requestable={} | {}",
                    data.title,
                    data.tmdb_id,
                    data.partial,
                    data.has_requestable(),
                    request_body(data.tmdb_id, &data.requestable()),
                    rows.join(" ")
                )
            }
        }
    }

    /// For the debug channel: the state of the dialog.
    pub fn request_dialog_debug(&self) -> String {
        let s = &self.enhanced.seasons;
        let mut selected: Vec<i64> = s.selected.iter().copied().collect();
        selected.sort_unstable();
        format!(
            "open={} loading={} sending={} selected={selected:?} error={:?} rows={}",
            s.open,
            s.loading,
            s.sending,
            s.error,
            s.data
                .as_ref()
                .map(|d| d
                    .rows
                    .iter()
                    .map(|r| format!("S{}:{}", r.number, r.status.label()))
                    .collect::<Vec<_>>()
                    .join(","))
                .unwrap_or_default()
        )
    }
}

/// `checkbox` answers a `Stateful<Div>` and the other cell a `Div`; both
/// become one element type for the row.
trait IntoAny {
    fn into_any_element_compat(self) -> gpui_kit::AnyElement;
}
impl<T: gpui_kit::IntoElement> IntoAny for T {
    fn into_any_element_compat(self) -> gpui_kit::AnyElement {
        self.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv() -> Value {
        json!({
            "id": 1215, "name": "Show",
            "seasons": [
                { "seasonNumber": 0, "episodeCount": 3, "name": "Specials" },
                { "seasonNumber": 1, "episodeCount": 12, "name": "Season 1", "airDate": "2007-08-13" },
                { "seasonNumber": 2, "episodeCount": 12, "name": "2", "airDate": "2008-09-28" },
                { "seasonNumber": 3, "episodeCount": 12, "name": "Season 3" },
                { "seasonNumber": 4, "episodeCount": 12, "name": "Season 4" },
                { "seasonNumber": 5, "episodeCount": 0, "name": "Season 5" },
                { "seasonNumber": 6, "episodeCount": 10, "name": "Season 6" },
                { "seasonNumber": 7, "episodeCount": 10, "name": "Season 7" },
            ],
            "mediaInfo": {
                "jellyfinMediaId": "abc",
                "seasons": [
                    { "seasonNumber": 1, "status": 5 },
                    { "seasonNumber": 2, "status": 5 },
                    { "seasonNumber": 3, "status": 5 },
                    { "seasonNumber": 4, "status": 1 },
                    { "seasonNumber": 6, "status": 4 },
                ],
                "requests": [
                    { "is4k": false, "seasons": [{ "seasonNumber": 7, "status": 2 }] },
                    { "is4k": true, "seasons": [{ "seasonNumber": 4, "status": 5 }] },
                ]
            }
        })
    }

    #[test]
    fn rows_by_status() {
        let library: HashSet<i64> = [1, 2].into_iter().collect();
        let list = rows(&tv(), Some(&library), true, false);
        let got: Vec<(i64, Status)> = list.iter().map(|r| (r.number, r.status)).collect();
        assert_eq!(
            got,
            [
                (1, Status::Available),
                (2, Status::Available),
                // Seerr says available, the server has no season 3: it can be requested again.
                (3, Status::Missing),
                (4, Status::Missing),
                // Season 5 has no episodes: it is left out.
                (6, Status::Partial),
                (7, Status::Pending),
            ]
        );
        assert_eq!(list[1].name, "Season 2", "a bare number is no name");
        assert_eq!(list[0].year, "2007");
        let data = Data { rows: list, ..Default::default() };
        assert!(data.has_requestable());
        assert_eq!(data.requestable(), [3, 4]);
    }

    #[test]
    fn specials_and_unknown_library() {
        let with = rows(&tv(), None, true, true);
        assert_eq!(with[0].number, 0);
        assert_eq!(with[0].name, "Specials");
        // Without a list of library seasons the show's id decides.
        assert_eq!(with[3].status, Status::Available);
        let without = rows(&tv(), None, false, false);
        assert_eq!(without[2].status, Status::Missing);
    }

    #[test]
    fn nothing_to_request() {
        let all = json!({
            "seasons": [{ "seasonNumber": 1, "episodeCount": 8 }],
            "mediaInfo": { "seasons": [{ "seasonNumber": 1, "status": 5 }] }
        });
        let library: HashSet<i64> = [1].into_iter().collect();
        let data = Data { rows: rows(&all, Some(&library), true, false), ..Default::default() };
        assert!(!data.has_requestable());
    }

    #[test]
    fn the_request_body() {
        assert_eq!(
            request_body(1215, &[3, 4]),
            json!({ "mediaType": "tv", "mediaId": 1215, "seasons": [3, 4] })
        );
        assert_eq!(request_body(5, &[]), json!({ "mediaType": "tv", "mediaId": 5, "seasons": [] }));
    }
}
