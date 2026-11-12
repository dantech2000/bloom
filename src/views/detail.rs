// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Item detail page, laid out like the Jellyfin web client with the Abyss
//! theme: a full-window backdrop, the logo, a poster that stays in place at
//! the left, a title ribbon with the actions, then details and rows.

use crate::ui::tone;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px, relative, rgb, rgba,
};

use crate::{
    app::{DetailData, Bloom, Page},
    icons::{Filled, filled},
    jellyfin::{Client, Item, ItemKind, MediaStream, Person, format_runtime},
    ui::{glass::glass, menu::Menu, scroll_area::ScrollArea, theme::UiTheme},
    views::{
        cards::{
            CARD_GAP, CARD_RADIUS, CARD_TEXT_H, Cards, artwork, card_frame, card_id,
            episode_card, icon, indicator, numbered_name, poster_card, progress_bar,
            section_padded, still_url,
        },
        parts::{icon_button, icon_menu, last_part},
        shell::TOPBAR_H,
    },
};
use crate::ui::tip::tip;

/// Sizes of the page for one window size.
struct Layout {
    /// Left edge of the text column.
    content_left: f32,
    /// Right padding of the page.
    side: f32,
    poster_w: f32,
    /// Height of the backdrop band above the title ribbon.
    band_h: f32,
    /// A phone: one column, no poster.
    compact: bool,
    /// Left edge and width of the logo in the band.
    logo: (f32, f32),
    /// Width of an episode card and of a season card.
    episode_w: f32,
    season_w: f32,
}

impl Bloom {
    /// The step from one episode card of a detail row to the next: the
    /// row of the season starts at the episode of the page by it.
    pub(crate) fn detail_episode_step(&self) -> f32 {
        self.detail_layout().episode_w + self.metrics().gap
    }

    fn detail_layout(&self) -> Layout {
        let (vw, vh) = (self.viewport_w, self.viewport_h);
        // A phone: one column the width of the page, and no poster beside
        // it (the band shows the art).
        if self.compact_w() {
            return Layout {
                content_left: 16.,
                side: 16.,
                poster_w: 0.,
                band_h: (TOPBAR_H + vh * 0.38).round(),
                compact: true,
                logo: (16., vw * 0.6),
                episode_w: (vw * 0.56).round(),
                season_w: (vw * 0.28).round(),
            };
        }
        // The web narrows the poster column on very wide windows.
        let (content, poster) = if vw / vh >= 1.98 {
            (0.2845, 0.21)
        } else {
            (0.3245, 0.25)
        };
        Layout {
            content_left: (vw * content).round(),
            side: (vw * 0.033).round(),
            poster_w: (vw * poster).round(),
            band_h: (TOPBAR_H + vh * 0.38).round(),
            compact: false,
            logo: (vw * 0.325, vw * 0.25),
            episode_w: (vw * 0.173).round(),
            season_w: (vw * 0.09).round(),
        }
    }

