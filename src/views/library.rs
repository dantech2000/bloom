// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Library grid and search results.

use crate::ui::tone;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgba,
};

use crate::{
    app::{Bloom, LibraryShow, PAGE_SIZE, Page},
    icons::{Filled, filled},
    jellyfin::ItemKind,
    ui::{
        glass::glass,
        menu::Menu,
        motion::{Enter, enter},
        scroll_area::ScrollArea,
        theme::UiTheme,
    },
    views::{
        cards::{empty_state, grid, icon, skeleton_row},
        parts::{TOOL_HOVER, icon_button, icon_menu, menu_over, play_all_pill},
    },
};
use crate::ui::tip::tip;

/// The letters of the rail at the right edge: "#" for a title that starts
/// with no letter, then the alphabet.
const LETTERS: [&str; 27] = [
    "#", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
    "S", "T", "U", "V", "W", "X", "Y", "Z",
];

impl Bloom {
    pub fn render_library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Library(data) = &self.nav.page else {
            unreachable!()
        };
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return div();
        };
        let colors = UiTheme::read(cx).colors;
        let m = self.metrics();
        let fg = colors.foreground;
        let soft = rgba(0xf5f5f7c7);
        // A view of lists or studios has no titles to play in a row.
        let playable = matches!(
            data.view.collection_type.as_deref(),
            Some("movies") | Some("tvshows")
        ) && !data.show.lists_groups();
        // `/Studios` sorts by name and has no filters of a title.
        let studios = data.show == LibraryShow::Studios;

        let range = if data.total == 0 {
            String::new()
        } else {
            let last = (data.start + data.items.len()).max(data.start + 1);
            format!("{}-{} of {}", data.start + 1, last.min(data.total), data.total)
        };

        // On a phone the toolbar takes two rows: the view with the range
        // and the pages, then the actions. Its tools are 44 points there,
        // the size a thumb needs; 38 on the Mac.
        let compact = self.compact_w();
        let tool_size = if compact { 44. } else { 38. };
        let has_previous = data.start > 0;
        let has_next = data.start + PAGE_SIZE < data.total;
        let page_arrow = |id: &'static str, glyph: LucideIcon, enabled: bool, forward: bool| {
            let label = if forward { "Next page" } else { "Previous page" };
            icon_button(id, label, icon(glyph, 22., fg), tool_size, TOOL_HOVER)
                .tooltip(tip(label))
                .when(!enabled, |el| el.opacity(0.3))
                .when(enabled, |el| {
                    el.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.update_library(
                            |d| {
                                d.start = if forward {
                                    d.start + PAGE_SIZE
                                } else {
                                    d.start.saturating_sub(PAGE_SIZE)
                                }
                            },
                            cx,
                        )
                    }))
                })
        };

        let play_all = play_all_pill(
            ("library.play-all", "library.shuffle"),
            tool_size,
            cx.listener(|this, _: &ClickEvent, window, cx| this.play_library(false, window, cx)),
            cx.listener(|this, _: &ClickEvent, window, cx| this.play_library(true, window, cx)),
            cx,
        );

        let menu = |state: &gpui_kit::Entity<crate::ui::menu::MenuState>,
                    label: &'static str,
                    glyph: LucideIcon,
                    active: bool| {
            menu_over(
                state,
                label,
                icon_button(label, label, icon(glyph, 20., fg), tool_size, TOOL_HOVER)
                    .tooltip(tip(label))
                    // A filter or a sort that is not the usual one lights
                    // its button.
                    .when(active, |el| el.bg(rgba(TOOL_HOVER))),
            )
        };

        let toolbar_h = if compact { 98. } else { 54. };
        let views = !shows_of(&data.view).is_empty();
        // Each filter that narrows the page as a pill; a click on it drops
        // that filter. The Mac only: a phone's toolbar has no room.
        let pills = (!compact).then(|| {
            div()
                .flex()
                .flex_shrink(1.)
                .min_w_0()
                .overflow_hidden()
                .items_center()
                .gap(px(6.))
                .children(filter_pills(data).into_iter().map(|(n, label, clear)| {
                    div()
                        .id(("library.pill", n))
                        .role(gpui_kit::Role::Button)
                        .aria_label(format!("Remove filter {label}"))
                        .flex_shrink_0()
                        .h(px(30.))
                        .pl(px(12.))
                        .pr(px(8.))
                        .rounded_full()
                        .bg(tone::HOVER)
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(fg)
                        .hover(|s| s.bg(rgba(0xffffff33)))
                        .tooltip(tip(format!("Remove the filter “{label}”")))
                        .child(label)
                        .child(icon(LucideIcon::X, 14., soft))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.update_library(clear, cx)
                        }))
                }))
        });
        let toolbar = div()
            .px(px(m.side))
            .h(px(toolbar_h))
            .flex()
            .when(compact, |el| el.flex_wrap())
            .items_center()
            .gap(px(if compact { 10. } else { 18. }))
            .child({
                // The Mac: the title, which opens the views of the library
                // when it has some. A phone has the title in the top bar,
                // so the row holds the view as a small pill instead (the
                // title stood twice, 2026-10-23), or nothing without views.
                if compact {
                    if views {
                        Menu::new(&self.popups.show_menu, "View")
                            .trigger(
                                div()
                                    .id("library.view")
                                    .role(gpui_kit::Role::Button)
                                    .aria_label("View")
                                    .h(px(36.))
                                    .pl(px(14.))
                                    .pr(px(8.))
                                    .rounded_full()
                                    .bg(tone::HOVER)
                                    .flex()
                                    .items_center()
                                    .gap(px(2.))
                                    .text_size(px(15.))
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(fg)
                                    .child(data.show.label(&data.title))
                                    .child(filled(Filled::DropDown, 22., soft)),
                            )
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }
                } else {
                    let title = div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_size(px(22.))
                        .text_color(soft)
                        .child(data.show.label(&data.title));
                    if views {
                        Menu::new(&self.popups.show_menu, "View")
                            .trigger(title.child(filled(Filled::DropDown, 24., soft)))
                            .into_any_element()
                    } else {
                        title.into_any_element()
                    }
                }
            })
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(soft)
                    .child(if data.loading && data.items.is_empty() {
                        "∙".to_string()
                    } else {
                        range.clone()
                    }),
            )
            .children(pills)
            .child(div().flex_1())
            .when(compact, |el| {
                el.child(page_arrow("library.previous", LucideIcon::ChevronLeft, has_previous, false))
                    .child(page_arrow("library.next", LucideIcon::ChevronRight, has_next, true))
                    // The second row starts here.
                    .child(div().w_full().h_0())
            })
            .when(playable, |el| el.child(play_all))
            .when(compact, |el| el.child(div().flex_1()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    // The page of a collection has a menu of its own.
                    .when(data.view.kind == ItemKind::BoxSet, |el| {
                        el.child(icon_menu(
                            &self.popups.more_menu,
                            "library.more",
                            "More",
                            filled(Filled::More, 22., fg),
                            tool_size,
                            TOOL_HOVER,
                        ))
                    })
                    .when(!studios, |el| {
                        el.child(menu(
                            &self.popups.filter_menu,
                            "Filter",
                            LucideIcon::Funnel,
                            !filter_pills(data).is_empty(),
                        ))
                        .child(menu(
                            &self.popups.sort_menu,
                            "Sort",
                            LucideIcon::ArrowDownAZ,
                            data.sort != "SortName" || data.descending,
                        ))
                    })
                    .when(!compact, |el| {
                        el.child(page_arrow(
                            "library.previous",
                            LucideIcon::ChevronLeft,
                            has_previous,
                            false,
                        ))
                        .child(page_arrow(
                            "library.next",
                            LucideIcon::ChevronRight,
                            has_next,
                            true,
                        ))
                    }),
            );

        // The pages again under the grid: a page of a hundred is ten screens
        // on a phone, and the arrows at the top were the only way on.
        let footer = (data.total > PAGE_SIZE && !(data.loading && data.items.is_empty())).then(|| {
            div()
                .px(px(m.side))
                .pt(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(14.))
                .child(page_arrow("library.previous.end", LucideIcon::ChevronLeft, has_previous, false))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(soft)
                        .child(range.clone()),
                )
                .child(page_arrow("library.next.end", LucideIcon::ChevronRight, has_next, true))
        });

        let body = if data.loading && data.items.is_empty() {
            skeleton_row(
                self,
                "sk.library",
                m.grid_columns,
                m.grid_w,
                m.grid_w * 1.5,
                cx,
            )
        } else if data.items.is_empty() {
            self.library_empty(data, cx)
        } else {
            // The grid starts under the toolbar and its gap. A new page of
            // it lifts into place (`ui::motion`); the key names the page,
            // so a redraw of the same page runs nothing.
            let key = SharedString::from(format!("library.grid.{}.{:?}.{}", data.view.id, data.show, data.start));
            div()
                .child(enter(grid(self, &data.items, &client, toolbar_h + 12., cx), key, Enter::Lift, cx))
        };

        // Letter picker at the right edge, as on the web. It needs a tall
        // window and a sort by name.
        // An empty page with no letter picked has nothing to pick from.
        let has_titles = !data.items.is_empty() || data.letter.is_some();
        let picker = (data.sort == "SortName" && self.viewport_h >= 610. && !compact && has_titles).then(|| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(6.))
                .flex()
                .flex_col()
                .justify_center()
                .child(
                    div()
                        .relative()
                        .p(px(2.))
                        .rounded_full()
                        .child(glass(px(999.), rgba(0x2a2a2a66)))
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .font_family(UiTheme::read(cx).fonts.mono.clone())
                        .text_size(px(11.))
                        .children(LETTERS.iter().enumerate().map(|(n, text)| {
                            let letter = text.chars().next().unwrap_or('#');
                            let selected = data.letter == Some(letter);
                            div()
                                .id(("library.letter", n))
                                .w(px(22.))
                                .h(px(16.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .text_color(if selected { colors.primary_foreground } else { soft })
                                .when(selected, |el| el.bg(colors.primary))
                                .when(!selected, |el| el.hover(|s| s.bg(tone::EDGE)))
                                .child(*text)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.update_library(
                                        |d| {
                                            d.letter =
                                                (d.letter != Some(letter)).then_some(letter)
                                        },
                                        cx,
                                    )
                                }))
                        })),
                )
        });

        div()
            .relative()
            .size_full()
            .child(
                ScrollArea::new("library.scroll")
                    .track(&self.nav.scroll)
                    .size_full()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .pb(px(72.))
                            .child(toolbar)
                            .child(body)
                            .children(footer),
                    ),
            )
            .children(picker)
    }
}

