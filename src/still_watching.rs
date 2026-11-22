// SPDX-License-Identifier: AGPL-3.0-or-later
//! "Are you still watching?" in the player: the rule is core's
//! (`bloom_core::still_watching`, Jellyfin Web 12.0's). At the end of an
//! episode, before the next one starts by itself, the rule may ask: the
//! next episode then loads paused, and a dialog over the player offers
//! Continue and Stop. Settings > Playback chooses the rule or turns it off.

use std::time::Instant;

use gpui_kit::{AnyElement, ClickEvent, Context, ParentElement as _, Styled as _, div, px};

use crate::{
    app::{Bloom, Panel},
    jellyfin::{Item, ItemKind},
    ui::button::{ButtonKind, button},
};

impl Bloom {
    /// The user is at the player: a key, a click, the pointer moving.
    pub fn still_watching_input(&mut self) {
        self.playback.still.input(Instant::now());
    }

    /// An item starts in the player. The first item of a play the user
    /// started begins a session; an item that follows counts toward it.
    pub fn still_watching_started(&mut self, item: &Item, first: bool) {
        let episode = item.kind == ItemKind::Episode;
        if first {
            self.playback.still.begin(&item.id, episode, Instant::now());
        } else {
            self.playback.still.note(&item.id, episode);
        }
    }

    /// The end of a file, before the next item of the queue starts by
    /// itself: true when the rule asks first. The next item then starts
    /// paused (`stream_begin`). Another device that plays (AirPlay, a
    /// group) has its own player and asks nothing here.
    pub fn still_watching_asks(&mut self) -> bool {
        if self.airplay.active() || self.sync.holds_player() {
            return false;
        }
        let Some(next) = self.queue.upcoming.first() else { return false };
        let (id, episode) = (next.id.clone(), next.kind == ItemKind::Episode);
        let rule = self.prefs.still_watching().rule();
        let ask = self.playback.still.ask(&id, episode, rule, Instant::now());
        if ask {
            log::info!("still watching: asking before {}", next.display_title());
            self.stream.start_paused = true;
        }
        ask
    }

    /// The next item started paused: the question shows over it.
    pub fn still_watching_show(&mut self, cx: &mut Context<Self>) {
        self.show_panel_alone(Panel::StillWatching, cx);
    }

    /// Continue: the item plays on, and a new session starts with it.
    pub fn still_watching_continue(&mut self, cx: &mut Context<Self>) {
        if !self.close_panel(Panel::StillWatching) {
            return;
        }
        if let Some(item) = &self.playback.playing {
            self.playback.still.keep_watching(&item.id, Instant::now());
        }
        self.player.set_paused(false);
        self.show_controls();
        cx.notify();
    }

    /// Stop: the player closes.
    pub fn still_watching_stop(&mut self, cx: &mut Context<Self>) {
        if self.close_panel(Panel::StillWatching) {
            self.request_stop(cx);
        }
    }

    /// The keys of the player while the question shows: Return is
    /// Continue, Escape is Stop, and no other key reaches the player (a
    /// space would start the video under the question). True when the
    /// question took the key.
    pub fn still_watching_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if !self.panel_is(Panel::StillWatching) {
            return false;
        }
        match key {
            "enter" => self.still_watching_continue(cx),
            "escape" => self.still_watching_stop(cx),
            _ => {}
        }
        true
    }

    /// The dialog over the paused player. A click beside it does nothing:
    /// the question waits for its answer.
    pub fn render_still_watching(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.panel_is(Panel::StillWatching) {
            return None;
        }
        let message = match &self.playback.playing {
            Some(item) => format!("{} is paused until you answer.", item.display_title()),
            None => "The next episode is paused until you answer.".to_string(),
        };
        let card = crate::ui::dialog::titled("still.card", "Are you still watching?", &message, cx).child(
            div()
                .mt(px(8.))
                .flex()
                .justify_end()
                .gap(px(10.))
                .child(
                    button("still.stop", "Stop", ButtonKind::Plain, cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.still_watching_stop(cx))),
                )
                .child(
                    button("still.continue", "Continue", ButtonKind::Primary, cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.still_watching_continue(cx))),
                ),
        );
        let layer = crate::ui::dialog::layer("still.dialog", card, cx);
        Some(crate::ui::dialog::shown(layer, "still.dialog", cx))
    }

    /// For the debug channel: `still-watching [continue|stop|ask]`. `ask`
    /// shows the question over the item that plays, as the end of an
    /// episode would.
    pub fn still_watching_debug(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        match rest {
            "" | "state" => {}
            "continue" => self.still_watching_continue(cx),
            "stop" => self.still_watching_stop(cx),
            "ask" if self.playback.playing.is_some() => {
                self.player.set_paused(true);
                self.still_watching_show(cx);
            }
            _ => return "error: usage: still-watching [state|continue|stop|ask] (ask needs an item playing)".into(),
        }
        format!(
            "still-watching prompt={} choice={:?} paused={} playing={:?}",
            self.panel_is(Panel::StillWatching),
            self.prefs.still_watching(),
            self.playback.status.paused,
            self.playback.playing.as_ref().map(|i| i.display_title()),
        )
    }
}