    pub fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Detail(data) = &self.nav.page else {
            unreachable!()
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let colors = UiTheme::read(cx).colors;
        let item = &data.item;
        let l = self.detail_layout();
        let (vw, vh) = (self.viewport_w, self.viewport_h);

        // The backdrop fills the window behind the page and stays in place,
        // dimmed so the text stays readable. A window-sized quad costs about
        // half a millisecond of GPU time per frame, so the dimming is folded
        // into the image: the image at 45% over the page background is the
        // same as a 55% veil of that colour over the image. The gradient is
        // fully transparent above its first stop, so it starts there.
        let backdrop = item
            .backdrop_url(&client, (vw * 2.).min(1920.) as u32)
            .or_else(|| item.wide_url(&client, (vw * 2.).min(1920.) as u32))
            .map(|url| {
                div()
                    .absolute()
                    .inset_0()
                    .child(
                        div().absolute().inset_0().opacity(0.45).child(
                            crate::images::remote_with(url, px(0.), ObjectFit::Cover)
                                .absolute()
                                .inset_0(),
                        ),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .top(relative(0.15))
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(colors.background.opacity(0.0), 0.),
                                linear_color_stop(colors.background.opacity(0.92), 0.8 / 0.85),
                            )),
                    )
            });
        let logo = item.logo_url(&client, 800);
        let band = div()
            .relative()
            .w_full()
            .h(px(l.band_h))
            .flex_shrink_0()
            .when_some(logo, |el, url| {
                el.child(
                    crate::images::remote_logo(url)
                        .absolute()
                        .left(px(l.logo.0))
                        .top(px(TOPBAR_H + vh * 0.20))
                        .w(px(l.logo.1))
                        .h(px(vh * 0.16)),
                )
            });

        let ribbon = div()
            .w_full()
            .min_h(px(107.))
            .py(px(8.))
            .pl(px(l.content_left))
            .pr(px(l.side))
            .bg(rgba(0x00000033))
            .flex()
            // A phone: the actions on a row of their own under the name.
            .when(l.compact, |el| el.flex_wrap())
            .items_center()
            .gap(px(if l.compact { 12. } else { 24. }))
            .child(
                div()
                    .when(!l.compact, |el| el.flex_1().min_w_0())
                    .when(l.compact, |el| el.w_full())
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(self.render_name(item, cx))
                    .child(meta_line(item, cx)),
            )
            .child(self.render_actions(item, cx));

        let mut body = div()
            .pl(px(l.content_left))
            .pr(px(l.side))
            .pt(px(20.))
            .flex()
            .flex_col()
            .gap(px(14.));
        body = body.children(self.render_tracks(data, cx));
        if let Some(tagline) = item.taglines.first().filter(|line| !line.is_empty()) {
            body = body.child(
                div()
                    .mt(px(6.))
                    .text_size(px(17.))
                    .text_color(colors.foreground)
                    .child(tagline.clone()),
            );
        }
        if let Some(text) = item.overview.clone().filter(|o| !o.is_empty()) {
            body = body.child(
                div()
                    .mt(px(8.))
                    .text_size(px(15.))
                    .line_height(relative(1.35))
                    .text_color(colors.foreground.opacity(0.82))
                    .child(text),
            );
        }
        body = body.children(tags_and_links(item, cx));
        body = body.children(self.enhanced_detail_lines(item, cx));
        body = body.children(credits(item, cx));

        let mut rows: Vec<Div> = Vec::new();
        if let Some(next) = &data.next_up {
            let w = l.episode_w;
            rows.push(plain_section(
                "Next Up",
                l.content_left,
                l.side,
                vec![episode_card(next, &client, w, cx)],
                cx,
            ));
        }
        if !data.seasons.is_empty() {
            let w = l.season_w;
            let cards = data
                .seasons
                .iter()
                .map(|season| poster_card(season, &client, w, cx))
                .collect();
            rows.push(plain_section(
                "Seasons",
                l.content_left,
                l.side,
                cards,
                cx,
            ));
        }
        if item.kind == ItemKind::Episode && data.episodes.len() > 1 {
            // The other episodes of the season, with this one marked.
            let w = l.episode_w;
            let current = item.id.clone();
            let cards = Cards::new(w, data.episodes.len(), |i, cx| {
                let episode = &data.episodes[i];
                // A ring on the image marks the episode of the page.
                episode_card(episode, &client, w, cx).when(episode.id == current, |el| {
                    el.relative().child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w(px(w))
                            .h(px((w * 9. / 16.).round()))
                            .rounded(px(CARD_RADIUS))
                            .border_2()
                            .border_color(rgba(0xf5f5f7e6)),
                    )
                })
            });
            rows.push(section_padded(
                self,
                "detail.season-episodes",
                format!(
                    "More from {}",
                    item.season_name.as_deref().unwrap_or("this season")
                ),
                None,
                (w * 9. / 16.).round() + CARD_TEXT_H,
                cards,
                l.content_left,
                l.side,
                cx,
            ));
        } else if !data.episodes.is_empty() {
            rows.push(
                div()
                    .pl(px(l.content_left - 12.))
                    .pr(px(l.side))
                    // Only the rows in view are built (`ui::rows`): a
                    // season of 150 episodes then costs as much per frame
                    // as one of 8.
                    .child(crate::ui::rows::rows(
                        cx,
                        data.episodes.len(),
                        EPISODE_ROW_H + EPISODE_ROW_GAP,
                        |this, range, cx| {
                            let (Page::Detail(data), Some(session)) = (&this.nav.page, &this.session)
                            else {
                                return Vec::new();
                            };
                            let episodes = &data.episodes;
                            episodes[range.start.min(episodes.len())..range.end.min(episodes.len())]
                                .iter()
                                .map(|ep| {
                                    episode_row(ep, &session.client, this.viewport_w, cx)
                                        .into_any_element()
                                })
                                .collect()
                        },
                    )),
            );
        }
        rows.extend(self.bookmarks_detail_section(item, l.content_left, l.side, cx));
        rows.extend(self.render_cast(data, &client, &l, cx));
        if !data.similar.is_empty() {
            let w = self.metrics().portrait_w;
            let cards = Cards::new(w, data.similar.len(), |i, cx| {
                poster_card(&data.similar[i], &client, w, cx)
            });
            rows.push(section_padded(
                self,
                "detail.similar",
                "More Like This",
                None,
                w * 1.5 + CARD_TEXT_H,
                cards,
                l.content_left,
                l.side,
                cx,
            ));
        }

        rows.extend(self.enhanced_detail_rows(item, l.content_left, l.side, cx));

        // The poster keeps its place while the page scrolls under it. A
        // phone has no room beside the text for it.
        let poster = (!l.compact).then(|| div()
            .absolute()
            .left(px(l.side))
            .top(px(vh * 0.15))
            .rounded(px(24.))
            .shadow(UiTheme::read(cx).shadows.lg.to_vec())
            .child(artwork(
                item.poster_url(&client, (l.poster_w * 2.) as u32),
                l.poster_w,
                (l.poster_w * poster_ratio(item)).round(),
                px(24.),
                LucideIcon::Clapperboard,
                cx,
            )));

        div()
            .relative()
            .size_full()
            .children(backdrop)
            .child(
                ScrollArea::new("detail.scroll")
                    .track(&self.nav.scroll)
                    .size_full()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .pb(px(72.))
                            .child(band)
                            .child(ribbon)
                            .child(body)
                            .child(div().mt(px(24.)).flex().flex_col().gap(px(22.)).children(rows)),
                    ),
            )
            .children(poster)
    }

    /// Title lines: the series is a link on season and episode pages.
    fn render_name(&self, item: &Item, cx: &mut Context<Self>) -> Div {
        let fg = UiTheme::read(cx).colors.foreground;
        let heading = |text: String| {
            div()
                .text_size(px(27.))
                .line_height(px(34.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(fg)
                .truncate()
                .child(text)
        };
        let Some(series) = item
            .series_name
            .clone()
            .filter(|_| matches!(item.kind, ItemKind::Season | ItemKind::Episode))
        else {
            return div().child(heading(item.name.clone()));
        };
        let series_id = item.series_id.clone();
        let target_name = series.clone();
        // On an episode page the season is a link too.
        let season = item.season_name.clone().filter(|_| item.kind == ItemKind::Episode);
        let second = if item.kind == ItemKind::Episode {
            numbered_name(item)
        } else {
            item.name.clone()
        };
        let season_link = season.zip(item.season_id.clone()).map(|(name, id)| {
            let target = Item {
                id,
                kind: "Season".into(),
                name: name.clone(),
                series_id: item.series_id.clone(),
                series_name: item.series_name.clone(),
                ..Default::default()
            };
            div()
                .id("detail.season-link")
                .flex_shrink_0()
                .cursor_pointer()
                .hover(|s| s.underline().text_color(fg))
                .tooltip(tip("Open the season"))
                .child(format!("{name} ›"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.open_item(target.clone(), cx)
                }))
        });
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("detail.series-link")
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .tooltip(tip("Open the show"))
                    .child(heading(series))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(id) = &series_id {
                            this.open_item(
                                Item {
                                    id: id.clone(),
                                    kind: "Series".into(),
                                    name: target_name.clone(),
                                    ..Default::default()
                                },
                                cx,
                            );
                        }
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(17.))
                    .text_color(fg.opacity(0.82))
                    .children(season_link)
                    .child(div().min_w_0().truncate().child(second)),
            )
    }

    /// Play pill and the icon actions at the right of the title ribbon.
    fn render_actions(&self, item: &Item, cx: &mut Context<Self>) -> Div {
        let fg = rgba(0xffffffde);
        let round = |id: &'static str, glyph: gpui_kit::Svg| {
            icon_button(id, last_part(id), glyph, 46., ACTION_HOVER)
        };
        let mut row = div().flex_shrink_0().flex().items_center().gap(px(8.));

        if item.is_playable() || item.is_series() {
            let resumable = item.resume_secs() > 0;
            row = row.child(
                div()
                    .id("detail.play")
                    .role(gpui_kit::Role::Button)
                    .aria_label("play")
                    .h(px(44.))
                    .pl(px(18.))
                    .pr(px(24.))
                    .mr(px(4.))
                    .rounded(px(CARD_RADIUS))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .bg(rgba(0xfffffff2))
                    .text_color(rgb(0x111111))
                    .text_size(px(16.))
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .hover(|s| s.bg(rgb(0xffffff)))
                    .child(filled(Filled::Play, 24., rgb(0x111111)))
                    .child(if resumable { "Resume" } else { "Play" })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.play_detail(resumable, window, cx)
                    })),
            );
            if resumable {
                row = row.child(
                    round("detail.restart", icon(LucideIcon::RotateCcw, 25., fg))
                        .tooltip(tip("Play from the beginning"))
                        .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.play_detail(false, window, cx)
                        }),
                    ),
                );
            }
        }
        // The trailer opens in the browser (iOS hands the link to Safari or
        // the YouTube app through `UIApplication openURL:`).
        if let Some(url) = item.remote_trailers.iter().find_map(|t| t.url.clone()) {
            row = row.child(
                round("detail.trailer", filled(Filled::Trailer, 25., fg))
                    .tooltip(tip("Watch the trailer"))
                    .on_click(move |_: &ClickEvent, _, cx| crate::macos::open_web_url(cx, &url)),
            );
        }

        let played = item.user_data.played;
        let favorite = item.user_data.is_favorite;
        // Download for offline use; see `downloads::ui`.
        row = row.children(self.render_download_action(item, cx));
        row.child(
            round(
                "detail.watched",
                icon(LucideIcon::Check, 25., if played { tone::INK } else { fg }),
            )
            .when(played, |el| el.bg(tone::EDGE))
                .tooltip(tip(if played { "Mark unplayed" } else { "Mark played" }))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_played(cx))),
        )
        .child(
            round(
                "detail.favorite",
                filled(Filled::Heart, 25., if favorite { rgb(0xf92672) } else { fg }),
            )
            .tooltip(tip(if favorite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            }))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_favorite(cx))),
        )
        .child(
            icon_menu(
                &self.popups.more_menu,
                "detail.more",
                "More",
                filled(Filled::More, 25., fg),
                46.,
                ACTION_HOVER,
            ),
        )
    }

    /// "Video", "Audio" and "Subtitles" rows of a playable item. Audio and
    /// subtitles are menus; the choice applies when playback starts.
    fn render_tracks(&self, data: &DetailData, cx: &Context<Self>) -> Option<Div> {
        let t = UiTheme::read(cx);
        let item = &data.item;
        item.media_sources.first()?;
        let label = t.colors.foreground;
        let value = t.colors.foreground.opacity(0.7);
        // The name of the track at a place among the tracks of its kind,
        // and the length of the longest name, which sizes the box.
        let title = |streams: &[&MediaStream], n: usize| {
            streams
                .get(n)
                .and_then(|s| s.display_title.clone())
                .unwrap_or_else(|| format!("Track {}", n + 1))
        };
        let longest = |streams: &[&MediaStream]| {
            streams
                .iter()
                .enumerate()
                .map(|(n, s)| match &s.display_title {
                    Some(title) => title.chars().count(),
                    None => format!("Track {}", n + 1).len(),
                })
                .max()
                .unwrap_or(0)
        };
        // A phone: a box wider than the room left beside the label goes
        // under it, the whole width (it ran off the page, 2026-10-23).
        let compact = self.compact_w();
        let row = |name: &'static str| {
            div()
                .flex()
                .when(compact, |el| el.flex_wrap().gap_y(px(6.)))
                .items_center()
                .text_size(px(15.))
                .child(div().w(px(93.)).flex_shrink_0().text_color(label).child(name))
        };
        // The box is as wide as its longest option needs, within limits.
        let select = |state: &gpui_kit::Entity<crate::ui::menu::MenuState>,
                      name: &'static str,
                      text: String,
                      longest: usize| {
            let width = (longest as f32 * 7.4 + 64.).clamp(170., 545.);
            Menu::new(state, name)
                .trigger(
                    div()
                        .when(!compact, |el| el.w(px(width)))
                        .when(compact, |el| el.w_full())
                        .h(px(if compact { 44. } else { 34. }))
                        .pl(px(9.))
                        .pr(px(6.))
                        .relative()
                        .rounded(px(8.))
                        .child(glass(px(8.), tone::HOVER))
                        .border_1()
                        .border_color(crate::ui::glass::ring(rgba(0xffffff33)))
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_size(px(15.))
                        .text_color(label)
                        .child(div().truncate().child(text))
                        .child(icon(LucideIcon::ChevronDown, 18., label)),
                )
        };

        let mut rows = div().max_w(px(655.)).flex().flex_col().gap(px(10.));
        if let Some(video) = item.stream_title("Video") {
            rows = rows.child(row("Video").h(px(34.)).child(div().text_color(value).child(video)));
        }
        let audio = item.streams("Audio");
        if !audio.is_empty() {
            let current = data
                .audio
                .or_else(|| audio.iter().position(|s| s.is_default))
                .unwrap_or(0);
            rows = rows.child(if audio.len() > 1 {
                row("Audio").child(select(
                    &self.popups.detail_audio_menu,
                    "Audio",
                    title(&audio, current),
                    longest(&audio),
                ))
            } else {
                row("Audio")
                    .h(px(34.))
                    .child(div().text_color(value).child(title(&audio, current)))
            });
        }
        let subtitles = item.streams("Subtitle");
        if !subtitles.is_empty() {
            let current = data
                .subtitle
                .unwrap_or_else(|| subtitles.iter().position(|s| s.is_default));
            let text = match current {
                Some(n) => title(&subtitles, n),
                None => "Off".to_string(),
            };
            rows = rows.child(row("Subtitles").child(select(
                &self.popups.detail_subtitle_menu,
                "Subtitles",
                text,
                longest(&subtitles),
            )));
        }
        Some(rows)
    }

    /// "Cast & Crew" and "Guest Stars" rows.
    fn render_cast(
        &self,
        data: &DetailData,
        client: &Client,
        l: &Layout,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        // A phone: about three and a half tiles across, so a name reads.
        let (w, h) = if self.compact_w() {
            (96., 134.)
        } else {
            ((self.viewport_w * 0.10 - 9.).round(), (self.viewport_w * 0.14 - 9.).round())
        };
        let mut rows = Vec::new();
        for (id, title, guests) in [
            ("detail.cast", "Cast & Crew", false),
            ("detail.guests", "Guest Stars", true),
        ] {
            let people = if guests { &data.guests } else { &data.cast };
            let cards = Cards::new(w, people.len(), |i, cx| {
                let (person, role) = &people[i];
                person_card(person, role, client, w, h, cx)
            });
            if !cards.is_empty() {
                rows.push(section_padded(
                    self,
                    id,
                    title,
                    None,
                    h + CARD_TEXT_H,
                    cards,
                    l.content_left,
                    l.side,
                    cx,
                ));
            }
        }
        rows
    }
}

