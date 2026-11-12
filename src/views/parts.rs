// SPDX-License-Identifier: AGPL-3.0-or-later
//! Small blocks the pages share: the icon button of a toolbar or the
//! player, the menu behind one, the "Play All" pill, and the click a panel
//! over the video takes.

use gpui_icons::LucideIcon;
use gpui_kit::{
    App, ClickEvent, Context, Div, Entity, InteractiveElement as _, MouseDownEvent,
    ParentElement as _, StatefulInteractiveElement as _, Styled, Svg, Window, div,
    px, rgba,
};

use crate::{
    app::Bloom,
    icons::{Filled, filled},
    jellyfin::Item,
    ui::{
        menu::{Menu, MenuState},
        theme::UiTheme,
        tip::tip,
    },
    views::cards::icon,
};

/// The hover fill of a tool on a page.
pub const TOOL_HOVER: u32 = 0xf5f5f71f;

/// A square button with one glyph: `size` points, round corners, `hover`
/// as its fill under the pointer. It carries the role and the label for
/// assistive tech and the tests that tap by label. A tooltip is the
/// caller's (an element takes one).
pub fn icon_button(
    id: &'static str,
    label: &'static str,
    glyph: Svg,
    size: f32,
    hover: u32,
) -> gpui_kit::base::Button {
    crate::ui::button::ghost_icon(id, label, glyph, size, rgba(hover))
}

/// The label of a button whose id ends in its name: "play" of
/// "detail.play".
pub fn last_part(id: &'static str) -> &'static str {
    id.rsplit('.').next().unwrap_or(id)
}

/// A menu whose trigger is `trigger` as it stands, with no button frame
/// around it.
pub fn menu_over(
    state: &Entity<MenuState>,
    label: &'static str,
    trigger: impl gpui_kit::IntoElement,
) -> Menu {
    Menu::new(state, label)
        .trigger(trigger)
}

/// A menu behind an [`icon_button`], with the label as its tooltip.
pub fn icon_menu(
    state: &Entity<MenuState>,
    id: &'static str,
    label: &'static str,
    glyph: Svg,
    size: f32,
    hover: u32,
) -> Menu {
    menu_over(state, label, icon_button(id, label, glyph, size, hover).tooltip(tip(label)))
}

/// "Play All" with "Shuffle" at its right, as one pill of the primary
/// colour. `ids` are the ids of the two halves, `h` the height.
pub fn play_all_pill(
    ids: (&'static str, &'static str),
    h: f32,
    on_play: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_shuffle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let colors = UiTheme::read(cx).colors;
    let half = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .role(gpui_kit::Role::Button)
            .aria_label(label)
            .h(px(h))
            .flex()
            .items_center()
            .cursor_pointer()
            .bg(colors.primary)
            .hover(|s| s.opacity(0.88))
    };
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .text_color(colors.primary_foreground)
        .text_size(px(14.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child(
            half(ids.0, "Play all")
                .pl(px(14.))
                .pr(px(16.))
                .rounded_l_full()
                .gap(px(6.))
                .child(filled(Filled::Play, 20., colors.primary_foreground))
                .child("Play All")
                .on_click(on_play),
        )
        .child(
            half(ids.1, "Shuffle")
                .tooltip(tip("Shuffle"))
                .px(px(14.))
                .rounded_r_full()
                .child(icon(LucideIcon::Shuffle, 18., colors.primary_foreground))
                .on_click(on_shuffle),
        )
}

/// The first letter of a name in capitals, for a picture that is missing.
pub fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}

impl Bloom {
    /// A press on something that lies over the video: it keeps the keys
    /// with the player and does not reach the video, which would pause.
    /// Hitboxes do not block by themselves.
    pub fn swallow_to_player(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.player_focus, cx);
        cx.stop_propagation();
    }

    /// Plays an item from a card or the hero: a series at the episode to
    /// watch next, anything else from where it was left.
    pub fn play_item(&mut self, item: &Item, window: &mut Window, cx: &mut Context<Self>) {
        if item.is_series() {
            self.play_series(item, window, cx);
        } else {
            self.play(item, item.resume_secs() > 0, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_initial_of_a_name() {
        assert_eq!(initial("daniel"), "D");
        assert_eq!(initial("éva"), "É");
        assert_eq!(initial(""), "");
    }

    #[test]
    fn the_label_of_an_id() {
        assert_eq!(last_part("player.close"), "close");
        assert_eq!(last_part("play"), "play");
    }
}
