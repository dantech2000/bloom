// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tap in the app: looks at the player, the window and the page, and
//! sends the events of what changed.

use std::time::{Duration, Instant};

use gpui_kit::{Context, Task};
use serde_json::json;

use super::{ItemInfo, Lights, Snapshot, artwork_colour, between, hub, picture_bands, socket_path};
use crate::{app::Bloom, player::PlayState};

/// Numbers the questions for artwork: an answer counts only for the
/// question that is still the newest. The number of a playback or the id
/// of a page can come again (the stream off and on, a page opened twice).
fn next_ask() -> u64 {
    static ASKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    ASKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// The state of the events in the app.
#[derive(Default)]
pub struct State {
    pub(super) last: Snapshot,
    pub(super) at: Option<Instant>,
    pub(super) session: u64,
    /// When the state for new readers was last written.
    pub(super) state_at: Option<Instant>,
    /// The colour of the artwork of what plays, once it is known.
    pub colour: Option<[u8; 3]>,
    /// The item of the detail page the readers were told of.
    pub(super) detail: Option<String>,
    /// The readers were told that a detail page opened, and not yet that
    /// it closed.
    detail_told: bool,
    /// The newest questions for the artwork of the page and of the film.
    detail_ask: u64,
    artwork_ask: u64,
    /// The count of the last look at the picture that was told.
    pub(super) colours_told: u64,
    /// The test film that runs ([`Bloom::events_test_run`]).
    pub(super) test_run: Option<Task<()>>,
    /// The lights of the settings (experimental).
    pub lights: Option<Lights>,
}

impl Bloom {
    /// Whether the stream is on.
    pub fn events_on(&self) -> bool {
        hub().on()
    }

    /// The setting "Events for other programs": on or off at once, and
    /// kept for the next start.
    pub fn set_events(&mut self, on: bool, cx: &mut Context<Self>) {
        if on {
            // The socket is new, also when the stream was on: its first
            // reader is told what plays now.
            let titles = self.config.events_titles == Some(true);
            let state = self.events.last.state_json(self.events.session, titles);
            if let Err(err) = hub().listen_with(socket_path(), state) {
                log::warn!("events: no socket at {}: {err}", socket_path());
                self.toast("Events", format!("The socket could not be opened: {err}"), cx);
            }
            // The lights were on when the events went off: their helper
            // has the events again. (A test instance starts none itself.)
            if hub().on() && self.config.lights == Some(true) && crate::options::options().test_name.is_none() {
                self.lights_helper(true);
            }
        } else {
            hub().stop();
            self.events = State { lights: self.events.lights.take(), ..State::default() };
            // The tick does not run with the stream off: the render thread
            // would look at frames for nobody.
            self.player.watch_colours(false);
        }
        self.config.events = Some(hub().on());
        // A detail page that is open now is told, or is closed.
        self.events_page(cx);
        self.save_config(cx);
        cx.notify();
    }

    /// Whether the events name the title of what plays.
    pub fn set_events_titles(&mut self, on: bool, cx: &mut Context<Self>) {
        self.config.events_titles = Some(on);
        self.save_config(cx);
        // A reader that connects now is told what plays from this copy:
        // with the titles off it must not have one.
        hub().set_state(self.events.last.state_json(self.events.session, on));
        cx.notify();
    }

    /// Whether the events tell the colours of the picture, for a light
    /// that follows the film (`frame.colours`).
    pub fn set_events_colours(&mut self, on: bool, cx: &mut Context<Self>) {
        self.config.events_colours = Some(on);
        self.save_config(cx);
        if !on {
            self.player.watch_colours(false);
        }
        // A detail page that is open now is told, or is closed.
        self.events_page(cx);
        cx.notify();
    }

    /// Opens the stream at the start of the app when the setting is on.
    pub fn start_events(&mut self) {
        if self.config.events == Some(true)
            && let Err(err) = hub().listen(socket_path())
        {
            log::warn!("events: no socket at {}: {err}", socket_path());
        }
    }

    fn events_snapshot(&self) -> Snapshot {
        let status = &self.player_status;
        let active = self.player_open && status.state == PlayState::Playing;
        Snapshot {
            active,
            item: self.playing.as_ref().filter(|_| active).map(|item| ItemInfo {
                id: item.id.clone(),
                kind: item.kind.to_string(),
                name: item.name.clone(),
                series: item.series_name.clone(),
                season: item.parent_index_number.map(i64::from),
                episode: item.index_number.map(i64::from),
                year: item.production_year.map(i64::from),
            }),
            paused: status.paused,
            buffering: status.buffering,
            position: status.position,
            duration: status.duration,
            rate: self.speed as f64,
            reached_end: status.reached_end,
            failed: status.error.is_some(),
            segment: self.current_segment().filter(|_| active).map(|segment| segment.kind.clone()),
            fullscreen: self.events_fullscreen(),
            pip: self.pip.is_some(),
        }
    }

    /// Looks at the player and the window, and sends the events of what
    /// changed since the last look. The poll of the player calls this, and
    /// so does the close of the player. Off, it is one read of a flag.
    pub(crate) fn events_tick(&mut self, cx: &mut Context<Self>) {
        if !hub().on() {
            return;
        }
        // The colours of the picture, when the setting is on and a program
        // reads: the render thread looks at a frame 12 times a second.
        let colours = self.config.events_colours == Some(true) && hub().readers() > 0;
        self.player.watch_colours(colours);
        let now = self.events_snapshot();
        let elapsed = self.events.at.replace(Instant::now()).map_or(0., |at| at.elapsed().as_secs_f64());
        let titles = self.config.events_titles == Some(true);
        let events = between(&self.events.last, &now, elapsed, &mut self.events.session, titles);
        // A reader that connects later is told what plays: at each event,
        // and once a second for the position.
        if !events.is_empty() || self.events.state_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
            self.events.state_at = Some(Instant::now());
            hub().set_state(now.state_json(self.events.session, titles));
        }
        let started = events.iter().any(|(event, _)| *event == "playback.started");
        for (event, data) in events {
            hub().emit(event, data);
        }
        if colours && self.events.last.active && now.active && !now.paused {
            let (count, bands) = self.player.colours();
            if count != self.events.colours_told && !bands.is_empty() {
                self.events.colours_told = count;
                hub().emit("frame.colours", json!({ "session": self.events.session, "colours": bands }));
            }
        }
        self.events.last = now;
        if started {
            // A film of the app ends a test film.
            self.events.test_run = None;
            self.events_artwork(cx);
        }
    }

    /// Looks at the artwork of what plays, away from the UI thread, and
    /// tells its colour (`artwork.colour`). A light can take the colour.
    fn events_artwork(&mut self, cx: &mut Context<Self>) {
        self.events.colour = None;
        let Some(item) = self.playing.clone() else { return };
        let session = self.events.session;
        let ask = next_ask();
        self.events.artwork_ask = ask;
        let agent = crate::images::agent(cx);
        self.fetch_in_session(
            cx,
            move |client| {
                let url = item.poster_url(&client, 160).or_else(|| item.wide_url(&client, 160));
                let url = url.ok_or_else(|| anyhow::anyhow!("the item has no artwork"))?;
                let bytes = crate::images::fetch_bytes(&agent, &url)?;
                Ok(artwork_colour(&crate::images::pixels(&bytes)?))
            },
            move |this, colour: anyhow::Result<Option<[u8; 3]>>, cx| {
                // The playback it was for is the one that runs.
                let (Ok(Some(colour)), true) = (colour, this.events.artwork_ask == ask && this.events.last.active)
                else {
                    return;
                };
                this.events.colour = Some(colour);
                hub().emit("artwork.colour", json!({ "session": session, "colour": colour }));
                cx.notify();
            },
        );
    }

    /// Tells the readers that a detail page opened, with the colours of
    /// its artwork from top to bottom, or that it closed. A light that
    /// follows the picture takes the colours at once, so it is with the
    /// film from its first frame. Only with the colours of the picture on.
    pub(crate) fn events_page(&mut self, cx: &mut Context<Self>) {
        self.lights_check(cx);
        let on = hub().on() && self.config.events_colours == Some(true);
        let item = match &self.page {
            crate::app::Page::Detail(detail) if on => Some(detail.item.clone()),
            _ => None,
        };
        let id = item.as_ref().map(|item| item.id.clone());
        if id == self.events.detail {
            return;
        }
        self.events.detail = id.clone();
        let ask = next_ask();
        self.events.detail_ask = ask;
        // With no session the question for the artwork gets no answer at
        // all: the page is one with no artwork.
        let Some((item, id)) = item.zip(id).filter(|_| self.session.is_some()) else {
            self.events_page_closed();
            return;
        };
        let agent = crate::images::agent(cx);
        let kind = item.kind.clone();
        self.fetch_in_session(
            cx,
            move |client| {
                let url = item.backdrop_url(&client, 320).or_else(|| item.poster_url(&client, 160));
                let url = url.ok_or_else(|| anyhow::anyhow!("the item has no artwork"))?;
                let picture = crate::images::pixels(&crate::images::fetch_bytes(&agent, &url)?)?;
                Ok(picture_bands(&picture, crate::player::COLOUR_BANDS))
            },
            move |this, bands: anyhow::Result<Vec<[u8; 3]>>, _| {
                // The page is still the one the colours were asked for.
                if this.events.detail_ask != ask {
                    return;
                }
                match bands {
                    Ok(bands) => {
                        this.events.detail_told = true;
                        hub().emit("detail.opened", json!({ "item": { "id": id, "type": kind.name() }, "colours": bands }));
                    }
                    // No artwork for this page: the page before it is
                    // left all the same, and a light gives its colours up.
                    Err(_) => this.events_page_closed(),
                }
            },
        );
    }

    /// Tells that the detail page closed, when its opening was told. From
    /// one detail page to the next there is no such event between the two:
    /// a light goes from the colours of one to those of the other.
    fn events_page_closed(&mut self) {
        if std::mem::take(&mut self.events.detail_told) {
            hub().emit("detail.closed", json!({}));
        }
    }
}