/// The fill under the pointer of an action of the title ribbon.
const ACTION_HOVER: u32 = 0x00000066;

/// Height to width ratio of the detail poster.
fn poster_ratio(item: &Item) -> f32 {
    if item.kind == ItemKind::Episode { 9. / 16. } else { 1.5 }
}

/// Year, runtime, age rating, score and end time under the title.
fn meta_line(item: &Item, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    let color = t.colors.foreground.opacity(0.82);
    let mut line = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(15.))
        .gap_y(px(4.))
        .text_size(px(15.))
        .text_color(color);
    if let Some(year) = item.year_label() {
        line = line.child(year);
    }
    let runtime = item.runtime_secs().filter(|_| !item.is_series());
    if let Some(secs) = runtime {
        line = line.child(format_runtime(secs));
    }
    if let Some(rating) = item
        .official_rating
        .clone()
        .filter(|_| !matches!(item.kind, ItemKind::Season | ItemKind::Episode))
    {
        line = line.child(
            div()
                .px(px(7.))
                .py(px(2.))
                .rounded(px(4.))
                .border_1()
                .border_color(color)
                .text_size(px(14.))
                .child(rating),
        );
    }
    if let Some(score) = item.community_rating {
        line = line.child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(filled(Filled::Star, 18., tone::INK_80))
                .child(format!("{score:.1}")),
        );
    }
    if let Some(score) = item.critic_rating {
        line = line.child(format!("{score:.0}%"));
    }
    if let Some(secs) = runtime {
        let left = (secs - item.resume_secs()).max(0);
        line = line.child(format!("Ends at {}", crate::macos::ends_at(left)));
    }
    // Audio languages, as the Jellyfin Enhanced plugin lists them.
    let mut languages: Vec<&'static str> = Vec::new();
    for stream in item.streams("Audio") {
        if let Some(name) = stream.language.as_deref().and_then(bloom_core::languages::name)
            && !languages.contains(&name)
        {
            languages.push(name);
        }
    }
    if !languages.is_empty() {
        line = line.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon(LucideIcon::Languages, 16., color.into()))
                .child(languages.join(", ")),
        );
    }
    line
}

