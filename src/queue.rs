// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the player has beyond the one item it plays: the items that follow,
//! the chapters and preview images of its timeline, and the "Up next" card.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui_kit::{Bounds, Context, Pixels};

use crate::{
    app::Bloom,
    jellyfin::{Chapter, Item, TimelineInfo},
    player::PlayState,
};

/// The "Up next" card comes up this long before the end of an item.
const UP_NEXT_SECS: f64 = 30.;
/// "Previous" within this time from the start goes to the item before;
/// later it starts the item again.
const RESTART_AFTER_SECS: f64 = 5.;

#[derive(Default)]
pub struct PlayQueue {
    /// Items that play after the one in the player, in order.
    pub upcoming: Vec<Item>,
    /// Items that played before it, oldest first.
    pub history: Vec<Item>,
    /// Chapters and preview images of the item in the player.
    pub timeline: TimelineInfo,
    /// Place of the pointer over the timeline, from 0 to 1.
    pub hover: Option<f32>,
    /// Where the timeline was painted in the last frame.
    pub timeline_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The user sent the "Up next" card away for this item.
    pub up_next_hidden: bool,
    /// A series of the queue is being changed into its episodes.
    pub expanding: bool,
    /// The next item to start is the first of a queue the user started, so
    /// it goes on from its resume point.
    pub resume_first: bool,
    /// Counts the queues, so an answer for an old queue is dropped.
    pub epoch: u64,
    /// The user chose the audio track of this item; the choice of the
    /// server does not replace it.
    pub explicit_audio: bool,
    /// The same for the subtitle.
    pub explicit_subtitle: bool,
    /// The preview frame painted last. It stays up while the sheet of the
    /// next one is not decoded yet.
    pub preview_last: Rc<RefCell<Option<std::sync::Arc<gpui_kit::RenderImage>>>>,
}

/// One frame of the preview images: the sheet it is on and its cell there.
#[derive(Clone)]
pub struct PreviewFrame {
    pub url: String,
    pub column: u32,
    pub row: u32,
    pub columns: u32,
    pub rows: u32,
    /// Height of a frame divided by its width.
    pub aspect: f32,
}

impl Bloom {
    /// Forgets the queue; a new item chosen by the user starts a new one.
    pub fn clear_queue(&mut self) {
        self.queue.upcoming.clear();
        self.queue.history.clear();
        self.queue.expanding = false;
        self.queue.resume_first = false;
        self.queue.epoch += 1;
    }

    /// Starts a queue the user asked for: the first item plays, from its
    /// resume point, and the others follow.
    pub fn start_queue(
        &mut self,
        items: Vec<Item>,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if self.cast_play(&items, true, cx) || self.sync_play(&items, true, cx) {
            return;
        }
        self.clear_queue();
        self.queue.upcoming = items;
        self.queue.resume_first = true;
        if self.play_next(cx) {
            window.focus(&self.player_focus, cx);
            self.start_player_poll(cx);
        }
    }

    /// A series in the queue stands for its episodes: from the next
    /// unwatched one on, or all of them when there is none. They take its
    /// place, and the first of them starts.
    fn expand_series(&mut self, series: Item, cx: &mut Context<Self>) {
        self.queue.expanding = true;
        let epoch = self.queue.epoch;
        self.fetch(
            cx,
            move |client| client.series_queue(&series.id),
            move |this, result, cx| {
                if this.queue.epoch != epoch {
                    return;
                }
                this.queue.expanding = false;
                match result {
                    Ok(episodes) => {
                        this.queue.upcoming.splice(0..0, episodes);
                    }
                    Err(err) => log::warn!("queue: no episodes for a series: {err:#}"),
                }
                // With nothing left, the player closes at its next poll.
                this.play_next(cx);
                cx.notify();
            },
        );
    }

    /// Loads the chapters and preview images of the item in the player.
    pub fn load_timeline(&mut self, item_id: String, cx: &mut Context<Self>) {
        self.queue.timeline = TimelineInfo::default();
        self.queue.hover = None;
        self.queue.up_next_hidden = false;
        self.fetch(
            cx,
            {
                let item_id = item_id.clone();
                // The timeline works without them.
                move |client| Ok(client.timeline_info(&item_id).unwrap_or_default())
            },
            move |this, result, cx| {
                if let Ok(timeline) = result
                    && this.playing.as_ref().is_some_and(|i| i.id == item_id)
                {
                    // The audio track that fits the settings of the user,
                    // unless the user chose one on the detail page. In a
                    // transcode the server chose it (see `stream`).
                    if let Some(track) = timeline.default_audio
                        && !this.queue.explicit_audio
                        && !crate::stream::transcoding()
                    {
                        this.player.set_audio(Some(track));
                    }
                    // The subtitle the server chose from the subtitle mode
                    // and language of the user ("no" for none).
                    if let Some(choice) = timeline.default_subtitle
                        && !this.queue.explicit_subtitle
                        && !crate::stream::transcoding()
                    {
                        this.player.set_subtitle(choice);
                    }
                    this.queue.timeline = timeline;
                    this.preload_previews(cx);
                    cx.notify();
                }
            },
        );
    }

    /// Fetches the preview sheets of the timeline ahead of a drag, the sheet
    /// of the resume point first and then the ones around it.
    fn preload_previews(&mut self, cx: &mut Context<Self>) {
        let (Some(trickplay), Some(item), Some(session)) = (
            self.queue.timeline.trickplay.as_ref(),
            self.playing.as_ref(),
            self.session.as_ref(),
        ) else {
            return;
        };
        let per_sheet = (trickplay.tile_width * trickplay.tile_height).max(1);
        let sheets = trickplay.thumbnail_count.div_ceil(per_sheet);
        let frame = (item.resume_secs().max(0) as u64 * 1000 / trickplay.interval.max(1) as u64) as u32;
        let start = (frame / per_sheet).min(sheets.saturating_sub(1));
        let mut order: Vec<u32> = (0..sheets).collect();
        order.sort_by_key(|sheet| sheet.abs_diff(start));
        let urls = order
            .into_iter()
            .map(|sheet| session.client.trickplay_url(&item.id, trickplay.width, sheet))
            .collect();
        crate::images::preload(urls, cx);
    }

