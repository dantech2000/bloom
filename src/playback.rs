// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the user does to playback, in one place. Every button, key and menu
//! entry that pauses, seeks, changes the item or stops goes through here.
//! Alone, the player does it at once. In a SyncPlay group it is a request to
//! the group, and the player follows the command that comes back.

use gpui_kit::Context;

use crate::{app::Bloom, syncplay::core::Intent};

impl Bloom {
    /// Gives an action of the user to the group. False when this player is
    /// alone, and the caller does the action itself.
    fn sync_intent(&mut self, intent: Intent, cx: &mut Context<Self>) -> bool {
        if !self.sync.following() {
            return false;
        }
        self.sync_user(intent, cx);
        self.show_controls();
        true
    }

    pub fn request_toggle_pause(&mut self, cx: &mut Context<Self>) {
        if !self.sync_intent(Intent::TogglePause, cx) {
            self.player.toggle_pause();
        }
    }

    /// Seeks to a position in seconds.
    pub fn request_seek_to(&mut self, secs: f64, cx: &mut Context<Self>) {
        if !self.sync_intent(Intent::Seek(secs * 1000.), cx) {
            self.player.seek_absolute(secs);
        }
    }

    /// Seeks by a number of seconds from the position now.
    pub fn request_seek_by(&mut self, secs: f64, cx: &mut Context<Self>) {
        // A group takes a position, not a step.
        let target = (self.player_status.position + secs).max(0.);
        if !self.sync_intent(Intent::Seek(target * 1000.), cx) {
            self.player.seek_relative(secs);
        }
    }

    /// Goes to the next item of the queue. False when there is none.
    pub fn request_next(&mut self, cx: &mut Context<Self>) -> bool {
        self.sync_intent(Intent::Next, cx) || self.play_next(cx)
    }

    pub fn request_previous(&mut self, cx: &mut Context<Self>) {
        if !self.sync_intent(Intent::Previous, cx) {
            self.play_previous(cx);
        }
    }

    /// Stops playback and closes the player.
    pub fn request_stop(&mut self, cx: &mut Context<Self>) {
        // The user leaves the player, not the group: the group goes on and
        // does not wait for this player.
        if self.sync.holds_player() {
            let ui = self.sync.session.as_mut().map(|s| s.player_closed());
            self.sync_apply(ui.unwrap_or_default(), cx);
        }
        self.player.stop();
        self.close_player_view(cx);
    }
}