/// "Tags: a, b" and the external site names.
fn tags_and_links(item: &Item, cx: &Context<Bloom>) -> Option<Div> {
    let t = UiTheme::read(cx);
    let links: Vec<String> = item
        .external_urls
        .iter()
        .filter_map(|link| link.name.clone())
        .collect();
    if item.tags.is_empty() && links.is_empty() {
        return None;
    }
    let mut block = div()
        .mt(px(14.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .text_size(px(14.))
        .text_color(t.colors.foreground);
    if !item.tags.is_empty() {
        block = block.child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(format!("Tags: {}", item.tags.join(", "))),
        );
    }
    if !links.is_empty() {
        block = block.child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(links.join(", ")),
        );
    }
    Some(block)
}

/// Director, writer, studio and genre rows.
fn credits(item: &Item, cx: &Context<Bloom>) -> Option<Div> {
    let t = UiTheme::read(cx);
    let plural = |one: &'static str, many: &'static str, n: usize| if n == 1 { one } else { many };
    let directors = item.people_of("Director");
    let writers = item.people_of("Writer");
    let creators = item.people_of("Creator");
    let studios: Vec<String> = item.studios.iter().filter_map(|s| s.name.clone()).collect();
    let rows = [
        (plural("Creator", "Creators", creators.len()), creators),
        (plural("Director", "Directors", directors.len()), directors),
        (plural("Writer", "Writers", writers.len()), writers),
        (plural("Studio", "Studios", studios.len()), studios),
        (
            plural("Genre", "Genres", item.genres.len()),
            item.genres.clone(),
        ),
    ];
    if rows.iter().all(|(_, values)| values.is_empty()) {
        return None;
    }
    Some(
        div().mt(px(14.)).flex().flex_col().gap(px(9.)).children(
            rows.into_iter()
                .filter(|(_, values)| !values.is_empty())
                .map(|(name, values)| {
                    div()
                        .flex()
                        .items_start()
                        .text_size(px(15.))
                        .child(
                            div()
                                .w(px(100.))
                                .flex_shrink_0()
                                .text_color(t.colors.foreground.opacity(0.82))
                                .child(name),
                        )
                        .child(
                            // The room left beside the label, up to 560: a
                            // long studio list wraps instead of running off
                            // a phone's page.
                            div()
                                .flex_1()
                                .min_w_0()
                                .max_w(px(560.))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(t.colors.foreground)
                                .child(values.join(", ")),
                        )
                }),
        ),
    )
}

