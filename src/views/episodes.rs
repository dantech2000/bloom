// SPDX-License-Identifier: AGPL-3.0-or-later
//! Episode list of the player: the seasons of the series that plays and the
//! episodes of one season, each with its still image. A click plays it.

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled,
    Window, div, point, prelude::FluentBuilder as _, px, relative, rgba,
};

use crate::{
    app::Bloom,
    jellyfin::{Item, format_runtime},
    ui::{glass::glass, scroll_area::ScrollArea, theme::UiTheme},
    views::cards::{artwork, icon},
};

/// Width of the list.
const PICKER_W: f32 = 500.;
/// Size of an episode's still image.
const STILL: (f32, f32) = (160., 90.);
/// Height of one episode row, with the space under it.
const ROW_H: f32 = 106.;

/// State of the episode list.
#[derive(Default)]
pub struct EpisodePicker {
    pub open: bool,
    pub loading: bool,
    pub seasons: Vec<Item>,
    /// Season whose episodes show.
    pub season: Option<String>,
    pub episodes: Vec<Item>,
    pub scroll: ScrollHandle,
}

impl Bloom {
    /// True when the item in the player belongs to a series.
    pub fn can_pick_episode(&self) -> bool {
        self.playing
            .as_ref()
            .is_some_and(|item| item.kind == "Episode" && item.series_id.is_some())
    }

