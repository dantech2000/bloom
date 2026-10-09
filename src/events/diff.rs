// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the player and the window do at one moment, and the events
//! between two such moments.

use serde_json::{Value, json};

/// A jump of the position by more than this, against where the clock would
/// put it, is a seek.
const SEEK_SECS: f64 = 2.;

/// The item in the player, as an event names it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemInfo {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub series: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub year: Option<i64>,
}

/// What the player and the window do at one moment, as far as the events
/// care. Two of these, one after the other, give the events between them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// An item is loaded in the player and the player is on the screen.
    pub active: bool,
    pub item: Option<ItemInfo>,
    pub paused: bool,
    pub buffering: bool,
    pub position: f64,
    pub duration: f64,
    pub rate: f64,
    /// The item played to its end; an error of the player.
    pub reached_end: bool,
    pub failed: bool,
    /// The marked range the position is in: "Intro", "Outro", ...
    pub segment: Option<String>,
    pub fullscreen: bool,
    pub pip: bool,
}

impl Snapshot {
    pub(super) fn item_json(&self, titles: bool) -> Value {
        match &self.item {
            None => Value::Null,
            Some(item) if titles => json!({
                "id": item.id, "type": item.kind, "name": item.name, "series": item.series,
                "season": item.season, "episode": item.episode, "year": item.year,
            }),
            Some(item) => json!({ "id": item.id, "type": item.kind }),
        }
    }

    pub(super) fn window_json(&self) -> Value {
        json!({ "fullscreen": self.fullscreen, "pip": self.pip })
    }

    pub(super) fn playback_json(&self, session: u64, titles: bool) -> Value {
        json!({
            "session": session,
            "item": self.item_json(titles),
            "position": (self.position * 1000.).round() / 1000.,
            "duration": (self.duration * 1000.).round() / 1000.,
            "paused": self.paused,
            "buffering": self.buffering,
            // The speed is kept as 32 bits: 0.97 would read 0.9700000286.
            "rate": (self.rate * 1000.).round() / 1000.,
            "target": "local",
            "window": self.window_json(),
        })
    }

    /// What a reader that connects now is told.
    pub fn state_json(&self, session: u64, titles: bool) -> Value {
        json!({
            "playback": if self.active { self.playback_json(session, titles) } else { Value::Null },
            "window": self.window_json(),
        })
    }
}