use crate::{
    app::{LIBRARY_FILTERS, LIBRARY_SORTS, LibraryData, library_query},
    jellyfin::Item,
    lists::{ListKind, PLAYLISTS_ID},
    pages::{DECADES, GENRE_VIEW, Resolution, decade_label},
    ui::menu::MenuItem,
};
use gpui_kit::Window;

/// The views of a library for the menu of its title. The page of the
/// playlists has the collections as its second view, so both lists are
/// one tab of the bar away.
pub(crate) fn shows_of(view: &Item) -> &'static [LibraryShow] {
    if view.id == PLAYLISTS_ID {
        return &[LibraryShow::All, LibraryShow::Collections];
    }
    LibraryShow::of(view.collection_type.as_deref())
}

/// A filter's name in the menu and on its pill, by its server key.
fn filter_label(key: &str) -> &'static str {
    LIBRARY_FILTERS
        .iter()
        .find(|(_, k)| *k == key)
        .map_or("", |(label, _)| label)
}

/// What drops one filter of a pill.
type Clear = fn(&mut LibraryData);

/// The filters that narrow the page, as pills: a number for the id, the
/// label, and what drops it. The letter has the rail; it is no pill.
pub(crate) fn filter_pills(data: &LibraryData) -> Vec<(usize, String, Clear)> {
    let mut pills: Vec<(usize, String, Clear)> = Vec::new();
    if let Some(key) = data.filter {
        pills.push((0, filter_label(key).to_string(), |d| d.filter = None));
    }
    if let Some((_, name)) = &data.genre {
        pills.push((1, name.clone(), |d| d.genre = None));
    }
    if let Some(decade) = data.decade {
        pills.push((2, decade_label(decade), |d| d.decade = None));
    }
    if let Some(resolution) = data.resolution {
        pills.push((3, resolution.label().to_string(), |d| d.resolution = None));
    }
    if data.subtitles {
        pills.push((4, "Subtitles".to_string(), |d| d.subtitles = false));
    }
    if let Some((_, name)) = &data.language.audio {
        pills.push((5, format!("{name} audio"), |d| d.language.audio = None));
    }
    if let Some((_, name)) = &data.language.subtitle {
        pills.push((6, format!("{name} subtitles"), |d| d.language.subtitle = None));
    }
    pills
}

