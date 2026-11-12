// SPDX-License-Identifier: AGPL-3.0-or-later
//! Episode list of the player: the seasons of the series that plays and the
//! episodes of one season, each with its still image. A click plays it.

use crate::ui::tone;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled, Window, div, point, prelude::FluentBuilder as _, px, rgba,
};

use crate::{
    app::{Bloom, Panel},
    jellyfin::{Client, Item, ItemKind, format_runtime},
    ui::{glass::glass, scroll_area::ScrollArea, theme::UiTheme},
    views::cards::{artwork, icon, numbered_name, progress_bar_sized, still_url},
};

/// Width of the list.
const PICKER_W: f32 = 500.;
/// Size of an episode's still image.
const STILL: (f32, f32) = (160., 90.);
/// Height of one episode row, with the space under it.
const ROW_H: f32 = 106.;
/// The still and the row of a phone.
const PHONE_STILL: (f32, f32) = (120., 68.);
const PHONE_ROW_H: f32 = 84.;
/// The quiet text of the list.
const SOFT: u32 = 0xf5f5f7b3;

/// State of the episode list.
#[derive(Default)]
pub struct EpisodePicker {
    pub loading: bool,
    pub seasons: Vec<Item>,
    /// Season whose episodes show.
    pub season: Option<String>,
    pub episodes: Vec<Item>,
    pub scroll: ScrollHandle,
    /// The number of the newest load. An answer of an older one (the
    /// user went on to another season meanwhile) is dropped.
    load: u64,
}

/// The answer of a load: the seasons when the load asked for them, the
/// season the episodes are of, and the episodes.
type Loaded = (Option<Vec<Item>>, Option<String>, Vec<Item>);

impl EpisodePicker {
    /// Starts a load: the list empties and says "Loading…". Returns the
    /// number of the load for `finish_load`.
    fn begin_load(&mut self) -> u64 {
        self.load += 1;
        self.loading = true;
        self.episodes.clear();
        self.load
    }

    /// Ends the load `load` with its answer, or with nothing when it
    /// failed. False for a load that is not the newest: its answer is
    /// dropped and the list goes on to wait for the newest. Before
    /// 2026-11-10 an old answer ended the wait, and the list said "This
    /// season has no episodes." until the right answer came.
    fn finish_load(&mut self, load: u64, answer: Option<Loaded>) -> bool {
        if load != self.load {
            return false;
        }
        self.loading = false;
        if let Some((seasons, season, episodes)) = answer {
            if let Some(seasons) = seasons {
                self.seasons = seasons;
            }
            self.season = season;
            self.episodes = episodes;
        }
        true
    }
}

impl Bloom {
    /// True when the item in the player belongs to a series.
    pub fn can_pick_episode(&self) -> bool {
        self.playback.playing
            .as_ref()
            .is_some_and(|item| item.kind == ItemKind::Episode && item.series_id.is_some())
    }