/// The events between two snapshots, `elapsed` seconds apart. `session`
/// counts the playbacks: it moves on at each start, and every event of one
/// playback has the same number.
pub fn between(
    before: &Snapshot,
    now: &Snapshot,
    elapsed: f64,
    session: &mut u64,
    titles: bool,
) -> Vec<(&'static str, Value)> {
    let mut events = Vec::new();
    let same_item = before.item.as_ref().map(|i| &i.id) == now.item.as_ref().map(|i| &i.id);
    let goes_on = before.active && now.active && same_item;
    if before.active && !goes_on {
        // The end is told with the place the item was at.
        let mut data = before.playback_json(*session, titles);
        let (reason, to_end) = match (now.active, now.reached_end, now.failed) {
            (true, ..) => ("replaced", false),
            (false, true, false) => ("ended", true),
            (false, _, true) => ("error", false),
            (false, false, false) => ("user", false),
        };
        data["reason"] = json!(reason);
        data["played_to_end"] = json!(to_end);
        events.push(("playback.stopped", data));
    }
    if now.active && !goes_on {
        *session += 1;
        events.push(("playback.started", now.playback_json(*session, titles)));
    }
    if goes_on {
        let playback = || now.playback_json(*session, titles);
        if before.paused != now.paused {
            events.push((if now.paused { "playback.paused" } else { "playback.resumed" }, playback()));
        }
        if before.buffering != now.buffering {
            events.push((if now.buffering { "playback.stalled" } else { "playback.recovered" }, playback()));
        }
        // A seek: the position went back, or further than the clock can
        // have taken it. A position that stood still while the clock went
        // on is no seek (the Mac slept, or the app did not look for a while).
        let moved = now.position - before.position;
        let by_clock = if before.paused || before.buffering { 0. } else { elapsed * before.rate };
        if moved < -SEEK_SECS || moved > by_clock + SEEK_SECS {
            let mut data = playback();
            data["from"] = json!((before.position * 1000.).round() / 1000.);
            events.push(("playback.seeked", data));
        }
        if before.rate != now.rate {
            events.push(("playback.rate_changed", playback()));
        }
    }
    if now.active && (before.segment != now.segment || !goes_on) {
        if goes_on && let Some(kind) = &before.segment {
            events.push(("segment.exited", json!({ "session": *session, "kind": kind, "position": (now.position * 1000.).round() / 1000. })));
        }
        if let Some(kind) = &now.segment {
            events.push(("segment.entered", json!({ "session": *session, "kind": kind, "position": (now.position * 1000.).round() / 1000. })));
        }
    }
    if (before.fullscreen, before.pip) != (now.fullscreen, now.pip) {
        let mut data = now.window_json();
        data["player_open"] = json!(now.active);
        events.push(("window.changed", data));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn film() -> Snapshot {
        Snapshot {
            active: true,
            item: Some(ItemInfo { id: "m1".into(), kind: "Movie".into(), name: "A Film".into(), ..Default::default() }),
            duration: 6000.,
            rate: 1.,
            ..Default::default()
        }
    }

    fn names(events: &[(&'static str, Value)]) -> Vec<&'static str> {
        events.iter().map(|(name, _)| *name).collect()
    }

    /// One playback from its start to its end: each change is one event,
    /// all with the number of the playback, and time that passes is none.
    #[test]
    fn a_playback_is_told_from_its_start_to_its_end() {
        let mut session = 0;
        let idle = Snapshot::default();
        let mut now = film();
        let started = between(&idle, &now, 0.05, &mut session, false);
        assert_eq!(names(&started), ["playback.started"]);
        assert_eq!(started[0].1["session"], 1);
        assert_eq!(started[0].1["item"], json!({ "id": "m1", "type": "Movie" }), "the title is told without leave");

        // Time passes at the speed of the item: no event.
        let mut before = now.clone();
        now.position = 10.;
        assert!(between(&before, &now, 10., &mut session, false).is_empty());

        before = now.clone();
        now.paused = true;
        assert_eq!(names(&between(&before, &now, 0.05, &mut session, false)), ["playback.paused"]);
        // Paused, the position stands still: no seek.
        before = now.clone();
        assert!(between(&before, &now, 30., &mut session, false).is_empty());
        now.paused = false;
        assert_eq!(names(&between(&before, &now, 0.05, &mut session, false)), ["playback.resumed"]);

        before = now.clone();
        now.position = 900.;
        let seeked = between(&before, &now, 0.05, &mut session, false);
        assert_eq!(names(&seeked), ["playback.seeked"]);
        assert_eq!(seeked[0].1["from"], 10.);

        before = now.clone();
        now.buffering = true;
        assert_eq!(names(&between(&before, &now, 0.05, &mut session, false)), ["playback.stalled"]);
        before = now.clone();
        now.buffering = false;
        assert_eq!(names(&between(&before, &now, 4., &mut session, false)), ["playback.recovered"]);

        // The credits start, then the item plays to its end.
        before = now.clone();
        now.segment = Some("Outro".into());
        let credits = between(&before, &now, 0.05, &mut session, false);
        assert_eq!(names(&credits), ["segment.entered"]);
        assert_eq!(credits[0].1["kind"], "Outro");
        // Out of the credits again (a seek back): told with the place.
        before = now.clone();
        now.segment = None;
        let left = between(&before, &now, 0.05, &mut session, false);
        assert_eq!(names(&left), ["segment.exited"]);
        assert_eq!((&left[0].1["kind"], &left[0].1["position"]), (&json!("Outro"), &json!(now.position)));
        now.segment = Some("Outro".into());
        before = now.clone();
        let end = Snapshot { reached_end: true, ..Snapshot::default() };
        let stopped = between(&before, &end, 0.05, &mut session, false);
        assert_eq!(names(&stopped), ["playback.stopped"]);
        assert_eq!((&stopped[0].1["reason"], &stopped[0].1["played_to_end"]), (&json!("ended"), &json!(true)));
        assert_eq!(stopped[0].1["session"], 1);
        assert_eq!(session, 1);
    }

    /// The next item of a queue: the end of the one and the start of the
    /// other, with a new number. A stop by the user says so.
    /// A look that comes late is no seek: the position stood still or
    /// went on by the clock. A jump back or far ahead is one.
    #[test]
    fn a_late_look_is_not_a_seek() {
        let mut session = 1;
        let at = |position: f64| Snapshot { position, ..film() };
        let seeks = |before: f64, now: f64, elapsed: f64, session: &mut u64| {
            names(&between(&at(before), &at(now), elapsed, session, false)).contains(&"playback.seeked")
        };
        // The Mac slept for a minute; the film stood still, or went on.
        assert!(!seeks(100., 100.2, 60., &mut session));
        assert!(!seeks(100., 160., 60., &mut session));
        assert!(seeks(100., 40., 0.05, &mut session));
        assert!(seeks(100., 130., 0.05, &mut session));
        assert!(!seeks(100., 101., 0.05, &mut session));
        let speed = Snapshot { rate: f64::from(0.97f32), ..film() };
        assert_eq!(speed.playback_json(1, false)["rate"], 0.97);
    }

    #[test]
    fn the_next_item_is_a_new_playback() {
        let mut session = 4;
        let before = film();
        let mut next = film();
        next.item.as_mut().unwrap().id = "m2".into();
        let events = between(&before, &next, 0.05, &mut session, false);
        assert_eq!(names(&events), ["playback.stopped", "playback.started"]);
        assert_eq!((&events[0].1["session"], &events[0].1["reason"]), (&json!(4), &json!("replaced")));
        assert_eq!(events[1].1["session"], 5);
        let closed = between(&next, &Snapshot::default(), 0.05, &mut session, false);
        assert_eq!(closed[0].1["reason"], "user");
        let failed = between(&next, &Snapshot { failed: true, ..Default::default() }, 0.05, &mut session, false);
        assert_eq!(failed[0].1["reason"], "error");
    }

    /// The title is in an event only with the leave of the user, and the
    /// window is told when it changes.
    #[test]
    fn the_title_needs_leave_and_the_window_is_told() {
        let mut session = 0;
        let with = between(&Snapshot::default(), &film(), 0., &mut session, true);
        assert_eq!(with[0].1["item"]["name"], "A Film");
        let before = film();
        let now = Snapshot { fullscreen: true, ..film() };
        let events = between(&before, &now, 0.05, &mut session, false);
        assert_eq!(names(&events), ["window.changed"]);
        assert_eq!(events[0].1, json!({ "fullscreen": true, "pip": false, "player_open": true }));
        let line = now.state_json(1, false).to_string();
        assert!(!line.contains("A Film"), "{line}");
    }
}