/// The submenu of one track language (Jellyfin 12.0): "Any" first, then
/// each language of the library's tracks. `pick` sets the page's choice.
fn language_menu(
    kind: &'static str,
    choices: Option<&[bloom_core::jellyfin::LanguageChoice]>,
    current: Option<&(String, String)>,
    pick: fn(&mut LibraryData, Option<(String, String)>),
    this: &gpui_kit::WeakEntity<Bloom>,
) -> MenuItem {
    let entry = |id: String, label: String, value: Option<(String, String)>| {
        let handle = this.clone();
        MenuItem::new(SharedString::from(id), label).on_click(move |_, _, cx| {
            let value = value.clone();
            handle.update(cx, move |this, cx| this.update_library(|d| pick(d, value), cx)).ok();
        })
    };
    let mut entries = vec![entry(format!("library.{kind}-language.any"), "Any language".into(), None).radio(current.is_none())];
    for choice in choices.unwrap_or_default() {
        let selected = current.is_some_and(|(code, _)| *code == choice.code);
        entries.push(
            entry(
                format!("library.{kind}-language.{}", choice.code),
                choice.name.clone(),
                Some((choice.code.clone(), choice.name.clone())),
            )
            .radio(selected),
        );
    }
    let title = if kind == "audio" { "Audio language" } else { "Subtitle language" };
    let label = match current {
        Some((_, name)) => format!("{title}: {name}"),
        None if choices.is_none() => format!("{title} (loading…)"),
        None => title.to_string(),
    };
    let icon = if kind == "audio" { LucideIcon::AudioLines } else { LucideIcon::Languages };
    MenuItem::submenu(SharedString::from(format!("library.{kind}-language")), label, entries).icon(icon)
}