    /// Opens or closes the episode list. It opens at the season of the
    /// episode that plays.
    pub fn toggle_episode_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let series = self.playback.playing.as_ref().and_then(|item| Some((item.series_id.clone()?, item.season_id.clone())));
        let Some((series_id, season_id)) = series else {
            // Nothing to list: whatever is open closes, as before.
            self.close_popups(None, window, cx);
            return;
        };
        if !self.toggle_panel(Panel::Episodes, window, cx) {
            return;
        }
        self.episode_picker.season = season_id;
        self.load_picker_episodes(series_id, true, cx);
    }

    /// Shows the episodes of another season.
    fn pick_season(&mut self, season_id: String, cx: &mut Context<Self>) {
        let Some(series_id) = self.playback.playing.as_ref().and_then(|i| i.series_id.clone()) else {
            return;
        };
        self.episode_picker.season = Some(season_id);
        self.load_picker_episodes(series_id, false, cx);
    }

    fn load_picker_episodes(&mut self, series_id: String, seasons: bool, cx: &mut Context<Self>) {
        let load = self.episode_picker.begin_load();
        let wanted = self.episode_picker.season.clone();
        self.fetch(
            cx,
            move |client| {
                let all = if seasons { Some(client.seasons(&series_id)?) } else { None };
                // An episode without a season id belongs to the first season.
                let season = wanted
                    .or_else(|| all.as_ref().and_then(|all| all.first().map(|s| s.id.clone())));
                let episodes = match &season {
                    Some(season) => client.episodes(&series_id, season)?,
                    None => Vec::new(),
                };
                Ok((all, season, episodes))
            },
            move |this, result, cx| {
                let (answer, error) = match result {
                    Ok(answer) => (Some(answer), None),
                    Err(err) => (None, Some(err)),
                };
                if !this.episode_picker.finish_load(load, answer) {
                    return;
                }
                match error {
                    Some(err) => this.toast("Could not load the episodes", format!("{err:#}"), cx),
                    None => {
                        // The list opens at the episode that plays.
                        let current = this.playback.playing.as_ref().map(|i| i.id.as_str());
                        let row = this
                            .episode_picker
                            .episodes
                            .iter()
                            .position(|ep| Some(ep.id.as_str()) == current)
                            .unwrap_or(0);
                        let top = (row as f32 - 1.).max(0.) * this.picker_row_h();
                        this.episode_picker
                            .scroll
                            .set_offset(point(px(0.), px(-top)));
                    }
                }
                cx.notify();
            },
        );
    }

    /// Height of a row of the list: a phone has smaller stills.
    fn picker_row_h(&self) -> f32 {
        if self.phone() { PHONE_ROW_H } else { ROW_H }
    }

    /// Plays an episode of the list; the episodes after it follow.
    fn pick_episode(&mut self, episode: Item, cx: &mut Context<Self>) {
        self.close_panel(Panel::Episodes);
        if self.playback.playing.as_ref().is_some_and(|i| i.id == episode.id) {
            cx.notify();
            return;
        }
        if self.cast_play(std::slice::from_ref(&episode), true, cx)
            || self.sync_play(std::slice::from_ref(&episode), true, cx)
        {
            return;
        }
        self.clear_queue();
        self.begin(&episode, true, cx);
        self.queue_followers(&episode, cx);
    }

    /// The episode list over the video, above the right end of the controls.
    pub fn render_episode_picker(&self, bottom: f32, cx: &mut Context<Self>) -> Option<Div> {
        let picker = &self.episode_picker;
        if !self.panel_is(Panel::Episodes) {
            return None;
        }
        self.session.as_ref()?;
        let playing = self.playback.playing.as_ref()?;
        let colors = UiTheme::read(cx).colors;
        let soft = rgba(SOFT);
        // A phone in landscape has little height: the picker is a sheet at
        // the right side from top to bottom, with smaller stills.
        let phone = self.phone();
        let (top_in, _, _, right_in) = crate::views::shell::safe_insets();
        let (still_size, row_h) = if phone { (PHONE_STILL, PHONE_ROW_H) } else { (STILL, ROW_H) };

        let mut seasons = div().flex().flex_wrap().gap(px(6.));
        if picker.seasons.len() > 1 {
            for season in &picker.seasons {
                let selected = picker.season.as_deref() == Some(season.id.as_str());
                let id = season.id.clone();
                seasons = seasons.child(
                    div()
                        .id(SharedString::from(format!("picker.season.{}", season.id)))
                        .h(px(30.))
                        .px(px(12.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .text_size(px(13.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .when(selected, |el| {
                            el.bg(colors.primary).text_color(colors.primary_foreground)
                        })
                        .when(!selected, |el| {
                            el.bg(tone::HOVER)
                                .text_color(colors.foreground)
                                .hover(|s| s.bg(rgba(0xffffff33)))
                        })
                        .child(season.name.clone())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.pick_season(id.clone(), cx)
                        })),
                );
            }
        }

        // The rows take the width of the list, so long text wraps inside it.
        let picker_w = if phone {
            PICKER_W.min(self.viewport_w * 0.62)
        } else {
            PICKER_W.min(self.viewport_w - 48.)
        };
        let row_w = picker_w - 30.;
        // Only the rows in view are built (`ui::rows`): the video
        // redraws the player at each of its frames, and a season can have
        // a hundred episodes.
        let count = picker.episodes.len();
        let rows = div().w(px(row_w)).child(crate::ui::rows::rows(
            cx,
            count,
            row_h,
            move |this, range, cx| {
                let Some(client) = this.session.as_ref().map(|s| s.client.clone()) else {
                    return Vec::new();
                };
                let playing = this.playback.playing.as_ref().map(|item| item.id.as_str());
                let episodes = &this.episode_picker.episodes;
                episodes[range.start.min(episodes.len())..range.end.min(episodes.len())]
                    .iter()
                    .map(|episode| {
                        let current = Some(episode.id.as_str()) == playing;
                        picker_row(episode, current, &client, row_w, row_h, still_size, cx)
                            .into_any_element()
                    })
                    .collect()
            },
        ));

        let list_h = if phone {
            // The sheet from top to bottom: the rows get what the title and
            // the seasons leave.
            let seasons_h = if picker.seasons.len() > 1 { 40. } else { 0. };
            (self.viewport_h - 8. - top_in - bottom - 28. - 34. - seasons_h - 20.).max(row_h)
        } else {
            (self.viewport_h - bottom - 190.).clamp(row_h * 2., row_h * 5.)
        };
        let body = if picker.loading || count == 0 {
            div()
                .h(px(row_h))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(14.))
                .text_color(soft)
                .child(if picker.loading { "Loading…" } else { "This season has no episodes." })
        } else {
            div()
                .h(px(list_h.min(count as f32 * row_h)))
                .child(
                    ScrollArea::new("picker.scroll")
                        .track(&picker.scroll)
                        .size_full()
                        .child(rows),
                )
        };

        Some(
            div()
                .absolute()
                .right(px(if phone { 12. + right_in } else { 24. }))
                // Over the video, not over the control bar.
                .bottom(px(bottom))
                .when(phone, |el| el.top(px(8. + top_in)))
                .w(px(picker_w))
                .rounded(px(24.))
                .border_1()
                .border_color(tone::EDGE)
                .on_mouse_down(MouseButton::Left, cx.listener(Self::swallow_to_player))
                .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
                .p(px(14.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .px(px(8.))
                        .truncate()
                        .text_size(px(17.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(colors.foreground)
                        .child(
                            playing
                                .series_name
                                .clone()
                                .unwrap_or_else(|| "Episodes".to_string()),
                        ),
                )
                .when(picker.seasons.len() > 1, |el| el.child(seasons.px(px(8.))))
                .child(body),
        )
    }
}

/// One episode of the list: its still, its number and name, its length,
/// two lines of its overview. `current` marks the episode that plays.
fn picker_row(
    episode: &Item,
    current: bool,
    client: &Client,
    row_w: f32,
    row_h: f32,
    still: (f32, f32),
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let fg = UiTheme::read(cx).colors.foreground;
    let soft = rgba(SOFT);
    let name = numbered_name(episode);
    let mut details: Vec<String> = Vec::new();
    if current {
        details.push("Now playing".to_string());
    }
    if let Some(secs) = episode.runtime_secs() {
        details.push(format_runtime(secs));
    }
    let pick = episode.clone();
    div()
        .id(SharedString::from(format!("picker.episode.{}", episode.id)))
        .role(gpui_kit::Role::Button)
        .aria_label(name.clone())
        .w(px(row_w))
        .h(px(row_h))
        .flex_shrink_0()
        .px(px(8.))
        .rounded(px(14.))
        .flex()
        .items_center()
        .gap(px(14.))
        .cursor_pointer()
        .when(current, |el| el.bg(tone::HOVER))
        .hover(|s| s.bg(rgba(0xffffff29)))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.pick_episode(pick.clone(), cx)
        }))
        .child(
            artwork(still_url(episode, client, 400), still.0, still.1, px(10.), LucideIcon::Tv, cx)
                .flex_shrink_0()
                .children(
                    episode
                        .progress()
                        .map(|value| progress_bar_sized(value, 8., 6., 4., 0x00000080)),
                )
                .when(current, |el| {
                    el.child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded(px(10.))
                            .bg(rgba(0x00000073))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(LucideIcon::AudioLines, 26., fg)),
                    )
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .truncate()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(name),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(soft)
                        .child(details.join(" · ")),
                )
                .children(episode.overview.clone().map(|overview| {
                    div()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .line_clamp(2)
                        .text_ellipsis()
                        .text_color(soft)
                        .child(overview)
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode(id: &str) -> Item {
        Item { id: id.into(), ..Default::default() }
    }

    /// Season A, then season B before A answered: the answer of A must
    /// neither end the wait nor fill the list.
    #[test]
    fn an_old_answer_does_not_end_the_wait_for_the_newest() {
        let mut picker = EpisodePicker::default();
        let first = picker.begin_load();
        let second = picker.begin_load();
        let old = (None, Some("a".to_string()), vec![episode("a1")]);
        assert!(!picker.finish_load(first, Some(old)));
        assert!(picker.loading, "the list still waits for season b");
        assert!(picker.episodes.is_empty());
        let new = (None, Some("b".to_string()), vec![episode("b1")]);
        assert!(picker.finish_load(second, Some(new)));
        assert!(!picker.loading);
        assert_eq!(picker.season.as_deref(), Some("b"));
        assert_eq!(picker.episodes[0].id, "b1");
    }

    /// The first load brings the seasons too; a season picked before it
    /// answered wins over it, whichever answer comes last.
    #[test]
    fn the_first_load_does_not_undo_a_season_the_user_picked() {
        let mut picker = EpisodePicker::default();
        let open = picker.begin_load();
        picker.season = Some("b".to_string());
        let pick = picker.begin_load();
        assert!(picker.finish_load(pick, Some((None, Some("b".to_string()), vec![episode("b1")]))));
        let late = (Some(vec![episode("a"), episode("b")]), Some("a".to_string()), vec![episode("a1")]);
        assert!(!picker.finish_load(open, Some(late)));
        assert_eq!(picker.season.as_deref(), Some("b"));
        assert_eq!(picker.episodes[0].id, "b1");
    }

    #[test]
    fn a_failed_load_ends_the_wait() {
        let mut picker = EpisodePicker::default();
        let load = picker.begin_load();
        assert!(picker.finish_load(load, None));
        assert!(!picker.loading);
    }
}