/// Heading with a wrapping row of cards and no arrows.
fn plain_section(
    title: &'static str,
    left: f32,
    right: f32,
    cards: Vec<Stateful<Div>>,
    cx: &Context<Bloom>,
) -> Div {
    let t = UiTheme::read(cx);
    div()
        .pl(px(left))
        .pr(px(right))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .text_size(px(22.))
                .line_height(px(30.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child(title),
        )
        .child(
            div()
                .pl(px(9.))
                .flex()
                .flex_wrap()
                .gap_x(px(CARD_GAP))
                .gap_y(px(10.))
                .children(cards),
        )
}

fn person_card(
    person: &Person,
    role: &str,
    client: &Client,
    w: f32,
    h: f32,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let colors = UiTheme::read(cx).colors;
    let url = person
        .primary_image_tag
        .as_deref()
        .map(|tag| client.image_url(&person.id, "Primary", Some(tag), (w * 2.) as u32));
    card_frame(SharedString::from(format!("person.{}.{role}", person.id)), w)
        .on_click(cx.listener({
            let (id, name) = (person.id.clone(), person.name.clone());
            move |this, _: &ClickEvent, _, cx| this.open_person(&id, &name, cx)
        }))
        .child(artwork(url, w, h, px(CARD_RADIUS), LucideIcon::User, cx))
        .child(
            div()
                .mt(px(4.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(colors.foreground)
                        .truncate()
                        .child(person.name.clone()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(colors.muted_foreground)
                        .truncate()
                        .child(role.to_string()),
                ),
        )
}

/// Height of an episode row: its still and the padding around it. The text
/// beside the still is at most four lines of overview, so it is never taller.
const EPISODE_ROW_H: f32 = 166.;
/// Space between two episode rows.
const EPISODE_ROW_GAP: f32 = 2.;

/// One episode of a season: still, number and name, details, overview.
fn episode_row(ep: &Item, client: &Client, vw: f32, cx: &mut Context<Bloom>) -> Stateful<Div> {
    let colors = UiTheme::read(cx).colors;
    // A phone: a wide still, as the episode's picture is, with the text
    // beside it; the Mac's column is taller than wide.
    let (w, h) = if vw < crate::views::shell::COMPACT_W {
        let w = (vw * 0.36).round();
        (w, (w * 9. / 16.).round())
    } else {
        ((vw * 0.195).round(), 160.)
    };
    let still = still_url(ep, client, 500);
    let name = numbered_name(ep);
    let mut details: Vec<String> = Vec::new();
    if let Some(secs) = ep.runtime_secs() {
        details.push(format_runtime(secs));
    }
    if let Some(score) = ep.community_rating {
        details.push(format!("★ {score:.1}"));
    }
    let (open, play) = (ep.clone(), ep.clone());
    let resume = ep.resume_secs() > 0;
    div()
        .id(card_id("episode", ep))
        .w_full()
        .h(px(EPISODE_ROW_H))
        .flex()
        .items_start()
        .gap(px(12.))
        .pl(px(12.))
        .py(px(3.))
        .rounded(px(CARD_RADIUS))
        .cursor_pointer()
        .hover(|s| s.bg(rgba(0xf5f5f714)))
        .on_click(cx.listener(move |this, _, _, cx| this.open_item(open.clone(), cx)))
        .child(
            artwork(still, w, h, px(CARD_RADIUS), LucideIcon::Tv, cx)
                .flex_shrink_0()
                .children(progress_bar(ep.progress()))
                .children(indicator(ep))
                .child(
                    div()
                        .id(card_id("episode.play", ep)).tooltip(tip("Play"))
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .size(px(48.))
                                .rounded(px(CARD_RADIUS))
                                .bg(rgba(0x00000066))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(filled(Filled::Play, 28., rgb(0xffffff))),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.play(&play, resume, window, cx);
                        })),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .py(px(12.))
                .pr(px(12.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(colors.foreground)
                        .truncate()
                        .child(name),
                )
                .when(!details.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(14.))
                            .text_color(colors.muted_foreground)
                            .child(details.join("   ")),
                    )
                })
                .when_some(ep.overview.clone().filter(|o| !o.is_empty()), |el, text| {
                    el.child(
                        div()
                            .text_size(px(14.))
                            .line_height(relative(1.35))
                            .text_color(colors.muted_foreground)
                            .line_clamp(4)
                            .child(text),
                    )
                }),
        )
}