/// The entries of the filter menu: the watch state as before, then the
/// genre, the decade and the resolution as submenus, "Has subtitles" as a
/// check, the audio and subtitle languages as submenus, each where it fits
/// the page, and "Clear filters".
fn filter_menu_items(data: &LibraryData, this: &gpui_kit::WeakEntity<Bloom>) -> Vec<MenuItem> {
    // An entry that changes the page and reloads it.
    let entry = |id: String, label: String, change: Box<dyn Fn(&mut LibraryData)>| {
        let handle = this.clone();
        let change: std::rc::Rc<dyn Fn(&mut LibraryData)> = change.into();
        MenuItem::new(SharedString::from(id), label).on_click(move |_, _, cx| {
            let change = change.clone();
            handle.update(cx, move |this, cx| this.update_library(|d| change(d), cx)).ok();
        })
    };
    let mut items = vec![entry("library.filter.none".into(), "All".into(), Box::new(|d| d.filter = None))
        .radio(data.filter.is_none())];
    for (label, key) in LIBRARY_FILTERS {
        items.push(
            entry(format!("library.filter.{key}"), label.into(), Box::new(move |d| d.filter = Some(key)))
                .radio(data.filter == Some(key)),
        );
    }
    let genre_page = data.view.collection_type.as_deref() == Some(GENRE_VIEW);
    let mut more = Vec::new();
    if data.can_narrow_by_genre() && !genre_page {
        let genres = data.genres.as_deref().unwrap_or_default();
        let mut entries = vec![entry("library.genre.any".into(), "Any genre".into(), Box::new(|d| d.genre = None))
            .radio(data.genre.is_none())];
        for genre in genres {
            let picked = (genre.id.clone(), genre.name.clone());
            let selected = data.genre.as_ref().is_some_and(|(id, _)| *id == genre.id);
            entries.push(
                entry(format!("library.genre.{}", genre.id), genre.name.clone(), Box::new(move |d| d.genre = Some(picked.clone())))
                    .radio(selected),
            );
        }
        let label = match &data.genre {
            Some((_, name)) => format!("Genre: {name}"),
            None if data.genres.is_none() => "Genre (loading…)".to_string(),
            None => "Genre".to_string(),
        };
        more.push(MenuItem::submenu("library.genre", label, entries).icon(LucideIcon::Tag));
    }
    if data.can_narrow_by_genre() {
        let mut entries = vec![entry("library.decade.any".into(), "Any decade".into(), Box::new(|d| d.decade = None))
            .radio(data.decade.is_none())];
        for decade in DECADES {
            entries.push(
                entry(format!("library.decade.{decade}"), decade_label(decade), Box::new(move |d| d.decade = Some(decade)))
                    .radio(data.decade == Some(decade)),
            );
        }
        let label = match data.decade {
            Some(decade) => format!("Decade: {}", decade_label(decade)),
            None => "Decade".to_string(),
        };
        more.push(MenuItem::submenu("library.decade", label, entries).icon(LucideIcon::CalendarDays));
    }
    if data.can_narrow_by_streams() {
        let mut entries = vec![entry("library.resolution.any".into(), "Any resolution".into(), Box::new(|d| d.resolution = None))
            .radio(data.resolution.is_none())];
        for resolution in Resolution::ALL {
            let detail = match resolution {
                Resolution::Uhd => "2160p",
                Resolution::Hd => "720p and up, 4K too",
                Resolution::Sd => "Below 720p",
            };
            entries.push(
                entry(
                    format!("library.resolution.{resolution:?}"),
                    resolution.label().into(),
                    Box::new(move |d| d.resolution = Some(resolution)),
                )
                .detail(detail)
                .radio(data.resolution == Some(resolution)),
            );
        }
        let label = match data.resolution {
            Some(resolution) => format!("Resolution: {}", resolution.label()),
            None => "Resolution".to_string(),
        };
        more.push(MenuItem::submenu("library.resolution", label, entries).icon(LucideIcon::MonitorPlay));
        let subtitles = data.subtitles;
        more.push(
            entry("library.subtitles".into(), "Has subtitles".into(), Box::new(move |d| d.subtitles = !subtitles))
                .icon(LucideIcon::Captions)
                .checked(subtitles),
        );
    }
    // The track languages: the server's lists, so a server before 12.0
    // (no lists) gets no entries once they load.
    if data.can_narrow_by_language() {
        let languages = data.language.choices.as_ref();
        if languages.is_none_or(|l| !l.audio.is_empty()) || data.language.audio.is_some() {
            more.push(language_menu(
                "audio",
                languages.map(|l| l.audio.as_slice()),
                data.language.audio.as_ref(),
                |d, value| d.language.audio = value,
                this,
            ));
        }
        if languages.is_none_or(|l| !l.subtitles.is_empty()) || data.language.subtitle.is_some() {
            more.push(language_menu(
                "subtitle",
                languages.map(|l| l.subtitles.as_slice()),
                data.language.subtitle.as_ref(),
                |d, value| d.language.subtitle = value,
                this,
            ));
        }
    }
    if !more.is_empty() {
        items.push(MenuItem::separator());
        items.extend(more);
    }
    if !filter_pills(data).is_empty() {
        items.push(MenuItem::separator());
        items.push(entry("library.filter.clear".into(), "Clear filters".into(), Box::new(|d| {
            let letter = d.letter;
            d.clear_filters();
            d.letter = letter;
        })));
    }
    items
}

