// SPDX-License-Identifier: AGPL-3.0-or-later
//! The colours of Bloom that are no slot of the component theme: the one
//! ink and its quieter levels, the edge and the fills of the glass
//! surfaces, and the three colours of a state. A screen names these and
//! writes no hex value of its own for them, so the look is in one place.
//! See "One ink; colour only for state" in CLAUDE.md.

use gpui_kit::Rgba;

/// A colour from `0xrrggbbaa`, as `gpui::rgba` makes it, for a constant.
pub const fn hex(value: u32) -> Rgba {
    Rgba {
        r: ((value >> 24) & 0xff) as f32 / 255.,
        g: ((value >> 16) & 0xff) as f32 / 255.,
        b: ((value >> 8) & 0xff) as f32 / 255.,
        a: (value & 0xff) as f32 / 255.,
    }
}

/// The one ink of text and icons.
pub const INK: Rgba = hex(0xf5f5f7ff);
/// The ink at lower opacity, for quieter text: 80, 70, 60, 50, 40 and 30
/// percent.
pub const INK_80: Rgba = hex(0xf5f5f7cc);
pub const INK_70: Rgba = hex(0xf5f5f7b3);
pub const INK_60: Rgba = hex(0xf5f5f799);
pub const INK_50: Rgba = hex(0xf5f5f780);
pub const INK_40: Rgba = hex(0xf5f5f766);
pub const INK_30: Rgba = hex(0xf5f5f74d);
/// The ink as a line or a fill that is barely there: a separator, the
/// row under the pointer in a list.
pub const FAINT: Rgba = hex(0xf5f5f71f);
/// The dark text and icons on an accent fill.
pub const ON_ACCENT: Rgba = hex(0x121212ff);

/// The light edge of a popup or a card.
pub const EDGE: Rgba = hex(0xf5f5f733);
/// The fill of a control under the pointer, and of a quiet pill.
pub const HOVER: Rgba = hex(0xffffff1f);
/// The fill of a card of a page where the glass is off.
pub const CARD: Rgba = hex(0x2a2a2ab0);
/// The dark fill of a raised button at rest.
pub const RAISED: Rgba = hex(0x282828cc);
/// The fill of a button that deletes or stops something.
pub const DANGER: Rgba = hex(0xc62828ff);
/// The dark layer over the page behind a dialog.
pub const SCRIM: Rgba = hex(0x00000099);
/// The quiet round disc icons and letters sit on, as in the navigation.
pub const DISC: Rgba = hex(0xcccccf40);

/// What a colour may say: a state, never a kind of thing. Green for what
/// works or is live, amber for what needs a look, red for what failed or
/// is dangerous. No state has no colour: the ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ok,
    Warn,
    Bad,
}

impl State {
    pub const fn color(self) -> Rgba {
        match self {
            State::Ok => hex(0x7ee787ff),
            State::Warn => hex(0xe3b341ff),
            State::Bad => hex(0xff7b72ff),
        }
    }
}

/// The colour of an icon or a text that may stand for a state: the
/// state's colour, or the ink.
pub fn state_ink(state: Option<State>) -> Rgba {
    state.map_or(INK, State::color)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constant is the colour `rgba` gives for the same number, to the
    /// bit: a screen that changes from one to the other looks the same.
    #[test]
    fn a_constant_is_the_colour_of_the_same_number() {
        for value in [0xf5f5f733, 0xffffff1f, 0x00000099, 0x2a2a2ab0, 0x7ee787ff] {
            assert_eq!(hex(value), gpui_kit::rgba(value));
        }
        assert_eq!(INK, gpui_kit::rgb(0xf5f5f7));
        assert_eq!(ON_ACCENT, gpui_kit::rgb(0x121212));
    }
}