    /// After an episode, the episodes that follow it play. This fills the
    /// queue with them, unless the caller gave the queue its own items.
    pub fn queue_followers(&mut self, item: &Item, cx: &mut Context<Self>) {
        if item.kind != "Episode" {
            return;
        }
        let Some(series_id) = item.series_id.clone() else {
            return;
        };
        let item_id = item.id.clone();
        self.fetch(
            cx,
            {
                let item_id = item_id.clone();
                move |client| client.episodes_after(&series_id, &item_id)
            },
            move |this, result, cx| {
                if let Ok(episodes) = result
                    && this.queue.upcoming.is_empty()
                    && this.playing.as_ref().is_some_and(|i| i.id == item_id)
                {
                    this.queue.upcoming = episodes;
                    cx.notify();
                }
            },
        );
    }

    /// Plays the next item of the queue. False when there is none.
    pub fn play_next(&mut self, cx: &mut Context<Self>) -> bool {
        if self.queue.expanding {
            return true;
        }
        if self.queue.upcoming.is_empty() {
            return false;
        }
        let next = self.queue.upcoming.remove(0);
        if next.is_series() {
            self.expand_series(next, cx);
            return true;
        }
        // Only the first item of a queue goes on from its resume point; an
        // item that follows another starts at its beginning, as on the web.
        let first = std::mem::take(&mut self.queue.resume_first);
        if let Some(current) = self.playing.take()
            && !first
        {
            self.queue.history.push(current);
        }
        self.begin(&next, first && next.resume_secs() > 0, cx);
        true
    }

    /// Goes to the item before; later than a few seconds into an item, or
    /// with no item before, it starts the item again.
    pub fn play_previous(&mut self, cx: &mut Context<Self>) {
        if self.player_status.position > RESTART_AFTER_SECS || self.queue.history.is_empty() {
            self.player.seek_absolute(0.);
            return;
        }
        let Some(previous) = self.queue.history.pop() else {
            return;
        };
        if let Some(current) = self.playing.take() {
            self.queue.upcoming.insert(0, current);
        }
        self.begin(&previous, false, cx);
    }

    /// The player reached the end of a file: the next item of the queue
    /// starts. False when playback is over and the player must close.
    pub fn advance_at_end(&mut self, cx: &mut Context<Self>) -> bool {
        // After an episode, the user's setting decides if the next starts.
        let stay = self.playing.as_ref().is_some_and(|item| item.kind == "Episode")
            && !self.prefs.auto_play_next();
        self.player_status.reached_end
            && self.player_status.error.is_none()
            && !stay
            && self.play_next(cx)
    }

    /// The next item and the seconds until it starts, in the time the
    /// "Up next" card shows: the last half minute, or the credits.
    pub fn up_next(&self) -> Option<(&Item, i64)> {
        let s = &self.player_status;
        if self.queue.up_next_hidden || s.state != PlayState::Playing || s.duration <= 0. {
            return None;
        }
        // The card is a setting of the user, and it has nothing to say when
        // the next episode does not start by itself.
        let episode = self.playing.as_ref().is_some_and(|item| item.kind == "Episode");
        if !self.prefs.up_next_card() || (episode && !self.prefs.auto_play_next()) {
            return None;
        }
        let next = self.queue.upcoming.first()?;
        let remaining = (s.duration - s.position).max(0.);
        let in_credits = self
            .segments
            .iter()
            .any(|seg| seg.kind == "Outro" && s.position >= seg.start_secs());
        (remaining <= UP_NEXT_SECS || in_credits)
            .then(|| (next, (remaining / self.speed.max(0.25) as f64).ceil() as i64))
    }

    /// The time under the pointer on the timeline; during a drag, the time
    /// of the thumb.
    pub fn timeline_hover_secs(&self, cx: &gpui_kit::App) -> Option<f64> {
        let duration = self.player_status.duration;
        if duration <= 0. {
            return None;
        }
        if self.scrubbing {
            return Some(self.seek_slider.read(cx).value().end() as f64);
        }
        self.queue.hover.map(|f| f as f64 * duration)
    }

    /// The chapter a time is in.
    pub fn chapter_at(&self, secs: f64) -> Option<&Chapter> {
        self.queue
            .timeline
            .chapters
            .iter()
            .rev()
            .find(|c| c.start_secs() <= secs)
    }

    /// The preview image of a time.
    pub fn preview_frame(&self, secs: f64) -> Option<PreviewFrame> {
        let trickplay = self.queue.timeline.trickplay.as_ref()?;
        let item = self.playing.as_ref()?;
        let client = &self.session.as_ref()?.client;
        let mut index = (secs.max(0.) * 1000. / trickplay.interval as f64) as u32;
        if trickplay.thumbnail_count > 0 {
            index = index.min(trickplay.thumbnail_count - 1);
        }
        let per_sheet = trickplay.tile_width * trickplay.tile_height;
        let cell = index % per_sheet;
        Some(PreviewFrame {
            url: client.trickplay_url(&item.id, trickplay.width, index / per_sheet),
            column: cell % trickplay.tile_width,
            row: cell / trickplay.tile_width,
            columns: trickplay.tile_width,
            rows: trickplay.tile_height,
            aspect: trickplay.height as f32 / trickplay.width as f32,
        })
    }
}