    /// Opens or closes the episode list. It opens at the season of the
    /// episode that plays.
    pub fn toggle_episode_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // One popup at a time: the list takes the place of whatever is open.
        let was_open = self.episode_picker.open;
        self.close_popups(None, window, cx);
        if was_open {
            cx.notify();
            return;
        }
        let Some(playing) = self.playing.clone() else {
            return;
        };
        let Some(series_id) = playing.series_id.clone() else {
            return;
        };
        self.episode_picker.open = true;
        self.episode_picker.season = playing.season_id.clone();
        self.load_picker_episodes(series_id, true, cx);
    }

    /// Shows the episodes of another season.
    fn pick_season(&mut self, season_id: String, cx: &mut Context<Self>) {
        let Some(series_id) = self.playing.as_ref().and_then(|i| i.series_id.clone()) else {
            return;
        };
        self.episode_picker.season = Some(season_id);
        self.load_picker_episodes(series_id, false, cx);
    }

    fn load_picker_episodes(&mut self, series_id: String, seasons: bool, cx: &mut Context<Self>) {
        self.episode_picker.loading = true;
        self.episode_picker.episodes.clear();
        let wanted = self.episode_picker.season.clone();
        self.fetch(
            cx,
            move |client| {
                let all = if seasons {
                    client.seasons(&series_id)?
                } else {
                    Vec::new()
                };
                // An episode without a season id belongs to the first season.
                let season = wanted.or_else(|| all.first().map(|s| s.id.clone()));
                let episodes = match &season {
                    Some(season) => client.episodes(&series_id, season)?,
                    None => Vec::new(),
                };
                Ok((all, season, episodes))
            },
            move |this, result, cx| {
                let picker = &mut this.episode_picker;
                picker.loading = false;
                match result {
                    Ok((all, season, episodes)) => {
                        // An answer for a season the user already left.
                        if !seasons && picker.season != season {
                            return;
                        }
                        if seasons {
                            picker.seasons = all;
                        }
                        picker.season = season;
                        picker.episodes = episodes;
                        // The list opens at the episode that plays.
                        let current = this.playing.as_ref().map(|i| i.id.clone());
                        let row = this
                            .episode_picker
                            .episodes
                            .iter()
                            .position(|ep| Some(&ep.id) == current.as_ref())
                            .unwrap_or(0);
                        let top = (row as f32 - 1.).max(0.) * ROW_H;
                        this.episode_picker
                            .scroll
                            .set_offset(point(px(0.), px(-top)));
                    }
                    Err(err) => this.toast("Could not load the episodes", format!("{err:#}"), cx),
                }
                cx.notify();
            },
        );
    }

    /// Plays an episode of the list; the episodes after it follow.
    fn pick_episode(&mut self, episode: Item, cx: &mut Context<Self>) {
        self.episode_picker.open = false;
        if self.playing.as_ref().is_some_and(|i| i.id == episode.id) {
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
        if !picker.open {
            return None;
        }
        let client = self.session.as_ref()?.client.clone();
        let playing = self.playing.as_ref()?;
        let t = UiTheme::read(cx).clone();
        let soft = rgba(0xf5f5f7b3);

        fn swallow(
            this: &mut Bloom,
            _: &MouseDownEvent,
            window: &mut Window,
            cx: &mut Context<Bloom>,
        ) {
            window.focus(&this.player_focus, cx);
            cx.stop_propagation();
        }

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
                            el.bg(t.colors.primary).text_color(t.colors.primary_foreground)
                        })
                        .when(!selected, |el| {
                            el.bg(rgba(0xffffff1f))
                                .text_color(t.colors.foreground)
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
        let row_w = PICKER_W.min(self.viewport_w - 48.) - 30.;
        let mut rows = div().w(px(row_w)).flex().flex_col();
        for episode in &picker.episodes {
            let current = episode.id == playing.id;
            let still = episode
                .image_tags
                .primary
                .as_deref()
                .map(|tag| client.image_url(&episode.id, "Primary", Some(tag), 400))
                .or_else(|| episode.wide_url(&client, 400));
            let name = match episode.index_number {
                Some(n) => format!("{n}. {}", episode.name),
                None => episode.name.clone(),
            };
            let mut details: Vec<String> = Vec::new();
            if current {
                details.push("Now playing".to_string());
            }
            if let Some(secs) = episode.runtime_secs() {
                details.push(format_runtime(secs));
            }
            let pick = episode.clone();
            rows = rows.child(
                div()
                    .id(SharedString::from(format!("picker.episode.{}", episode.id)))
                    .w(px(row_w))
                    .h(px(ROW_H))
                    .flex_shrink_0()
                    .px(px(8.))
                    .rounded(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .cursor_pointer()
                    .when(current, |el| el.bg(rgba(0xffffff1f)))
                    .hover(|s| s.bg(rgba(0xffffff29)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.pick_episode(pick.clone(), cx)
                    }))
                    .child(
                        artwork(still, STILL.0, STILL.1, px(10.), LucideIcon::Tv, cx)
                            .flex_shrink_0()
                            .when_some(episode.progress(), |el, value| {
                                el.child(
                                    div()
                                        .absolute()
                                        .bottom(px(6.))
                                        .left(px(8.))
                                        .right(px(8.))
                                        .h(px(4.))
                                        .rounded_full()
                                        .overflow_hidden()
                                        .bg(rgba(0x00000080))
                                        .child(
                                            div()
                                                .h_full()
                                                .rounded_full()
                                                .w(relative(value))
                                                .bg(rgba(0xf5f5f7f2)),
                                        ),
                                )
                            })
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
                                        .child(icon(LucideIcon::AudioLines, 26., t.colors.foreground)),
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
                                    .text_color(t.colors.foreground)
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
                    ),
            );
        }

        let list_h = (self.viewport_h - bottom - 190.).clamp(ROW_H * 2., ROW_H * 5.);
        let body = if picker.loading {
            div()
                .h(px(ROW_H))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(14.))
                .text_color(soft)
                .child("Loading…")
        } else if picker.episodes.is_empty() {
            div()
                .h(px(ROW_H))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(14.))
                .text_color(soft)
                .child("This season has no episodes.")
        } else {
            div()
                .h(px(list_h.min(picker.episodes.len() as f32 * ROW_H)))
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
                .right(px(24.))
                .bottom(px(bottom))
                .w(px(PICKER_W.min(self.viewport_w - 48.)))
                .rounded(px(24.))
                .border_1()
                .border_color(rgba(0xf5f5f733))
                .on_mouse_down(MouseButton::Left, cx.listener(swallow))
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
                        .text_color(t.colors.foreground)
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
