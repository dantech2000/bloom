// SPDX-License-Identifier: AGPL-3.0-or-later
//! Home hero: a slideshow of random unwatched titles, after the Media Bar
//! Enhanced plugin of the web client as styled by the Abyss theme.

use std::time::{Duration, Instant};

use gpui_icons::LucideIcon;
use gpui_kit::{
    Animation, AnimationExt as _, ClickEvent, Context, Div, InteractiveElement as _, ObjectFit, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px, relative, rgb, rgba,
    surface,
};

use crate::{
    icons::{Filled, filled},
    app::{HERO_SLIDE, Bloom, Page},
    jellyfin::Item,
    trailer::{TrailerPlayer, best_trailer},
    ui::{glass::glass as frosted, theme::UiTheme},
    video_surface::VideoFrame,
    views::cards::icon,
};
use crate::ui::tip::tip;

/// Time a new slide takes to fade in.
const HERO_FADE: Duration = Duration::from_millis(500);
/// Share of the window height the hero covers.
const HERO_HEIGHT: f32 = 0.90;
/// Share of the window height where the rows below the hero start.
pub const HERO_ROWS_START: f32 = 0.68;
/// Time a trailer may take to start before the slide timer runs anyway.
const TRAILER_GRACE: Duration = Duration::from_secs(5);

/// Where the trailer of the current slide is.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// No trailer: the slide timer runs.
    None,
    /// Requested, no frame yet.
    Loading,
    /// Frames arrive; the slide lasts until the trailer ends.
    Playing,
}

/// Trailer backdrop state of the hero.
pub struct HeroVideo {
    player: TrailerPlayer,
    /// Id of the newest trailer request.
    load: u64,
    /// Slide the trailer was requested for.
    slide: Option<usize>,
    phase: Phase,
    /// Newest frame and its number.
    frame: Option<(u64, VideoFrame)>,
    /// Trailer position as a share of its length.
    progress: f32,
    /// Sound is off until the user turns it on; kept across slides.
    muted: bool,
    /// False while another app is in front; the hero holds then.
    window_active: bool,
    /// The video is paused because the hero is paused or the window is not active.
    held: bool,
    /// Keep playing behind other apps (`BLOOM_HERO_PLAY_INACTIVE`, for tests).
    play_inactive: bool,
    /// Device pixels per point of the window.
    scale: f32,
    last_redraw: Instant,
    _activation: Option<Subscription>,
}

impl Default for HeroVideo {
    fn default() -> Self {
        Self {
            player: TrailerPlayer::default(),
            load: 0,
            slide: None,
            phase: Phase::None,
            frame: None,
            progress: 0.,
            muted: true,
            window_active: true,
            held: false,
            play_inactive: std::env::var_os("BLOOM_HERO_PLAY_INACTIVE").is_some(),
            scale: 2.,
            last_redraw: Instant::now(),
            _activation: None,
        }
    }
}

impl HeroVideo {
    fn stop(&mut self) {
        if self.phase != Phase::None {
            self.player.stop();
        }
        self.phase = Phase::None;
        self.slide = None;
        self.frame = None;
        self.progress = 0.;
        self.held = false;
    }
}

impl Bloom {
    /// Height of the hero for the current window.
    pub fn hero_height(&self) -> f32 {
        (self.viewport_h * HERO_HEIGHT).round()
    }

    /// Time the current slide has been on screen.
    fn hero_elapsed(&self) -> Duration {
        self.hero_paused
            .unwrap_or_else(|| self.hero_since.elapsed())
    }

    pub fn show_hero_slide(&mut self, index: usize) {
        if self.hero.is_empty() {
            return;
        }
        self.hero_video.stop();
        let next = index % self.hero.len();
        if next != self.hero_index {
            self.hero_previous = Some(self.hero_index);
            self.hero_changed = Instant::now();
        }
        self.hero_index = next;
        self.hero_since = Instant::now();
        if self.hero_paused.is_some() {
            self.hero_paused = Some(Duration::ZERO);
        }
    }

    pub fn step_hero(&mut self, forward: bool) {
        let len = self.hero.len();
        if len == 0 {
            return;
        }
        let next = if forward {
            self.hero_index + 1
        } else {
            self.hero_index + len - 1
        };
        self.show_hero_slide(next);
    }

    pub fn toggle_hero_pause(&mut self) {
        match self.hero_paused.take() {
            Some(shown) => self.hero_since = Instant::now() - shown,
            None => self.hero_paused = Some(self.hero_since.elapsed()),
        }
    }