/// The words of an empty page: a title, a hint, and the button that helps,
/// if one does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum EmptyAction {
    ClearFilters,
    NewList(ListKind),
}

/// Shows another view of the library. The studios have no filters of a
/// title, so those go (the letter stays: `/Studios` takes it too).
pub(crate) fn set_show(data: &mut LibraryData, show: LibraryShow) {
    data.show = show;
    if show == LibraryShow::Studios {
        let letter = data.letter;
        data.clear_filters();
        data.letter = letter;
        (data.sort, data.descending) = ("SortName", false);
    }
}

pub(crate) fn empty_words(data: &LibraryData, can_collect: bool) -> (&'static str, &'static str, Option<EmptyAction>) {
    if data.narrowed() {
        return (
            "Nothing matches",
            "Remove a filter or the letter to see more.",
            Some(EmptyAction::ClearFilters),
        );
    }
    match data.show {
        LibraryShow::Collections => (
            "No collections yet",
            "A collection groups titles that belong together, such as a film series.",
            // A library lists the collections with its titles, which a new
            // (empty) one is not: the button is on the page of all lists.
            (can_collect && data.view.id == PLAYLISTS_ID).then_some(EmptyAction::NewList(ListKind::Collection)),
        ),
        _ if data.view.id == PLAYLISTS_ID => (
            "No playlists yet",
            "A playlist plays titles in the order you choose. Make one here, or use Add to playlist in the menu of a title.",
            Some(EmptyAction::NewList(ListKind::Playlist)),
        ),
        LibraryShow::Playlists => (
            "No playlists yet",
            // A new playlist is empty and would not show here: no button.
            "A playlist with a title of this library shows here. Use Add to playlist in the menu of a title.",
            None,
        ),
        LibraryShow::Studios => ("No studios", "The titles of this library name no studio.", None),
        LibraryShow::Favorites => ("No favorites yet", "Click the heart of a title to keep it here.", None),
        _ if data.view.id == crate::app::FAVORITES_ID => {
            ("No favorites yet", "Click the heart of a title to keep it here.", None)
        }
        _ => ("Nothing here yet", "This library has no titles yet.", None),
    }
}

