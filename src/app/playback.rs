//! The user data of an item (played, favourite) and playback: starting an
//! item, the controls, the volume, picture-in-picture, the frame sync.

use super::*;

impl Bloom {
    // ----- user data --------------------------------------------------------

    /// Flips the watched state of the item shown on the detail page.
    pub fn toggle_played(&mut self, cx: &mut Context<Self>) {
        let Page::Detail(data) = &self.nav.page else {
            return;
        };
        let (item_id, played) = (data.item.id.clone(), !data.item.user_data.played);
        self.set_user_data(&item_id, Mark::Played, played, true, cx, |this, user_data, cx| {
            if let Page::Detail(data) = &mut this.nav.page {
                data.item.user_data = user_data;
                if data.item.is_series() {
                    // Episodes and season counts changed server-side too.
                    this.load_page(cx);
                }
            }
        });
    }

    /// Flips the favorite state of the item shown on the detail page.
    pub fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        let Page::Detail(data) = &self.nav.page else {
            return;
        };
        let (item_id, favorite) = (data.item.id.clone(), !data.item.user_data.is_favorite);
        self.set_user_data(&item_id, Mark::Favorite, favorite, true, cx, |this, user_data, _| {
            if let Page::Detail(data) = &mut this.nav.page {
                data.item.user_data = user_data;
            }
        });
    }

    // ----- playback ---------------------------------------------------------

    /// Plays the next unwatched episode of a series, or the first episode
    /// when everything has been watched.
    pub fn play_series(&mut self, series: &Item, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return;
        };
        let series_id = series.id.clone();
        let generation = self.nav.generation;
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Option<Item>> = cx
                .background_executor()
                .spawn(async move {
                    if let Some(next) = client.series_next_up(&series_id)? {
                        return Ok(Some(next));
                    }
                    let seasons = client.seasons(&series_id)?;
                    let Some(first) = seasons
                        .iter()
                        .find(|s| s.index_number != Some(0))
                        .or_else(|| seasons.first())
                    else {
                        return Ok(None);
                    };
                    let episodes = client.episodes(&series_id, &first.id)?;
                    Ok(episodes.into_iter().next())
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.nav.generation != generation {
                    return;
                }
                match result {
                    Ok(Some(episode)) => {
                        let resume = episode.resume_secs() > 0;
                        this.play(&episode, resume, window, cx);
                    }
                    Ok(None) => this.toast("Nothing to play", "This series has no episodes.", cx),
                    Err(err) => this.toast("Could not start playback", format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn play(&mut self, item: &Item, resume: bool, window: &mut Window, cx: &mut Context<Self>) {
        // With a target set, the item plays there (see `src/cast`).
        if self.cast_play(std::slice::from_ref(item), resume, cx)
            || self.sync_play(std::slice::from_ref(item), resume, cx)
        {
            return;
        }
        self.clear_queue();
        self.begin(item, resume, cx);
        window.focus(&self.playback.focus, cx);
        self.start_player_poll(cx);
        self.queue_followers(item, cx);
    }

    /// Starts an item in the player. The queue stays as it is, so this is
    /// also the step from one item of the queue to the next.
    pub fn begin(&mut self, item: &Item, resume: bool, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        let start_secs = if resume { item.resume_secs() } else { 0 };
        // While an item is on an AirPlay receiver, the next one goes there
        // too; the local player stays quiet.
        if self.airplay.active() {
            self.airplay_send(item, start_secs as f64, cx);
            return;
        }
        // A downloaded item plays from its file on this Mac; otherwise the
        // server says how the item plays (the file, or a transcode).
        self.stream_begin(item, start_secs as f64);
        self.player.set_volume(self.playback.volume as f64);
        self.player.set_muted(self.playback.muted);
        self.player.set_speed(self.playback.speed as f64);
        self.apply_subtitle_style();
        self.playback.playing = Some(item.clone());
        self.close_panel(Panel::Episodes);
        self.playback.segments.clear();
        let item_id = item.id.clone();
        self.fetch(
            cx,
            {
                let item_id = item_id.clone();
                // The ranges are optional; playback does not need them. A
                // downloaded item has them in its `meta.json`, so a play
                // with no server still skips the intro.
                move |client| {
                    Ok(crate::downloads::stored_segments(client.server_id.as_deref(), &item_id)
                        .unwrap_or_else(|| client.media_segments(&item_id).unwrap_or_default()))
                }
            },
            move |this, result, cx| {
                if let Ok(segments) = result
                    && this.playback.playing.as_ref().is_some_and(|i| i.id == item_id)
                {
                    this.playback.segments = segments;
                    cx.notify();
                }
            },
        );
        self.load_timeline(item.id.clone(), cx);
        self.playback.status = self.player.status();
        self.playback.open = true;
        // A phone turns for the video; an iPad keeps the orientation it has.
        if self.phone() {
            crate::set_landscape(true);
        }
        self.show_controls();
        self.playback.tracks_version = 0;
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// When the last local input happened.
    pub fn last_activity(&self) -> std::time::Instant {
        self.playback.last_activity
    }

    /// Called on pointer activity over the video; controls hide again after a delay.
    pub fn show_controls(&mut self) {
        self.playback.last_activity = std::time::Instant::now();
        self.playback.controls_visible = true;
    }

    /// Controls stay while paused, buffering, scrubbing, or when a track menu is open.
    pub(super) fn controls_pinned(&self, cx: &App) -> bool {
        self.playback.status.paused
            || self.playback.status.state != PlayState::Playing
            || self.playback.scrubbing
            || self.playback.audio_menu.read(cx).is_open()
            || self.playback.subtitle_menu.read(cx).is_open()
            || self.playback.settings_menu.read(cx).is_open()
            || self.panel_open()
    }

    /// Switches the player between the full window and picture in picture.
    pub fn toggle_pip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.playback.pip.take() {
            Some(normal) => crate::pip::leave(window, normal),
            None => {
                if window.is_fullscreen() {
                    window.toggle_fullscreen();
                }
                self.playback.pip = crate::pip::enter(window, self.config.pip_window);
            }
        }
        self.show_controls();
        cx.notify();
    }

    /// Ends picture in picture when the player goes away.
    pub fn leave_pip(&mut self, window: &mut Window) {
        if let Some(normal) = self.playback.pip.take() {
            crate::pip::leave(window, normal);
        }
    }

    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.playback.volume = volume.clamp(0., 100.);
        self.player.set_volume(self.playback.volume as f64);
        if self.playback.muted && self.playback.volume > 0. {
            self.playback.muted = false;
            self.player.set_muted(false);
        }
        cx.notify();
    }

    /// Changes the volume by a step and moves the slider with it.
    pub fn nudge_volume(&mut self, step: f32, window: &mut Window, cx: &mut Context<Self>) {
        let volume = (self.playback.volume + step).clamp(0., 100.);
        self.playback.volume_slider
            .update(cx, |slider, cx| slider.set_value(volume, window, cx));
        self.set_volume(volume, cx);
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.playback.muted = !self.playback.muted;
        self.player.set_muted(self.playback.muted);
        cx.notify();
    }

    pub fn set_speed(&mut self, speed: f32, cx: &mut Context<Self>) {
        // A group plays at one speed.
        if self.sync.following() {
            self.toast("SyncPlay", "The speed stays at 1x while you play with a group.", cx);
            return;
        }
        self.playback.speed = speed;
        self.player.set_speed(speed as f64);
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// Favourite toggle for the item in the player.
    pub fn toggle_playing_favorite(&mut self, cx: &mut Context<Self>) {
        let Some(item) = &self.playback.playing else {
            return;
        };
        let (item_id, favorite) = (item.id.clone(), !item.user_data.is_favorite);
        self.set_user_data(&item_id.clone(), Mark::Favorite, favorite, false, cx, move |this, user_data, _| {
            if let Some(item) = this.playback.playing.as_mut().filter(|i| i.id == item_id) {
                item.user_data = user_data;
            }
        });
    }

    /// The marked range the playback position is in, if any.
    pub fn current_segment(&self) -> Option<&MediaSegment> {
        let position = self.playback.status.position;
        self.playback.segments.iter().find(|s| {
            // A range under three seconds is not worth a prompt.
            s.end_secs() - s.start_secs() >= 3.
                && position >= s.start_secs()
                && position < s.end_secs() - 1.
        })
    }

    /// Shows or hides the data of the player about the video that plays.
    pub fn toggle_playback_info(&mut self, cx: &mut Context<Self>) {
        self.playback.info = !self.playback.info;
        self.player
            .command("script-binding", &["stats/display-stats-toggle"]);
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// Full screen that the user entered for an item ends when the player
    /// closes: the page it gives way to has no key and no button to leave
    /// full screen, and nobody asked for the library in full screen. A
    /// window that was full screen before the item started stays so, and
    /// the next item of a queue keeps the player, and so the full screen.
    pub(super) fn follow_player_fullscreen(&mut self, window: &mut Window) {
        if self.playback.open == self.playback.was_open {
            return;
        }
        self.playback.was_open = self.playback.open;
        if self.playback.open {
            self.playback.fullscreen_before = window.is_fullscreen();
        } else if leaves_fullscreen_with_the_player(self.playback.fullscreen_before, window.is_fullscreen()) {
            window.toggle_fullscreen();
        }
    }

    pub fn close_player_view(&mut self, cx: &mut Context<Self>) {
        self.playback.open = false;
        if self.phone() {
            crate::set_landscape(false);
        }
        // The end of the playback is an event, at once.
        self.events_tick(cx);
        // The preview sheets of the timeline are not needed any more.
        crate::images::preload(Vec::new(), cx);
        self.news_player_closed(cx);
        cx.notify();
    }

    /// The seek slider and the time label follow the position, by whole
    /// quarter seconds, in the draw of a frame (see the frames task): while
    /// frames flow, the poll asks for no draw of its own for the position.
    pub(super) fn follow_position(&mut self, cx: &mut Context<Self>) {
        let status = &self.playback.status;
        if self.playback.scrubbing || status.duration <= 0. {
            return;
        }
        let pos = (status.position * 4.).floor() / 4.;
        if pos == self.playback.slider_pos {
            return;
        }
        self.playback.slider_pos = pos;
        self.move_seek_slider(cx);
    }

    /// Puts the seek slider at the position of the player.
    pub(super) fn move_seek_slider(&mut self, cx: &mut Context<Self>) {
        let (pos, dur) = (self.playback.status.position as f32, self.playback.status.duration as f32);
        self.playback.seek_slider.update(cx, |s, cx| {
            crate::ui::slider::set_range_and_value(s, dur, pos);
            cx.notify();
        });
    }

    /// Measures how long a new frame waited for the UI, once for a frame.
    pub(super) fn note_frame_wait(&mut self) {
        let seq = self.player.frame_seq();
        if seq != self.playback.noticed_frame_seq {
            self.playback.noticed_frame_seq = seq;
            crate::perf::video_frame_noticed(self.player.frame_wait_ms());
        }
    }

    /// Swaps in the newest video frame. With display pacing (`pacing.rs`)
    /// a frame is shown one tick of the display after its own.
    pub(super) fn sync_frame(&mut self, window: &mut Window) {
        self.player.ui_seen();
        let display = self.player.display_pacing();
        if display && self.playback.open {
            crate::pacing::note_window(window, &self.player);
        }
        let prev_draw = std::mem::replace(&mut self.playback.last_draw_ns, crate::player::clock_ns());
        // A closed player draws no frame: the surface goes back to the pool
        // at once, not when the next item plays.
        if !self.playback.open {
            self.playback.frame = None;
        }
        if self.player.frame_seq() == self.playback.frame_seq {
            return;
        }
        // One read of the frame, its number and its tick: a frame that is
        // published between two reads cannot be shown with the tick of the
        // one before.
        let published = self.player.frame();
        let seq = published.seq;
        if seq == self.playback.frame_seq {
            return;
        }
        let draw_id = gpui_apple::present_trace::next_draw_id();
        if display {
            let (now, period) = self.player.clock_time();
            // The frame of this tick came before this draw: it waits for
            // the draw the frames task asks for, like every frame.
            if !crate::pacing::due(published.tick_ns, now, period) {
                crate::pacing::count_held();
                crate::pacing::trace::held(seq);
                window.request_animation_frame();
                return;
            }
            if crate::pacing::late(published.tick_ns, now, period) {
                crate::pacing::count_late();
            }
            // The phase loop hears where this draw comes out.
            crate::pacing::phase::adopted(seq, draw_id, published.tick_ns, published.vsync_ns, period, now);
        }
        self.playback.frame_seq = seq;
        crate::pacing::trace::adopted(seq, self.playback.frame_wake_ns, self.playback.last_draw_ns, prev_draw, draw_id);
        crate::perf::video_frame_shown();
        self.playback.frame = if self.playback.open { published.frame } else { None };
    }

    pub(crate) fn start_player_poll(&mut self, cx: &mut Context<Self>) {
        self.playback.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let fast = this
                    .read_with(cx, |this, _| this.playback.open)
                    .unwrap_or(false);
                // The frames have their own signal. Everything here moves
                // by quarter seconds or slower, or is a change the eye sees
                // within 50 ms (a pause, the end, a seek's new position; the
                // frame of a seek comes through the frames task).
                let interval = if fast { 50 } else { 500 };
                cx.background_executor()
                    .timer(Duration::from_millis(interval))
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        let poll_started = crate::player::clock_ns();
                        let status = this.player.status();
                        let ended = status.state == PlayState::Ended;
                        // The seek slider and the time label move by whole
                        // seconds; a redraw for each 8 ms change of the
                        // position cost the whole UI about 48 draws a
                        // second. `playback.status` itself stays current for
                        // everything that reads it.
                        let position_moved = (status.position * 4.).floor()
                            != (this.playback.status.position * 4.).floor();
                        // While frames come, each of them draws the UI and
                        // moves the slider (`follow_position`), so a draw
                        // for the position alone is not needed: it lands at
                        // the tick, misses the compositor and pushes the
                        // next frame a refresh late (see pacing.rs).
                        let frames_flow = this.player.display_pacing()
                            && status.state == PlayState::Playing
                            && !status.paused
                            && this.player.frame_wait_ms() < 100.;
                        let backdrop = crate::ui::glass::video_backdrop_version();
                        let backdrop_moved = backdrop != this.backdrop_seen;
                        this.backdrop_seen = backdrop;
                        let changed = (position_moved && !frames_flow)
                            || backdrop_moved
                            || status.paused != this.playback.status.paused
                            || status.buffering != this.playback.status.buffering
                            || status.state != this.playback.status.state
                            || status.tracks_version != this.playback.status.tracks_version;
                        this.playback.status = status;
                        this.subs_status_changed(cx);
                        for index in this.player.take_failed_subtitles() {
                            this.subtitle_file_failed(index);
                        }
                        if this.playback.status.tracks_version != this.playback.tracks_version {
                            this.playback.tracks_version = this.playback.status.tracks_version;
                            this.rebuild_track_menus(cx);
                        }
                        // The slider follows the position; an update with no
                        // change would only cause a redraw.
                        if changed && !this.playback.scrubbing && this.playback.status.duration > 0. {
                            this.move_seek_slider(cx);
                        }
                        this.enhanced_tick(cx);
                        this.adaptive_tick(cx);
                        this.events_tick(cx);
                        // At the end of a file the queue goes on.
                        // In a group the group decides what comes after an item.
                        let ended = ended
                            && !this.sync.holds_player()
                            && !this.advance_at_end(cx)
                            && !this.refind_lost_episode(cx);
                        if ended {
                            if let Some(err) = this.playback.status.error.clone() {
                                this.toast("Playback stopped", err, cx);
                            }
                            // Nothing may play behind the closed view: an
                            // item that still plays after a load failed,
                            // or a load whose answer is on its way.
                            this.player.stop();
                            this.player.acknowledge_end();
                            this.playback.status = this.player.status();
                            // The one way out of the player view, as Stop
                            // takes it: the preview sheets go as well.
                            this.close_player_view(cx);
                            this.load_page(cx);
                        }
                        if changed || ended {
                            this.macos_player_changed(cx);
                        }
                        let idle = this.playback.last_activity.elapsed() > CONTROLS_HIDE_AFTER;
                        let should_show = !this.playback.open || !idle || this.controls_pinned(cx);
                        let controls_changed = should_show != this.playback.controls_visible;
                        this.playback.controls_visible = should_show;
                        // The pause screen comes up after a rest with no input.
                        let pause_screen = this.playback.open
                            && this.playback.pip.is_none()
                            && this.playback.status.paused
                            && this.playback.status.state == PlayState::Playing
                            && this.playback.last_activity.elapsed() > PAUSE_SCREEN_AFTER;
                        let pause_changed = pause_screen != this.playback.pause_screen;
                        this.playback.pause_screen = pause_screen;
                        if changed || ended || controls_changed || pause_changed {
                            cx.notify();
                        }
                        crate::pacing::trace::main_job("player poll", poll_started, crate::player::clock_ns());
                        !ended
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }
}

/// Whether the window leaves full screen as the player closes: only when
/// it is full screen now and was not before the item started.
fn leaves_fullscreen_with_the_player(fullscreen_before: bool, fullscreen_now: bool) -> bool {
    fullscreen_now && !fullscreen_before
}

#[cfg(test)]
mod fullscreen_tests {
    use super::leaves_fullscreen_with_the_player as leaves;

    #[test]
    fn full_screen_that_began_with_the_player_ends_with_it() {
        assert!(leaves(false, true), "entered during the item: back to the window");
        assert!(!leaves(true, true), "the window was full screen before: it stays");
        assert!(!leaves(false, false), "left during the item: nothing to do");
        assert!(!leaves(true, false), "left during the item: it is not put back");
    }
}