use crate::ui::menu::MenuItem;
use gpui_kit::Window;

// The menus of the page, and Play with the tracks picked on it.
impl Bloom {
    /// Rebuilds the track menus and the "more" menu of the detail page.
    pub(crate) fn rebuild_detail_menus(&mut self, cx: &mut Context<Self>) {
        self.subs_load_rights(cx);
        let Page::Detail(data) = &self.nav.page else {
            return;
        };
        let this = cx.weak_entity();
        let title = |stream: &crate::jellyfin::MediaStream, n: usize| {
            stream
                .display_title
                .clone()
                .unwrap_or_else(|| format!("Track {}", n + 1))
        };

        let audio_streams = data.item.streams("Audio");
        let current_audio = data
            .audio
            .or_else(|| audio_streams.iter().position(|s| s.is_default))
            .unwrap_or(0);
        let audio: Vec<MenuItem> = audio_streams
            .iter()
            .enumerate()
            .map(|(n, stream)| {
                let handle = this.clone();
                MenuItem::new(SharedString::from(format!("detail.audio.{n}")), title(stream, n))
                    .radio(n == current_audio)
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |this, cx| {
                                if let Page::Detail(data) = &mut this.nav.page {
                                    data.audio = Some(n);
                                }
                                this.rebuild_detail_menus(cx);
                                cx.notify();
                            })
                            .ok();
                    })
            })
            .collect();

        let subtitle_streams = data.item.streams("Subtitle");
        let current_subtitle = data
            .subtitle
            .unwrap_or_else(|| subtitle_streams.iter().position(|s| s.is_default));
        let pick_subtitle = |choice: Option<usize>, label: String| {
            let handle = this.clone();
            let id = match choice {
                Some(n) => format!("detail.subtitle.{n}"),
                None => "detail.subtitle.off".to_string(),
            };
            MenuItem::new(SharedString::from(id), label)
                .radio(choice == current_subtitle)
                .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| {
                            if let Page::Detail(data) = &mut this.nav.page {
                                data.subtitle = Some(choice);
                            }
                            this.rebuild_detail_menus(cx);
                            cx.notify();
                        })
                        .ok();
                })
        };
        let mut subtitles = vec![pick_subtitle(None, "Off".to_string())];
        subtitles.extend(
            subtitle_streams
                .iter()
                .enumerate()
                .map(|(n, stream)| pick_subtitle(Some(n), title(stream, n))),
        );

        if let Some(search) = self.subs_search_item(&data.item.id, &data.item.display_title(), cx) {
            subtitles.push(MenuItem::separator());
            subtitles.push(search);
        }

        let web = self
            .session
            .as_ref()
            .map(|s| s.client.web_url(&data.item.id));
        let handle = this.clone();
        let mut more = vec![MenuItem::new("detail.refresh", "Refresh").on_click(
            move |_, _, cx| {
                handle.update(cx, |this, cx| this.load_page(cx)).ok();
            },
        )];
        if let Some(url) = web {
            more.push(
                MenuItem::new("detail.web", "Open in Jellyfin Web")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }

        let item = data.item.clone();
        let list_items = self.list_menu_items(&item, cx);
        if !list_items.is_empty() {
            more.push(MenuItem::separator());
            more.extend(list_items);
        }
        more.extend(self.hide_menu_items(&item, true, cx));
        more.extend(self.metadata_menu_items(&item.id, &item.name, item.kind.name(), cx));

        self.popups.detail_audio_menu
            .update(cx, |menu, cx| menu.set_items(audio, cx));
        self.popups.detail_subtitle_menu
            .update(cx, |menu, cx| menu.set_items(subtitles, cx));
        self.popups.more_menu.update(cx, |menu, cx| menu.set_items(more, cx));
    }

    /// Plays the detail page item with the tracks picked on the page.
    pub(crate) fn play_detail(&mut self, resume: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Detail(data) = &self.nav.page else {
            return;
        };
        let (item, audio, subtitle) = (data.item.clone(), data.audio, data.subtitle);
        if item.is_series() {
            self.play_series(&item, window, cx);
            return;
        }
        // The load takes the choice: the server gets it, for a transcode,
        // and mpv, for the file itself (`stream_begin`).
        self.stream.wanted_audio = audio;
        self.stream.wanted_subtitle = subtitle;
        self.play(&item, resume, window, cx);
        self.stream_choice_left(&item);
    }
}