    pub fn toggle_hero_mute(&mut self) {
        self.hero_video.muted = !self.hero_video.muted;
        self.hero_video.player.set_muted(self.hero_video.muted);
    }

    /// Turns trailer backdrops on or off and keeps the choice.
    /// Liquid glass or the frosted glass of before, for every panel.
    pub fn toggle_liquid_glass(&mut self, cx: &mut Context<Self>) {
        let on = !self.config.liquid_glass.unwrap_or(true);
        self.config.liquid_glass = Some(on);
        self.save_config(cx);
        crate::ui::glass::set_liquid(on);
        cx.refresh_windows();
    }

    pub fn toggle_hero_video(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.config.hero_video.unwrap_or(true);
        self.config.hero_video = Some(enabled);
        self.save_config(cx);
        if !enabled && self.hero_video.phase != Phase::None {
            // The slide goes on as an image slide from here.
            self.hero_video.stop();
            self.hero_video.slide = Some(self.hero_index);
            self.hero_since = Instant::now();
        } else {
            self.hero_video.slide = None;
        }
        self.rebuild_menu(cx);
        cx.notify();
    }

    /// True while the hero is on screen.
    fn hero_visible(&self) -> bool {
        matches!(self.page, Page::Home(_))
            && !self.player_open
            && !self.hero.is_empty()
            && -f32::from(self.page_scroll.offset().y) < self.hero_height()
    }

    /// Requests the trailer of the current slide, if it has one.
    fn start_hero_video(&mut self) {
        let index = self.hero_index;
        self.hero_video.slide = Some(index);
        if !self.config.hero_video.unwrap_or(true) || !self.hero_video.player.available() {
            return;
        }
        let Some(url) = self
            .hero
            .get(index)
            .and_then(|item| best_trailer(&item.remote_trailers))
        else {
            return;
        };
        let video = &mut self.hero_video;
        video.load += 1;
        video.phase = Phase::Loading;
        video.frame = None;
        video.progress = 0.;
        log::debug!("hero trailer: slide {index} loads {url}");
        video.player.play(url, video.load, video.muted);
        // The slide timer starts only if the trailer is not up in time.
        self.hero_since = Instant::now() + TRAILER_GRACE;
    }

    /// One step of the slideshow: starts and follows the trailer, advances
    /// the slide, and redraws when something on screen changed.
    fn tick_hero(&mut self, cx: &mut Context<Self>) {
        if !self.hero_visible() {
            // Hold the slide while it is hidden; its trailer starts again later.
            self.hero_video.stop();
            if self.hero_paused.is_none() {
                self.hero_since = Instant::now() - self.hero_elapsed().min(HERO_SLIDE);
            }
            return;
        }
        let hold = self.hero_paused.is_some()
            || !(self.hero_video.window_active || self.hero_video.play_inactive);
        if hold != self.hero_video.held && self.hero_video.phase != Phase::None {
            self.hero_video.player.set_paused(hold);
        }
        self.hero_video.held = hold;
        if hold {
            if self.hero_paused.is_none() {
                self.hero_since = Instant::now() - self.hero_elapsed().min(HERO_SLIDE);
            }
            return;
        }
        if self.hero_video.slide != Some(self.hero_index) {
            self.start_hero_video();
        }

        let status = self.hero_video.player.status();
        let current = status.load == self.hero_video.load;
        match self.hero_video.phase {
            Phase::Playing => {
                if current && (status.ended || status.failed) {
                    log::debug!("hero trailer: ended, next slide");
                    self.step_hero(true);
                    cx.notify();
                    return;
                }
                if current && status.duration > 0. {
                    self.hero_video.progress =
                        (status.position / status.duration).clamp(0., 1.) as f32;
                }
                let newest = self.hero_video.player.frame(self.hero_video.load);
                let shown = self.hero_video.frame.as_ref().map(|(seq, _)| *seq);
                // A trailer above 30 frames a second costs redraws nobody sees.
                if let Some((seq, frame)) = newest
                    && shown != Some(seq)
                    && self.hero_video.last_redraw.elapsed() >= Duration::from_millis(30)
                {
                    self.hero_video.frame = Some((seq, frame));
                    self.hero_video.last_redraw = Instant::now();
                    self.hero_only_redraw = true;
                    cx.notify();
                }
                return;
            }
            Phase::Loading => {
                if current && (status.failed || status.ended) {
                    // No picture came; the slide goes on as an image slide.
                    log::debug!("hero trailer: could not play, slide timer runs");
                    self.hero_video.phase = Phase::None;
                    self.hero_since = Instant::now();
                } else if let Some(frame) = self.hero_video.player.frame(self.hero_video.load) {
                    log::debug!("hero trailer: playing");
                    self.hero_video.frame = Some(frame);
                    self.hero_video.phase = Phase::Playing;
                    cx.notify();
                    return;
                }
            }
            Phase::None => {}
        }

        if self.hero_since.elapsed() >= HERO_SLIDE {
            self.step_hero(true);
            cx.notify();
        } else if self.hero_video.last_redraw.elapsed() >= Duration::from_millis(150) {
            // The rows are cached, so a redraw for the progress line is cheap;
            // it still runs only a few times a second to keep the idle load low.
            self.hero_video.last_redraw = Instant::now();
            self.hero_only_redraw = true;
            cx.notify();
        }
    }