// The menus of the toolbar, and Play of the library.
impl Bloom {
    /// Changes the library view (sort, filter, letter or page) and reloads it.
    pub(crate) fn update_library(&mut self, change: impl FnOnce(&mut LibraryData), cx: &mut Context<Self>) {
        if let Page::Library(data) = &mut self.nav.page {
            let start = data.start;
            change(data);
            // A new sort, filter or letter starts again at the first page.
            if data.start == start {
                data.start = 0;
            }
            self.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            self.rebuild_library_menus(cx);
            self.load_page(cx);
            cx.notify();
        }
    }

    /// Rebuilds the sort and filter dropdowns of the library toolbar.
    pub(crate) fn rebuild_library_menus(&mut self, cx: &mut Context<Self>) {
        let Page::Library(data) = &self.nav.page else {
            return;
        };
        let this = cx.weak_entity();
        let mut sorts: Vec<MenuItem> = LIBRARY_SORTS
            .iter()
            .map(|(label, key)| {
                let handle = this.clone();
                let key: &'static str = key;
                MenuItem::new(SharedString::from(format!("library.sort.{key}")), *label)
                    .radio(data.sort == key)
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |this, cx| this.update_library(|d| d.sort = key, cx))
                            .ok();
                    })
            })
            .collect();
        sorts.push(MenuItem::separator());
        for (label, descending) in [("Ascending", false), ("Descending", true)] {
            let handle = this.clone();
            sorts.push(
                MenuItem::new(SharedString::from(format!("library.order.{label}")), label)
                    .radio(data.descending == descending)
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |this, cx| {
                                this.update_library(|d| d.descending = descending, cx)
                            })
                            .ok();
                    }),
            );
        }
        let filters = filter_menu_items(data, &this);
        let library = data.title.clone();
        let shows: Vec<MenuItem> = shows_of(&data.view)
            .iter()
            .map(|show| {
                let (show, handle) = (*show, this.clone());
                MenuItem::new(
                    SharedString::from(format!("library.show.{show:?}")),
                    show.label(&library),
                )
                .radio(data.show == show)
                .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| this.update_library(|d| set_show(d, show), cx))
                        .ok();
                })
            })
            .collect();
        self.popups.show_menu.update(cx, |menu, cx| menu.set_items(shows, cx));
        self.popups.sort_menu.update(cx, |menu, cx| menu.set_items(sorts, cx));
        self.popups.filter_menu
            .update(cx, |menu, cx| menu.set_items(filters, cx));
        self.rebuild_collection_menu(cx);
    }

    /// The `browse` debug command: the tab the bar lights, and the filters,
    /// genres and lists of the browsing pages, without the pointer.
    ///   browse tab | pills
    ///   browse filter genre <name> | decade <year> | resolution 4k|hd|sd
    ///                 | subtitles on|off | watch <server key>|none | clear
    ///                 | audio-language|subtitle-language <code or name>|any
    ///   browse genre <name>          open the titles of a genre
    ///   browse playlists [collections]
    ///   browse new-list playlist|collection   (opens the dialog only)
    pub(crate) fn debug_browse(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let mut words = rest.split_whitespace();
        let verb = words.next().unwrap_or("");
        let arg: Vec<&str> = words.collect();
        let text = arg.join(" ");
        let pills = |this: &Self| match &this.nav.page {
            Page::Library(data) => filter_pills(data).into_iter().map(|(_, label, _)| label).collect::<Vec<_>>().join(", "),
            _ => String::new(),
        };
        match verb {
            "tab" => return self.active_tab().unwrap_or_else(|| "none".into()),
            "pills" => return pills(self),
            "filter" => {
                let Page::Library(data) = &self.nav.page else {
                    return "error: not on a library page".into();
                };
                let value = arg.get(1..).map(|rest| rest.join(" ")).unwrap_or_default();
                let genre = data
                    .genres
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .find(|g| g.name.eq_ignore_ascii_case(&value))
                    .map(|g| (g.id.clone(), g.name.clone()));
                match arg.first().copied().unwrap_or("") {
                    "genre" => match genre {
                        Some(genre) => self.update_library(|d| d.genre = Some(genre), cx),
                        None => return format!("error: no genre {value:?} in this library"),
                    },
                    "decade" => match value.parse::<i32>() {
                        Ok(year) => self.update_library(|d| d.decade = Some(year - year % 10), cx),
                        Err(_) => return "error: usage: browse filter decade <year>".into(),
                    },
                    "resolution" => {
                        let resolution = match value.to_lowercase().as_str() {
                            "4k" => Resolution::Uhd,
                            "hd" => Resolution::Hd,
                            "sd" => Resolution::Sd,
                            _ => return "error: usage: browse filter resolution 4k|hd|sd".into(),
                        };
                        self.update_library(|d| d.resolution = Some(resolution), cx)
                    }
                    "subtitles" => self.update_library(|d| d.subtitles = value != "off", cx),
                    // A track language by its code or name, as the menu
                    // lists it; "any" drops it.
                    kind @ ("audio-language" | "subtitle-language") => {
                        let choices = data.language.choices.clone().unwrap_or_default();
                        let list = if kind == "audio-language" { choices.audio } else { choices.subtitles };
                        let picked = match value.as_str() {
                            "any" => None,
                            _ => match list.iter().find(|c| c.code.eq_ignore_ascii_case(&value) || c.name.eq_ignore_ascii_case(&value)) {
                                Some(choice) => Some((choice.code.clone(), choice.name.clone())),
                                None => return format!("error: no {kind} {value:?} in this library"),
                            },
                        };
                        if kind == "audio-language" {
                            self.update_library(|d| d.language.audio = picked, cx)
                        } else {
                            self.update_library(|d| d.language.subtitle = picked, cx)
                        }
                    }
                    "watch" => {
                        let key = LIBRARY_FILTERS.iter().map(|(_, k)| *k).find(|k| *k == value);
                        self.update_library(|d| d.filter = key, cx)
                    }
                    "clear" => self.update_library(|d| d.clear_filters(), cx),
                    _ => return "error: usage: browse filter genre|decade|resolution|subtitles|audio-language|subtitle-language|watch|clear ...".into(),
                }
                return pills(self);
            }
            "genre" => {
                let Page::Home(data) = &self.nav.page else {
                    return "error: open home first".into();
                };
                match data.genres.iter().find(|g| g.name.eq_ignore_ascii_case(&text)).cloned() {
                    Some(genre) => self.open_genre(&genre, cx),
                    None => return format!("error: no genre {text:?} on the home page"),
                }
            }
            "playlists" => {
                self.open_playlists(cx);
                if text == "collections" {
                    self.update_library(|d| d.show = LibraryShow::Collections, cx);
                }
            }
            // With a name (a test name only), the dialog's Create runs too.
            "new-list" => {
                let kind = if arg.first() == Some(&"collection") { ListKind::Collection } else { ListKind::Playlist };
                match arg.get(1) {
                    None => self.ask_new_list(kind, window, cx),
                    Some(name) if crate::lists::is_test_name(name) => {
                        self.create_empty_list(kind, name.to_string(), cx)
                    }
                    Some(_) => return format!("error: the name must start with {}", crate::lists::TEST_PREFIX),
                }
            }
            _ => return "error: usage: browse tab|pills|filter|genre|playlists|new-list ...".into(),
        }
        cx.notify();
        "ok".into()
    }

    /// The empty page of a library: why it is empty, in plain words, and
    /// the button that helps (clear the filters, make a list).
    fn library_empty(&self, data: &LibraryData, cx: &mut Context<Self>) -> gpui_kit::Div {
        let (title, hint, action) = empty_words(data, self.lists.can_collect);
        let mut state = empty_state(title, hint, cx);
        if let Some(action) = action {
            let (id, label) = match action {
                EmptyAction::ClearFilters => ("library.clear-filters", "Clear filters"),
                EmptyAction::NewList(ListKind::Playlist) => ("library.new-playlist", "New playlist"),
                EmptyAction::NewList(ListKind::Collection) => ("library.new-collection", "New collection"),
            };
            state = state.child(
                div().mt(px(12.)).child(crate::ui::button::raised(id, label, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| match action {
                        EmptyAction::ClearFilters => this.update_library(|d| d.clear_filters(), cx),
                        EmptyAction::NewList(kind) => this.ask_new_list(kind, window, cx),
                    },
                ))),
            );
        }
        state
    }

    /// Plays the first item of the current library view, or a random one.
    pub(crate) fn play_library(&mut self, shuffle: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Library(data) = &self.nav.page else {
            return;
        };
        let mut query = library_query(data);
        query.start = Some(0);
        // The first item plays; the others wait in the queue.
        query.limit = Some(30);
        if shuffle {
            query.sort_by = Some("Random".to_string());
        }
        let generation = self.nav.generation;
        cx.spawn_in(window, async move |this, cx| {
            let Ok(client) = this.read_with(cx, |this, _| {
                this.session.as_ref().map(|s| s.client.clone())
            }) else {
                return;
            };
            let Some(client) = client else { return };
            let result = cx
                .background_executor()
                .spawn(async move { client.items(&query) })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.nav.generation != generation {
                    return;
                }
                match result {
                    // A series in the queue plays its episodes when its turn
                    // comes; see `expand_series`.
                    Ok(page) => this.start_queue(page.items, window, cx),
                    Err(err) => this.toast("Could not start playback", format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: &str, collection: &str) -> LibraryData {
        LibraryData::new(Item { id: id.into(), collection_type: Some(collection.into()), ..Default::default() })
    }

    #[test]
    fn each_filter_is_a_pill_that_drops_it() {
        let mut movies = page("m", "movies");
        assert!(filter_pills(&movies).is_empty());
        movies.filter = Some("IsUnplayed");
        movies.genre = Some(("g".into(), "Drama".into()));
        movies.decade = Some(1990);
        movies.resolution = Some(Resolution::Uhd);
        movies.subtitles = true;
        movies.language.audio = Some(("jpn".into(), "Japanese".into()));
        movies.language.subtitle = Some(("eng".into(), "English".into()));
        movies.letter = Some('K');
        let labels: Vec<String> = filter_pills(&movies).into_iter().map(|(_, label, _)| label).collect();
        assert_eq!(
            labels,
            ["Unplayed", "Drama", "1990s", "4K", "Subtitles", "Japanese audio", "English subtitles"],
            "the letter has the rail"
        );
        for (_, _, clear) in filter_pills(&movies) {
            clear(&mut movies);
        }
        assert!(filter_pills(&movies).is_empty());
        assert_eq!(movies.letter, Some('K'));
    }

    /// The page of the playlists said "Change the filter or the letter to
    /// see more" with no playlist on the server (2026-11-18).
    #[test]
    fn an_empty_page_says_why_and_offers_what_helps() {
        let playlists = page(PLAYLISTS_ID, PLAYLISTS_ID);
        let (title, _, action) = empty_words(&playlists, false);
        assert_eq!((title, action), ("No playlists yet", Some(EmptyAction::NewList(ListKind::Playlist))));
        let mut collections = page(PLAYLISTS_ID, PLAYLISTS_ID);
        collections.show = LibraryShow::Collections;
        assert_eq!(empty_words(&collections, false).2, None, "no right to make one");
        assert_eq!(empty_words(&collections, true).2, Some(EmptyAction::NewList(ListKind::Collection)));
        let mut movies = page("m", "movies");
        movies.decade = Some(1920);
        assert_eq!(empty_words(&movies, true).2, Some(EmptyAction::ClearFilters));
        assert_eq!(shows_of(&playlists.view), [LibraryShow::All, LibraryShow::Collections]);
    }
}
