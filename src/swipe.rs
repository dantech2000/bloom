// SPDX-License-Identifier: AGPL-3.0-or-later
//! Back and forward from a two-finger swipe on the trackpad, as in Safari
//! and Finder. AppKit gives the gesture to the app as scroll events (when
//! "Swipe between pages" is set to "Scroll left or right with two fingers"
//! or "with two or three fingers"), so the app itself decides when a run
//! of them is a swipe between pages. A three-finger swipe and the back and
//! forward buttons of a mouse come as `MouseButton::Navigate` and need no
//! decision.
//!
//! The decision is in [`Swipe`], a plain value: one gesture runs from the
//! event of phase `Started` to the one of phase `Ended`; its sideways
//! travel decides, and a row that scrolls sideways under the pointer
//! claims the gesture so that a scroll of the row never turns a page.

use gpui_kit::{Global, TouchPhase};

/// How far two fingers travel sideways for a swipe between pages, in
/// pixels of precise scroll delta.
pub const SWIPE_PX: f32 = 160.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Back,
    Forward,
}

/// The gesture under way. Lives as a global, since the element of a row and
/// the root of the page see the same events, the row first.
#[derive(Default)]
pub struct Swipe {
    /// Between the `Started` and the `Ended` event of one gesture.
    tracking: bool,
    /// A row took this gesture.
    claimed: bool,
    /// A row saw the current event before the root did (the root clears
    /// it with each event, and takes it over at the start of a gesture).
    claim_pending: bool,
    /// Sideways and up-or-down travel since the start, in pixels.
    dx: f32,
    dy: f32,
}

impl Global for Swipe {}

impl Swipe {
    /// A row that scrolls sideways saw a scroll event under the pointer.
    pub fn claim(&mut self) {
        if self.tracking {
            self.claimed = true;
        } else {
            self.claim_pending = true;
        }
    }

    /// Feeds one scroll event of the root with its precise delta. `allowed`
    /// is false when the setting of the Mac does not turn pages with a
    /// scroll, or when the player is open. The page to turn comes with the
    /// end of a gesture that travelled sideways far enough and not much up
    /// or down, and that no row took.
    pub fn feed(&mut self, phase: TouchPhase, dx: f32, dy: f32, allowed: bool) -> Option<Direction> {
        let pending = std::mem::take(&mut self.claim_pending);
        match phase {
            TouchPhase::Started => {
                self.tracking = allowed;
                self.claimed = pending;
                self.dx = 0.;
                self.dy = 0.;
                None
            }
            TouchPhase::Moved => {
                if self.tracking {
                    self.dx += dx;
                    self.dy += dy;
                }
                None
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let decided = self.tracking
                    && !self.claimed
                    && phase == TouchPhase::Ended
                    && self.dx.abs() >= SWIPE_PX
                    && self.dy.abs() < self.dx.abs() / 2.;
                let direction = decided.then(|| if self.dx > 0. { Direction::Back } else { Direction::Forward });
                *self = Self::default();
                direction
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gesture of `steps` events, each moving by (dx, dy).
    fn gesture(swipe: &mut Swipe, dx: f32, dy: f32, steps: usize, allowed: bool) -> Option<Direction> {
        assert_eq!(swipe.feed(TouchPhase::Started, 0., 0., allowed), None);
        for _ in 0..steps {
            assert_eq!(swipe.feed(TouchPhase::Moved, dx, dy, allowed), None);
        }
        swipe.feed(TouchPhase::Ended, 0., 0., allowed)
    }

    #[test]
    fn fingers_moving_right_go_back_and_left_go_forward() {
        let mut swipe = Swipe::default();
        assert_eq!(gesture(&mut swipe, 40., 2., 5, true), Some(Direction::Back));
        assert_eq!(gesture(&mut swipe, -40., -2., 5, true), Some(Direction::Forward));
    }

    #[test]
    fn a_scroll_up_or_down_or_a_short_or_slanted_one_turns_no_page() {
        let mut swipe = Swipe::default();
        assert_eq!(gesture(&mut swipe, 0., 60., 5, true), None, "vertical");
        assert_eq!(gesture(&mut swipe, 30., 0., 5, true), None, "150 px is short of the 160");
        assert_eq!(gesture(&mut swipe, 40., 25., 5, true), None, "slanted: dy is more than half of dx");
        assert_eq!(gesture(&mut swipe, 40., 0., 5, false), None, "the setting is off or the player is open");
    }

    #[test]
    fn a_row_that_scrolls_sideways_keeps_the_gesture() {
        let mut swipe = Swipe::default();
        // The row sees each event before the root does.
        swipe.claim();
        swipe.feed(TouchPhase::Started, 0., 0., true);
        for _ in 0..5 {
            swipe.claim();
            swipe.feed(TouchPhase::Moved, 40., 0., true);
        }
        swipe.claim();
        assert_eq!(swipe.feed(TouchPhase::Ended, 0., 0., true), None);
        // The next gesture, off the row, is its own.
        assert_eq!(gesture(&mut swipe, 40., 0., 5, true), Some(Direction::Back));
    }

    #[test]
    fn a_row_entered_during_the_gesture_keeps_it_too() {
        let mut swipe = Swipe::default();
        swipe.feed(TouchPhase::Started, 0., 0., true);
        swipe.feed(TouchPhase::Moved, 100., 0., true);
        swipe.claim();
        swipe.feed(TouchPhase::Moved, 100., 0., true);
        assert_eq!(swipe.feed(TouchPhase::Ended, 0., 0., true), None);
    }

    #[test]
    fn momentum_events_over_a_row_do_not_claim_the_next_gesture() {
        let mut swipe = Swipe::default();
        assert_eq!(gesture(&mut swipe, 40., 0., 5, true), Some(Direction::Back));
        // After the fingers lift, AppKit sends momentum events with no
        // phase; the row under the pointer sees them, then the root.
        for _ in 0..3 {
            swipe.claim();
            assert_eq!(swipe.feed(TouchPhase::Moved, 5., 0., true), None);
        }
        assert_eq!(gesture(&mut swipe, 40., 0., 5, true), Some(Direction::Back));
    }

    #[test]
    fn a_cancelled_gesture_turns_no_page() {
        let mut swipe = Swipe::default();
        swipe.feed(TouchPhase::Started, 0., 0., true);
        swipe.feed(TouchPhase::Moved, 300., 0., true);
        assert_eq!(swipe.feed(TouchPhase::Cancelled, 0., 0., true), None);
    }
}