    /// Runs the slideshow. The loop is slow while images show and fast while
    /// a trailer plays, so a new frame reaches the screen without delay.
    pub fn start_hero_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hero_video.scale = window.scale_factor();
        self.hero_video._activation =
            Some(cx.observe_window_activation(window, |this, window, cx| {
                this.hero_video.window_active = window.is_window_active();
                cx.notify();
            }));
        self.hero_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                let fast = this
                    .read_with(cx, |this, _| this.hero_video.phase != Phase::None)
                    .unwrap_or(false);
                cx.background_executor()
                    .timer(Duration::from_millis(if fast { 8 } else { 150 }))
                    .await;
                if this.update(cx, |this, cx| this.tick_hero(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Favourite toggle for a hero slide. It changes the slide in place, so
    /// the page does not reload and the slide order stays.
    fn toggle_hero_favorite(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self.hero.get(index) else {
            return;
        };
        let item_id = item.id.clone();
        let favorite = !item.user_data.is_favorite;
        self.fetch(
            cx,
            {
                let item_id = item_id.clone();
                move |client| client.set_favorite(&item_id, favorite)
            },
            move |this, result, cx| {
                match result {
                    Ok(user_data) => {
                        if let Some(item) = this.hero.iter_mut().find(|i| i.id == item_id) {
                            item.user_data = user_data;
                        }
                    }
                    Err(err) => this.toast("Could not update favorites", format!("{err:#}"), cx),
                }
                cx.notify();
            },
        );
    }

    pub fn render_hero(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let item = self.hero.get(self.hero_index)?;
        let client = self.session.as_ref()?.client.clone();
        let t = UiTheme::read(cx).clone();
        let (vw, vh) = (self.viewport_w, self.viewport_h);
        let height = self.hero_height();
        let left = vw * 0.04;
        let index = self.hero_index;
        let total = self.hero.len();
        let white = rgb(0xffffff);
        let soft = rgba(0xffffffd9);

        let video = &self.hero_video;
        video
            .player
            .set_target_size((vw * video.scale) as u32, (height * video.scale) as u32);
        let playing = video.phase == Phase::Playing;
        let frame = video
            .frame
            .as_ref()
            .filter(|_| playing)
            .map(|(_, frame)| frame.buffer());

        let backdrop_of = |item: &Item| {
            item.backdrop_url(&client, (vw * 2.).min(1920.) as u32)
                .map(|url| {
                    crate::images::remote_with(url, px(0.), ObjectFit::Cover)
                        .absolute()
                        .inset_0()
                })
        };
        // While a slide comes in, the old backdrop stays under the new one,
        // and the new one fades in over it.
        let fading = self.hero_changed.elapsed() < HERO_FADE;
        let previous = self
            .hero_previous
            .filter(|_| fading)
            .and_then(|index| self.hero.get(index))
            .and_then(backdrop_of);
        let backdrop = backdrop_of(item).map(|image| {
            div()
                .absolute()
                .inset_0()
                .child(image)
                .with_animation(
                    ("hero.fade", self.hero_index),
                    Animation::new(HERO_FADE),
                    |layer, progress| layer.opacity(progress),
                )
        });

        let title = match item.logo_url(&client, 900) {
            Some(url) => div().size_full().child(
                crate::images::remote_logo(url)
                    .w_full()
                    .h(relative(0.7))
                    .mt(px(vh * 0.315 * 0.15)),
            ),
            None => {
                let size = match item.name.chars().count() {
                    0..=12 => 84.,
                    13..=25 => 56.,
                    26..=44 => 42.,
                    _ => 36.,
                };
                div().size_full().flex().items_center().child(
                    div()
                        .text_size(px(size))
                        .line_height(relative(1.3))
                        .font_weight(gpui_kit::FontWeight::BOLD)
                        .text_color(white)
                        .line_clamp(2)
                        .child(item.name.clone()),
                )
            }
        };

        let chip_with = |radius: f32| {
            div()
                .h(px(26.))
                .px(px(12.))
                .relative()
                .rounded(px(radius))
                .flex()
                .items_center()
                .gap(px(5.))
                .child(frosted(px(radius), rgba(0x282828a6)))
                .border_1()
                .border_color(rgba(0xffffff26))
                .text_size(px(12.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(soft)
                .whitespace_nowrap()
        };
        let chip = || chip_with(8.);
        let mut chips = div().flex().items_center().gap(px(4.));
        if let Some(score) = item.community_rating {
            chips = chips.child(
                chip()
                    .child(filled(Filled::Star, 15., rgb(0xf2b01e)))
                    .child(format!("{score:.1}")),
            );
        }
        if let Some(score) = item.critic_rating {
            chips = chips.child(chip().child(format!("{score:.0}%")));
        }
        if let Some(year) = item.production_year {
            chips = chips.child(chip().child(year.to_string()));
        }
        if let Some(rating) = &item.official_rating {
            chips = chips.child(chip().child(rating.clone()));
        }
        if item.is_series() {
            if let Some(n) = item.child_count.filter(|n| *n > 0) {
                let word = if n == 1 { "Season" } else { "Seasons" };
                chips = chips.child(chip().child(format!("{n} {word}")));
            }
        } else if let Some(secs) = item.runtime_secs() {
            chips = chips.child(chip().child(format!("Ends at {}", crate::macos::ends_at(secs))));
        }

        let genres = if item.genres.is_empty() {
            "No Genre Available".to_string()
        } else {
            item.genres
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("   ")
        };

        let glass_button = |id: &'static str, radius: f32| {
            div()
                .id(id)
                .size(px(50.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .relative()
                .rounded(px(radius))
                .child(frosted(px(radius), rgba(0xffffff1f)))
                .border_1()
                .border_color(crate::ui::glass::ring(rgba(0xffffff33)))
                .hover(|s| s.opacity(0.8))
        };
        let (details_target, play_target) = (item.clone(), item.clone());
        let favorite = item.user_data.is_favorite;
        let buttons = div()
            .flex()
            .items_center()
            .gap(px(15.))
            .child(
                glass_button("hero.details", 12.).tooltip(tip("More info"))
                    .child(icon(LucideIcon::Info, 22., rgba(0xffffffe6)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.open_item(details_target.clone(), cx)
                    })),
            )
            .child(
                div()
                    .id("hero.play")
                    .h(px(50.))
                    .px(px(16.))
                    .rounded(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .bg(t.colors.primary)
                    .text_color(t.colors.primary_foreground)
                    .text_size(px(18.))
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .hover(|s| s.opacity(0.8))
                    .child(filled(Filled::Play, 24., t.colors.primary_foreground))
                    .child("Play")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if play_target.is_series() {
                            this.play_series(&play_target, window, cx);
                        } else {
                            let resume = play_target.resume_secs() > 0;
                            this.play(&play_target, resume, window, cx);
                        }
                    })),
            )
            .child(
                glass_button("hero.favorite", 999.).tooltip(tip(if favorite {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    }))
                    .child(if favorite {
                        filled(Filled::Heart, 22., rgb(0xf92672))
                    } else {
                        icon(LucideIcon::Heart, 22., rgba(0xffffffe6))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_hero_favorite(index, cx)
                    })),
            );

        // Progress line: it fills on even slides and empties on odd ones, so
        // it never jumps back to empty between slides.
        let fraction = if playing {
            video.progress
        } else {
            (self.hero_elapsed().as_secs_f32() / HERO_SLIDE.as_secs_f32()).clamp(0., 1.)
        };
        let fill = if index % 2 == 0 {
            div().h_full().w(relative(fraction))
        } else {
            div()
                .absolute()
                .right_0()
                .top_0()
                .h_full()
                .w(relative(1. - fraction))
        };
        let counter = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.))
            .child(
                chip_with(999.)
                    .child(format!("{} / {total}", index + 1)),
            )
            .child(
                div()
                    .relative()
                    .w(px(64.))
                    .h(px(5.))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(rgb(0x686868))
                    .child(fill.rounded_full().bg(white)),
            );

