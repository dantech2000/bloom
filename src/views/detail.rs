// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Item detail page, laid out like the Jellyfin web client with the Abyss
//! theme: a full-window backdrop, the logo, a poster that stays in place at
//! the left, a title ribbon with the actions, then details and rows.

use gpui_icons::LucideIcon;
use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, ClickEvent, Context, Div, Element, ElementId,
    GlobalElementId, InspectorElementId, InteractiveElement as _, IntoElement, LayoutId,
    ObjectFit, ParentElement as _, Pixels, SharedString, Stateful,
    StatefulInteractiveElement as _, Style, Styled, WeakEntity, Window, div, linear_color_stop,
    linear_gradient, point, prelude::FluentBuilder as _, px, relative, rgb, rgba, size,
};

use crate::{
    app::{DetailData, Bloom, Page},
    icons::{Filled, filled},
    jellyfin::{Client, Item, Person, format_runtime},
    ui::{glass::glass, menu::Menu, scroll_area::ScrollArea, theme::UiTheme},
    views::{
        cards::{
            CARD_GAP, CARD_RADIUS, CARD_TEXT_H, Cards, artwork, card_id, episode_card, icon,
            poster_card, section_padded,
        },
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
}

impl Bloom {
    fn detail_layout(&self) -> Layout {
        let (vw, vh) = (self.viewport_w, self.viewport_h);
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
        }
    }

    pub fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Detail(data) = &self.page else {
            unreachable!()
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let t = UiTheme::read(cx).clone();
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
                                linear_color_stop(t.colors.background.opacity(0.0), 0.),
                                linear_color_stop(
                                    t.colors.background.opacity(0.92),
                                    0.8 / 0.85,
                                ),
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
                        .left(px(vw * 0.325))
                        .top(px(TOPBAR_H + vh * 0.20))
                        .w(px(vw * 0.25))
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
            .items_center()
            .gap(px(24.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
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
                    .text_color(t.colors.foreground)
                    .child(tagline.clone()),
            );
        }
        if let Some(text) = item.overview.clone().filter(|o| !o.is_empty()) {
            body = body.child(
                div()
                    .mt(px(8.))
                    .text_size(px(15.))
                    .line_height(relative(1.35))
                    .text_color(t.colors.foreground.opacity(0.82))
                    .child(text),
            );
        }
        body = body.children(tags_and_links(item, cx));
        body = body.children(self.enhanced_detail_lines(item, cx));
        body = body.children(credits(item, cx));

        let mut rows: Vec<Div> = Vec::new();
        if let Some(next) = &data.next_up {
            let w = (vw * 0.173).round();
            rows.push(plain_section(
                "Next Up",
                l.content_left,
                l.side,
                vec![episode_card(next, &client, w, cx)],
                cx,
            ));
        }
        if !data.seasons.is_empty() {
            let w = (vw * 0.09).round();
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
        if item.kind == "Episode" && data.episodes.len() > 1 {
            // The other episodes of the season, with this one marked.
            let w = (vw * 0.173).round();
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
                    .child(EpisodeRows {
                        app: cx.weak_entity(),
                        count: data.episodes.len(),
                    }),
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

        // The poster keeps its place while the page scrolls under it.
        let poster = div()
            .absolute()
            .left(px(l.side))
            .top(px(vh * 0.15))
            .rounded(px(24.))
            .shadow(t.shadows.lg.clone())
            .child(artwork(
                item.poster_url(&client, (l.poster_w * 2.) as u32),
                l.poster_w,
                (l.poster_w * poster_ratio(item)).round(),
                px(24.),
                LucideIcon::Clapperboard,
                cx,
            ));

        div()
            .relative()
            .size_full()
            .children(backdrop)
            .child(
                ScrollArea::new("detail.scroll")
                    .track(&self.page_scroll)
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
            .child(poster)
    }

    /// Title lines: the series is a link on season and episode pages.
    fn render_name(&self, item: &Item, cx: &mut Context<Self>) -> Div {
        let t = UiTheme::read(cx).clone();
        let heading = |text: String| {
            div()
                .text_size(px(27.))
                .line_height(px(34.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .truncate()
                .child(text)
        };
        let Some(series) = item
            .series_name
            .clone()
            .filter(|_| matches!(item.kind.as_str(), "Season" | "Episode"))
        else {
            return div().child(heading(item.name.clone()));
        };
        let series_id = item.series_id.clone();
        let target_name = series.clone();
        // On an episode page the season is a link too.
        let season = item.season_name.clone().filter(|_| item.kind == "Episode");
        let second = match (item.kind.as_str(), item.index_number) {
            ("Episode", Some(n)) => format!("{n}. {}", item.name),
            _ => item.name.clone(),
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
                .hover(|s| s.underline().text_color(t.colors.foreground))
                .tooltip(crate::ui::tip::tip("Open the season"))
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
                    .tooltip(crate::ui::tip::tip("Open the show"))
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
                    .text_color(t.colors.foreground.opacity(0.82))
                    .children(season_link)
                    .child(div().min_w_0().truncate().child(second)),
            )
    }

    /// Play pill and the icon actions at the right of the title ribbon.
    fn render_actions(&self, item: &Item, cx: &mut Context<Self>) -> Div {
        let fg = rgba(0xffffffde);
        let round = |id: &'static str, glyph: gpui_kit::Svg| {
            div()
                .id(id)
                .size(px(46.))
                .rounded(px(CARD_RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0x00000066)))
                .child(glyph)
        };
        let mut row = div().flex_shrink_0().flex().items_center().gap(px(8.));

        if item.is_playable() || item.is_series() {
            let resumable = item.resume_secs() > 0;
            row = row.child(
                div()
                    .id("detail.play")
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
                icon(LucideIcon::Check, 25., if played { rgb(0xf5f5f7) } else { fg }),
            )
            .when(played, |el| el.bg(rgba(0xf5f5f733)))
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
            Menu::new(&self.more_menu, "More")
                .trigger_style_with(|button| button)
                .trigger(
                    div()
                        .id("detail.more")
                        .tooltip(tip("More"))
                        .size(px(46.))
                        .rounded(px(CARD_RADIUS))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(rgba(0x00000066)))
                        .child(filled(Filled::More, 25., fg)),
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
        let title = |kind: &str, n: usize| {
            item.streams(kind)
                .get(n)
                .and_then(|s| s.display_title.clone())
                .unwrap_or_else(|| format!("Track {}", n + 1))
        };
        let row = |name: &'static str| {
            div()
                .flex()
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
                .trigger_style_with(|button| button)
                .trigger(
                    div()
                        .w(px(width))
                        .h(px(34.))
                        .pl(px(9.))
                        .pr(px(6.))
                        .relative()
                        .rounded(px(8.))
                        .child(glass(px(8.), rgba(0xffffff1f)))
                        .border_1()
                        .border_color(rgba(0xffffff33))
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
                let longest = (0..audio.len())
                    .map(|n| title("Audio", n).chars().count())
                    .max()
                    .unwrap_or(0);
                row("Audio").child(select(
                    &self.detail_audio_menu,
                    "Audio",
                    title("Audio", current),
                    longest,
                ))
            } else {
                row("Audio")
                    .h(px(34.))
                    .child(div().text_color(value).child(title("Audio", current)))
            });
        }
        let subtitles = item.streams("Subtitle");
        if !subtitles.is_empty() {
            let current = data
                .subtitle
                .unwrap_or_else(|| subtitles.iter().position(|s| s.is_default));
            let text = match current {
                Some(n) => title("Subtitle", n),
                None => "Off".to_string(),
            };
            let longest = (0..subtitles.len())
                .map(|n| title("Subtitle", n).chars().count())
                .max()
                .unwrap_or(0);
            rows = rows.child(row("Subtitles").child(select(
                &self.detail_subtitle_menu,
                "Subtitles",
                text,
                longest,
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
        let w = (self.viewport_w * 0.10 - 9.).round();
        let h = (self.viewport_w * 0.14 - 9.).round();
        let mut rows = Vec::new();
        for (id, title, guests) in [
            ("detail.cast", "Cast & Crew", false),
            ("detail.guests", "Guest Stars", true),
        ] {
            let people = merged_people(&data.item.people, guests);
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

/// Height to width ratio of the detail poster.
fn poster_ratio(item: &Item) -> f32 {
    if item.kind == "Episode" { 9. / 16. } else { 1.5 }
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
        .filter(|_| !matches!(item.kind.as_str(), "Season" | "Episode"))
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
                .child(filled(Filled::Star, 18., rgba(0xf5f5f7cc)))
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
        if let Some(name) = stream.language.as_deref().and_then(language_name)
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

/// English name of a three-letter language code; `None` for unknown codes.
fn language_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "eng" => "English",
        "spa" => "Spanish",
        "fre" | "fra" => "French",
        "ger" | "deu" => "German",
        "ita" => "Italian",
        "por" => "Portuguese",
        "jpn" => "Japanese",
        "kor" => "Korean",
        "chi" | "zho" => "Chinese",
        "rus" => "Russian",
        "hin" => "Hindi",
        "ara" => "Arabic",
        "dut" | "nld" => "Dutch",
        "swe" => "Swedish",
        "nor" => "Norwegian",
        "dan" => "Danish",
        "fin" => "Finnish",
        "pol" => "Polish",
        "tur" => "Turkish",
        _ => return None,
    })
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
                            div()
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

/// Cast in credit order. The crew roles of one person join into one card.
fn merged_people(people: &[Person], guests: bool) -> Vec<(Person, String)> {
    let mut out: Vec<(Person, String)> = Vec::new();
    for person in people {
        if (person.kind == "GuestStar") != guests {
            continue;
        }
        let role = match person.kind.as_str() {
            "Actor" | "GuestStar" => person
                .role
                .clone()
                .filter(|r| !r.is_empty())
                .map(|r| format!("as {r}"))
                .unwrap_or_default(),
            other => other.to_string(),
        };
        match out.iter_mut().find(|(p, _)| p.id == person.id) {
            Some((_, roles)) if !role.is_empty() && !roles.contains(&role) => {
                roles.push_str(" / ");
                roles.push_str(&role);
            }
            Some(_) => {}
            None => out.push((person.clone(), role)),
        }
    }
    out
}

fn person_card(
    person: &Person,
    role: &str,
    client: &Client,
    w: f32,
    h: f32,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    let t = UiTheme::read(cx).clone();
    let url = person
        .primary_image_tag
        .as_deref()
        .map(|tag| client.image_url(&person.id, "Primary", Some(tag), (w * 2.) as u32));
    div()
        .id(SharedString::from(format!("person.{}.{role}", person.id)))
        .w(px(w))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .cursor_pointer()
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
                        .text_color(t.colors.foreground)
                        .truncate()
                        .child(person.name.clone()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(t.colors.muted_foreground)
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

/// The episode rows of a season, built on demand: only the rows inside the
/// window (and one more at each side) are built and laid out for a frame.
/// Every row is `EPISODE_ROW_H` tall, so the element knows its height and
/// the place of each row without building the rest; a season of 150
/// episodes then costs as much per frame as one of 8.
struct EpisodeRows {
    app: WeakEntity<Bloom>,
    count: usize,
}

impl IntoElement for EpisodeRows {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for EpisodeRows {
    type RequestLayoutState = ();
    type PrepaintState = Vec<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        Some("detail.episodes".into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let stride = EPISODE_ROW_H + EPISODE_ROW_GAP;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(stride * self.count as f32 - EPISODE_ROW_GAP).into();
        style.flex_shrink = 0.;
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let stride = EPISODE_ROW_H + EPISODE_ROW_GAP;
        // `bounds` is in window coordinates, so the page scroll is in it.
        let top = f32::from(bounds.origin.y);
        let view_h = f32::from(window.viewport_size().height);
        let first = ((-top / stride).floor() - 1.).max(0.) as usize;
        let last = ((((view_h - top) / stride).ceil() + 1.).max(0.) as usize).min(self.count);
        let first = first.min(last);
        let mut rows: Vec<AnyElement> = self
            .app
            .update(cx, |this, cx| {
                let Page::Detail(data) = &this.page else {
                    return Vec::new();
                };
                let Some(client) = this.session.as_ref().map(|s| s.client.clone()) else {
                    return Vec::new();
                };
                data.episodes
                    .iter()
                    .take(last)
                    .skip(first)
                    .map(|ep| episode_row(ep, &client, this.viewport_w, cx).into_any_element())
                    .collect()
            })
            .unwrap_or_default();
        let space = size(
            AvailableSpace::Definite(bounds.size.width),
            AvailableSpace::Definite(px(EPISODE_ROW_H)),
        );
        for (i, row) in rows.iter_mut().enumerate() {
            row.layout_as_root(space, window, cx);
            let y = bounds.origin.y + px(stride * (first + i) as f32);
            row.prepaint_at(point(bounds.origin.x, y), window, cx);
        }
        rows
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        rows: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for row in rows {
            row.paint(window, cx);
        }
    }
}

/// One episode of a season: still, number and name, details, overview.
fn episode_row(ep: &Item, client: &Client, vw: f32, cx: &mut Context<Bloom>) -> Stateful<Div> {
    let t = UiTheme::read(cx).clone();
    let w = (vw * 0.195).round();
    let h = 160.;
    let still = ep
        .image_tags
        .primary
        .as_deref()
        .map(|tag| client.image_url(&ep.id, "Primary", Some(tag), 500))
        .or_else(|| ep.wide_url(client, 500));
    let name = match ep.index_number {
        Some(n) => format!("{n}. {}", ep.name),
        None => ep.name.clone(),
    };
    let mut details: Vec<String> = Vec::new();
    if let Some(secs) = ep.runtime_secs() {
        details.push(format_runtime(secs));
    }
    if let Some(score) = ep.community_rating {
        details.push(format!("★ {score:.1}"));
    }
    let (open, play) = (ep.clone(), ep.clone());
    let resume = ep.resume_secs() > 0;
    let played = ep.user_data.played;
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
                .when_some(ep.progress(), |el, value| {
                    el.child(
                        div()
                            .absolute()
                            .bottom(px(7.))
                            .left(px(12.))
                            .right(px(12.))
                            .h(px(7.))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(rgba(0x00000059))
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .w(relative(value))
                                    .bg(rgba(0xf5f5f7f2)),
                            ),
                    )
                })
                .when(played, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top(px(5.))
                            .right(px(5.))
                            .size(px(26.))
                            .rounded(px(10.))
                            .bg(rgb(0xf5f5f7))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(LucideIcon::Check, 16., rgb(0x121212))),
                    )
                })
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
                        .text_color(t.colors.foreground)
                        .truncate()
                        .child(name),
                )
                .when(!details.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(14.))
                            .text_color(t.colors.muted_foreground)
                            .child(details.join("   ")),
                    )
                })
                .when_some(ep.overview.clone().filter(|o| !o.is_empty()), |el, text| {
                    el.child(
                        div()
                            .text_size(px(14.))
                            .line_height(relative(1.35))
                            .text_color(t.colors.muted_foreground)
                            .line_clamp(4)
                            .child(text),
                    )
                }),
        )
}

