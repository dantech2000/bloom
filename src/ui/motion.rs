// SPDX-License-Identifier: AGPL-3.0-or-later
//! How things come in. One system for the whole app: a page pushed onto
//! the one before slides in from the right, a sheet rises from the bottom,
//! a popup or a dialog settles into place from a little above, and content
//! that changed in place fades in. Nothing moves when the theme or the
//! system asks for less motion: GPUI ends an animation at once under the
//! system's setting, and the theme's flag skips it here.

use std::time::Duration;

use gpui_kit::{Animation, AnimationExt as _, AnyElement, App, ElementId, IntoElement, Styled, ease_out_quint, px};

use super::theme::UiTheme;

/// The way an element comes in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Enter {
    /// In place: the content of a section that changed.
    Fade,
    /// A page pushed onto the one before: from the right, as iOS pushes.
    Push,
    /// A sheet from the bottom of the window.
    Rise,
    /// A popup or a dialog: settles down into place.
    Settle,
    /// A page that replaces another in the same frame (the Mac's pages,
    /// a new page of a grid): lifts into place from a little below.
    Lift,
}

impl Enter {
    fn duration(self, cx: &App) -> Duration {
        let motion = UiTheme::read(cx).motion;
        match self {
            Self::Fade | Self::Settle => motion.fast,
            Self::Push | Self::Rise | Self::Lift => motion.normal,
        }
    }
}

/// `el` with its entrance. The animation runs from the frame the element
/// first appears under `id`; an element that leaves the tree and comes
/// back runs it again, and a key in `id` restarts it in place.
pub fn enter<E>(el: E, id: impl Into<ElementId>, how: Enter, cx: &App) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    if UiTheme::read(cx).motion.reduced || cx.reduce_motion() {
        return el.into_any_element();
    }
    let ease = ease_out_quint();
    el.with_animation(id, Animation::new(how.duration(cx)).with_easing(ease), move |el, progress| {
        let el = el.opacity(progress);
        let left = 1. - progress;
        match how {
            Enter::Fade => el,
            Enter::Push => el.relative().left(px(32. * left)),
            Enter::Rise => el.relative().top(px(28. * left)),
            Enter::Settle => el.relative().top(px(-6. * left)),
            Enter::Lift => el.relative().top(px(12. * left)),
        }
    })
    .into_any_element()
}