        let arrow = |id: &'static str, glyph: LucideIcon, forward: bool| {
            div()
                .id(id)
                .absolute()
                .top(px(height * 0.35))
                .size(px(40.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .opacity(0.)
                .group_hover("hero", |s| s.opacity(1.))
                .hover(|s| s.bg(rgba(0x00000066)))
                .child(icon(glyph, 24., white))
                    .tooltip(tip(if forward { "Next (→)" } else { "Previous (←)" }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.step_hero(forward);
                    cx.notify();
                }))
        };

        let paused = self.hero_paused.is_some();
        Some(
            div()
                .id(SharedString::from("hero"))
                .group("hero")
                .relative()
                .w_full()
                .h(px(height))
                .flex_shrink_0()
                .overflow_hidden()
                .bg(t.colors.background)
                .children(previous)
                .children(backdrop)
                .child(div().absolute().inset_0().bg(rgba(0x00000033)))
                // The trailer covers the image; the web does not dim it.
                .when_some(frame, |el, frame| {
                    el.child(
                        div().absolute().inset_0().child(
                            surface(frame)
                                .size_full()
                                .object_fit(ObjectFit::Cover),
                        ),
                    )
                })
                .child(div().absolute().inset_0().bg(linear_gradient(
                    130.,
                    linear_color_stop(rgba(0x1d1d1da6), 0.1),
                    linear_color_stop(rgba(0x1d1d1d00), 1.0),
                )))
                // The web page masks the image out toward the rows below it.
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(height * 0.34))
                        .bg(linear_gradient(
                            180.,
                            linear_color_stop(t.colors.background.opacity(0.), 0.0),
                            linear_color_stop(t.colors.background, 0.82),
                        )),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(vh * 0.15))
                        .w(relative(0.4))
                        .h(px(vh * 0.315))
                        .child(title),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(vh * 0.45))
                        .child(chips),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(vh * 0.49 + 6.))
                        .text_size(px(18.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(rgba(0xffffff80))
                        .child(genres),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(vh * 0.53))
                        .max_w(px((vw * 0.6).min(840.)))
                        .text_size(px(16.))
                        .line_height(relative(1.4))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(white)
                        .line_clamp(2)
                        .child(item.overview.clone().unwrap_or_default()),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(vh * 0.62 + 12.))
                        .child(buttons),
                )
                .child(
                    div()
                        .absolute()
                        .right(relative(0.03))
                        .top(px(vh * 0.63))
                        .child(counter),
                )
                .child(arrow("hero.prev", LucideIcon::ChevronLeft, false).left(px(20.)))
                .child(arrow("hero.next", LucideIcon::ChevronRight, true).right(px(20.)))
                .when(playing, |el| {
                    el.child(
                        div()
                            .id("hero.mute").tooltip(tip(if video.muted {
                                "Unmute the trailer (M)"
                            } else {
                                "Mute the trailer (M)"
                            }))
                            .absolute()
                            .top(px(crate::views::shell::TOPBAR_H))
                            .right(px(vw * 0.028 + 60.))
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .opacity(0.3)
                            .hover(|s| s.opacity(0.9))
                            .child(icon(
                                if video.muted {
                                    LucideIcon::VolumeX
                                } else {
                                    LucideIcon::Volume2
                                },
                                22.,
                                white,
                            ))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.toggle_hero_mute();
                                cx.notify();
                            })),
                    )
                })
                .child(
                    div()
                        .id("hero.pause").tooltip(tip(if paused {
                            "Resume the slideshow (Space)"
                        } else {
                            "Pause the slideshow (Space)"
                        }))
                        .absolute()
                        .top(px(crate::views::shell::TOPBAR_H))
                        .right(px(vw * 0.028 + 18.))
                        .size(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .opacity(0.3)
                        .hover(|s| s.opacity(0.9))
                        .child(filled(
                            if paused { Filled::Play } else { Filled::Pause },
                            24.,
                            white,
                        ))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.toggle_hero_pause();
                            cx.notify();
                        })),
                ),
        )
    }
}

/// A hero item is usable only with a backdrop to show.
pub fn usable(item: &Item) -> bool {
    !item.backdrop_image_tags.is_empty()
}
