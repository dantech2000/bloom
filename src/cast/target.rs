// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one target of playback: this Mac, another Jellyfin session, a cast
//! device, or an AirPlay receiver. Only one is set at a time; choosing one
//! leaves the other. Each kind reports its state in its own shape; the
//! panel and the chip read one small view of it, made here. No part of the
//! app is in this file, so a test can check the transitions and the
//! mapping alone.

use std::time::Instant;

use super::protocol::SessionInfo;
use crate::{
    airplay::{AirPlayEvent, AirPlayState, AirPlayStatus},
    chromecast::{mdns::Device, session::Status},
};

/// Where playback goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Target {
    /// The player of this app.
    #[default]
    Local,
    /// Another session of the server, with its last known state.
    JellyfinSession(SessionInfo),
    /// A cast device; the connection lives in `Bloom::chromecast`.
    Chromecast(Device),
    /// The AirPlay receiver the user picked in the system picker. macOS
    /// gives no name for it.
    AirPlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Local,
    Jellyfin,
    Chromecast,
    AirPlay,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Local => "local",
            Kind::Jellyfin => "jellyfin",
            Kind::Chromecast => "chromecast",
            Kind::AirPlay => "airplay",
        }
    }
}

/// What leaving a target must undo.
#[derive(Clone, Debug, PartialEq)]
pub enum Leave {
    Jellyfin,
    Chromecast(Device),
    AirPlay,
}

impl Target {
    pub fn kind(&self) -> Kind {
        match self {
            Target::Local => Kind::Local,
            Target::JellyfinSession(_) => Kind::Jellyfin,
            Target::Chromecast(_) => Kind::Chromecast,
            Target::AirPlay => Kind::AirPlay,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Target::Local)
    }

    /// The name the panel shows.
    pub fn name(&self) -> String {
        match self {
            Target::Local => "This device".into(),
            Target::JellyfinSession(session) => session.device_name.clone(),
            Target::Chromecast(device) => device.name.clone(),
            Target::AirPlay => "AirPlay".into(),
        }
    }

    /// Two targets are the same device.
    pub fn same(&self, other: &Target) -> bool {
        match (self, other) {
            (Target::Local, Target::Local) | (Target::AirPlay, Target::AirPlay) => true,
            (Target::JellyfinSession(a), Target::JellyfinSession(b)) => a.id == b.id,
            (Target::Chromecast(a), Target::Chromecast(b)) => a.id == b.id,
            _ => false,
        }
    }

    /// What leaving this target undoes; nothing for this device.
    pub fn leave(&self) -> Option<Leave> {
        match self {
            Target::Local => None,
            Target::JellyfinSession(_) => Some(Leave::Jellyfin),
            Target::Chromecast(device) => Some(Leave::Chromecast(device.clone())),
            Target::AirPlay => Some(Leave::AirPlay),
        }
    }

    /// Chooses `chosen`: the target becomes it, and the old one is left,
    /// unless it is the same device.
    pub fn switch(self, chosen: Target) -> (Target, Option<Leave>) {
        if self.same(&chosen) {
            return (chosen, None);
        }
        let leave = self.leave();
        (chosen, leave)
    }

    /// What an event of the AirPlay engine makes of the target. A route
    /// the user picked (the player reports external playback) or an item
    /// sent to the engine makes AirPlay the target; the end of external
    /// playback, or of a local run, gives it up.
    pub fn after_airplay(self, event: &AirPlayEvent, status: &AirPlayStatus) -> Target {
        let loaded = matches!(
            status.state,
            AirPlayState::Loading | AirPlayState::Playing | AirPlayState::Paused | AirPlayState::Buffering
        );
        match event {
            AirPlayEvent::External(true) | AirPlayEvent::Loaded { .. } => Target::AirPlay,
            AirPlayEvent::State(AirPlayState::Loading) => Target::AirPlay,
            AirPlayEvent::External(false) if !loaded => self.give_up_airplay(),
            AirPlayEvent::Stopped | AirPlayEvent::Ended | AirPlayEvent::LoadFailed { .. } if !status.external => {
                self.give_up_airplay()
            }
            _ => self,
        }
    }

    fn give_up_airplay(self) -> Target {
        if matches!(self, Target::AirPlay) { Target::Local } else { self }
    }
}

/// The state of the target as the panel and the chip show it, whatever
/// its kind. A control the kind lacks is hidden (`can_*`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TargetView {
    pub kind: Option<Kind>,
    pub name: String,
    /// A second line under the name: the client, the model, or the kind.
    pub detail: String,
    /// The connection to the device is up (always for a Jellyfin session
    /// and for AirPlay).
    pub connected: bool,
    pub error: Option<String>,
    /// Something is loaded: the transport shows.
    pub loaded: bool,
    pub title: Option<String>,
    pub item_id: Option<String>,
    /// Seconds, moved on by the time since the last report while playing.
    pub position: f64,
    /// Seconds; 0 while not known.
    pub duration: f64,
    pub paused: bool,
    /// 0 to 100; none when the kind has no volume of its own.
    pub volume: Option<f32>,
    pub muted: bool,
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
    pub can_seek: bool,
    /// Next and previous in the queue of the target.
    pub can_skip: bool,
    /// Audio and subtitle streams of the item can be chosen.
    pub can_tracks: bool,
}

impl TargetView {
    /// This device.
    pub fn local() -> Self {
        Self { kind: Some(Kind::Local), name: "This device".into(), detail: "Plays here".into(), connected: true, ..Default::default() }
    }

    pub fn fraction(&self) -> f32 {
        if self.duration > 0. { (self.position / self.duration).clamp(0., 1.) as f32 } else { 0. }
    }
}

/// A Jellyfin session: the last report, moved on by the time since `at`
/// while it plays.
pub fn from_session(session: &SessionInfo, at: Option<Instant>) -> TargetView {
    let state = &session.play_state;
    let reported = session.position_secs();
    let duration = session.duration_secs();
    let loaded = session.now_playing_item.is_some();
    let position = if state.is_paused || !loaded {
        reported
    } else {
        let since = at.map_or(0., |at| at.elapsed().as_secs_f64());
        let moved = reported + since;
        if duration > 0. { moved.min(duration) } else { moved }
    };
    TargetView {
        kind: Some(Kind::Jellyfin),
        name: session.device_name.clone(),
        detail: session.client.clone(),
        connected: true,
        error: None,
        loaded,
        title: session.now_playing_title(),
        item_id: session.now_playing_item.as_ref().map(|item| item.id.clone()),
        position,
        duration,
        paused: state.is_paused,
        volume: Some(state.volume_level.unwrap_or(100).clamp(0, 100) as f32),
        muted: state.is_muted,
        audio_index: state.audio_stream_index,
        subtitle_index: state.subtitle_stream_index,
        can_seek: true,
        can_skip: true,
        can_tracks: true,
    }
}

/// The streams the Jellyfin receiver app reported it plays.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CastTracks {
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
}

/// A cast device: the status of the connection, with the title of the item
/// this app loaded on it (the device reports only the id).
pub fn from_chromecast(device: &Device, status: &Status, title: Option<String>, tracks: CastTracks) -> TargetView {
    let loaded = matches!(status.player_state.as_str(), "PLAYING" | "PAUSED" | "BUFFERING");
    let jellyfin_app = status.playing_jellyfin_app();
    TargetView {
        kind: Some(Kind::Chromecast),
        name: device.name.clone(),
        detail: if device.model.is_empty() { "Chromecast".into() } else { device.model.clone() },
        connected: status.connected,
        error: status.error.clone(),
        loaded,
        title: if loaded { title.or_else(|| status.item_id.clone()) } else { None },
        item_id: status.item_id.clone(),
        position: status.position_now(),
        duration: status.duration.unwrap_or(0.),
        paused: status.player_state == "PAUSED",
        volume: Some((status.volume * 100.).round().clamp(0., 100.) as f32),
        muted: status.muted,
        audio_index: tracks.audio_index,
        subtitle_index: tracks.subtitle_index,
        can_seek: true,
        can_skip: false,
        // Only the Jellyfin receiver app takes a stream index.
        can_tracks: loaded && jellyfin_app,
    }
}

/// The AirPlay engine: the item it holds, playing on the receiver or, in a
/// local run, on this Mac.
pub fn from_airplay(status: &AirPlayStatus) -> TargetView {
    let loaded = matches!(
        status.state,
        AirPlayState::Loading | AirPlayState::Playing | AirPlayState::Paused | AirPlayState::Buffering
    );
    let position = match status.position_at {
        Some(at) if status.state == AirPlayState::Playing => status.position + at.elapsed().as_secs_f64(),
        _ => status.position,
    };
    let position = if status.duration > 0. { position.min(status.duration) } else { position };
    TargetView {
        kind: Some(Kind::AirPlay),
        name: "AirPlay".into(),
        detail: match (status.external, status.route.as_deref()) {
            (_, Some(route)) => route.to_string(),
            (true, None) => "Apple TV or AirPlay device".into(),
            (false, None) => "No receiver picked".into(),
        },
        connected: true,
        error: status.error.clone(),
        loaded,
        title: loaded.then(|| status.title.clone()).filter(|t| !t.is_empty()),
        item_id: status.item_id.clone(),
        position,
        duration: status.duration,
        paused: status.state == AirPlayState::Paused,
        volume: Some((status.volume * 100.).round().clamp(0., 100.)),
        muted: status.muted,
        audio_index: None,
        subtitle_index: None,
        can_seek: true,
        can_skip: false,
        can_tracks: false,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::chromecast::messages::App;

    fn session(id: &str, name: &str) -> SessionInfo {
        serde_json::from_value(json!({
            "Id": id, "DeviceName": name, "Client": "Jellyfin Web", "SupportsRemoteControl": true,
            "NowPlayingItem": { "Id": "item1", "Name": "Film", "Type": "Movie", "RunTimeTicks": 6_000_000_000_i64 },
            "PlayState": { "PositionTicks": 300_000_000_i64, "IsPaused": false, "VolumeLevel": 40, "IsMuted": true, "AudioStreamIndex": 1, "SubtitleStreamIndex": -1 },
        }))
        .unwrap()
    }

    fn device(id: &str) -> Device {
        Device {
            id: id.into(),
            name: "Living room".into(),
            model: "Google TV".into(),
            address: std::net::Ipv4Addr::LOCALHOST,
            port: 8009,
            instance: "x".into(),
        }
    }

    #[test]
    fn choosing_a_target_leaves_the_one_before() {
        // From this device nothing is left.
        let (target, leave) = Target::Local.switch(Target::JellyfinSession(session("s1", "TV")));
        assert_eq!(target.kind(), Kind::Jellyfin);
        assert_eq!(leave, None);
        // Jellyfin to Chromecast leaves the session.
        let (target, leave) = target.switch(Target::Chromecast(device("c1")));
        assert_eq!(target.kind(), Kind::Chromecast);
        assert_eq!(leave, Some(Leave::Jellyfin));
        // The same device again leaves nothing.
        let (target, leave) = target.switch(Target::Chromecast(device("c1")));
        assert_eq!(target, Target::Chromecast(device("c1")));
        assert_eq!(leave, None);
        // Another cast device leaves the first, by its id.
        let (target, leave) = target.switch(Target::Chromecast(device("c2")));
        assert_eq!(leave, Some(Leave::Chromecast(device("c1"))));
        // AirPlay leaves the cast device; back to this device leaves AirPlay.
        let (target, leave) = target.switch(Target::AirPlay);
        assert_eq!(target, Target::AirPlay);
        assert_eq!(leave, Some(Leave::Chromecast(device("c2"))));
        let (target, leave) = target.switch(Target::Local);
        assert!(target.is_local());
        assert_eq!(leave, Some(Leave::AirPlay));
        assert_eq!(Target::Local.switch(Target::Local), (Target::Local, None));
        // Names and kinds for the chip and the debug channel.
        assert_eq!(Target::AirPlay.name(), "AirPlay");
        assert_eq!(Target::Chromecast(device("c1")).name(), "Living room");
        assert_eq!(Target::JellyfinSession(session("s", "Bedroom")).name(), "Bedroom");
        assert_eq!(Kind::Chromecast.name(), "chromecast");
    }

    #[test]
    fn airplay_events_set_and_clear_the_target() {
        let idle = AirPlayStatus::default();
        let playing = AirPlayStatus { state: AirPlayState::Playing, external: true, ..Default::default() };
        let local_run = AirPlayStatus { state: AirPlayState::Playing, external: false, ..Default::default() };
        // A route picked: AirPlay, whatever was set before.
        assert_eq!(Target::Local.after_airplay(&AirPlayEvent::External(true), &idle), Target::AirPlay);
        let cast = Target::Chromecast(device("c1"));
        assert_eq!(cast.clone().after_airplay(&AirPlayEvent::External(true), &idle), Target::AirPlay);
        // An item sent to the engine as well (a local run in a test).
        assert_eq!(Target::Local.after_airplay(&AirPlayEvent::Loaded { token: 1 }, &local_run), Target::AirPlay);
        assert_eq!(Target::Local.after_airplay(&AirPlayEvent::State(AirPlayState::Loading), &idle), Target::AirPlay);
        // External playback ends with nothing loaded: back to this device.
        assert_eq!(Target::AirPlay.after_airplay(&AirPlayEvent::External(false), &idle), Target::Local);
        // ...but not while an item still plays locally in the engine.
        assert_eq!(Target::AirPlay.after_airplay(&AirPlayEvent::External(false), &local_run), Target::AirPlay);
        // A stop of a local run gives AirPlay up; a stop while the route
        // is still external keeps it for the next item.
        assert_eq!(Target::AirPlay.after_airplay(&AirPlayEvent::Stopped, &idle), Target::Local);
        assert_eq!(Target::AirPlay.after_airplay(&AirPlayEvent::Ended, &idle), Target::Local);
        assert_eq!(Target::AirPlay.after_airplay(&AirPlayEvent::Stopped, &playing), Target::AirPlay);
        // Position samples change nothing.
        let sample = AirPlayEvent::Position(crate::player::Sample { position: 1., at: Instant::now(), paused: false, duration: 2. });
        assert_eq!(Target::AirPlay.after_airplay(&sample, &playing), Target::AirPlay);
        // Events of the engine do not touch another kind of target.
        assert_eq!(cast.clone().after_airplay(&AirPlayEvent::External(false), &idle), cast);
        assert_eq!(cast.clone().after_airplay(&AirPlayEvent::Stopped, &idle), cast);
    }

    #[test]
    fn maps_a_jellyfin_session() {
        let s = session("s1", "TV");
        let view = from_session(&s, Some(Instant::now() - Duration::from_secs(2)));
        assert_eq!(view.kind, Some(Kind::Jellyfin));
        assert_eq!(view.name, "TV");
        assert_eq!(view.detail, "Jellyfin Web");
        assert!(view.connected && view.loaded);
        assert_eq!(view.title.as_deref(), Some("Film"));
        assert_eq!(view.item_id.as_deref(), Some("item1"));
        // 30 s reported, about 2 s ago, while playing.
        assert!((view.position - 32.).abs() < 0.5, "{}", view.position);
        assert_eq!(view.duration, 600.);
        assert!(!view.paused);
        assert_eq!(view.volume, Some(40.));
        assert!(view.muted);
        assert_eq!(view.audio_index, Some(1));
        assert_eq!(view.subtitle_index, Some(-1));
        assert!(view.can_seek && view.can_skip && view.can_tracks);
        // Paused: the position stays.
        let mut paused = s.clone();
        paused.play_state.is_paused = true;
        let view = from_session(&paused, Some(Instant::now() - Duration::from_secs(2)));
        assert_eq!(view.position, 30.);
        assert!(view.paused);
        // Nothing playing: no title, no transport.
        let mut idle = s;
        idle.now_playing_item = None;
        let view = from_session(&idle, None);
        assert!(!view.loaded);
        assert_eq!(view.title, None);
        assert_eq!(view.fraction(), 0.);
    }

    #[test]
    fn maps_a_cast_device() {
        let status = Status {
            connected: true,
            app: Some(App {
                app_id: crate::chromecast::jellyfin::APP_STABLE.into(),
                display_name: "Jellyfin".into(),
                session_id: "s".into(),
                transport_id: "t".into(),
                namespaces: Vec::new(),
                is_idle_screen: false,
                status_text: String::new(),
            }),
            volume: 0.35,
            muted: false,
            player_state: "PLAYING".into(),
            position: 100.,
            position_at: Some(Instant::now() - Duration::from_secs(1)),
            duration: Some(600.),
            item_id: Some("item1".into()),
            ..Default::default()
        };
        let tracks = CastTracks { audio_index: Some(2), subtitle_index: Some(-1) };
        let view = from_chromecast(&device("c1"), &status, Some("Film".into()), tracks);
        assert_eq!(view.kind, Some(Kind::Chromecast));
        assert_eq!(view.name, "Living room");
        assert_eq!(view.detail, "Google TV");
        assert!(view.connected && view.loaded && !view.paused);
        assert_eq!(view.title.as_deref(), Some("Film"));
        assert!((view.position - 101.).abs() < 0.5, "{}", view.position);
        assert_eq!(view.duration, 600.);
        assert_eq!(view.volume, Some(35.));
        assert_eq!(view.audio_index, Some(2));
        assert!(view.can_seek && !view.can_skip && view.can_tracks);
        // The media receiver takes no stream index.
        let mut default_app = status.clone();
        default_app.app.as_mut().unwrap().app_id = crate::chromecast::messages::APP_DEFAULT_MEDIA.into();
        default_app.player_state = "PAUSED".into();
        let view = from_chromecast(&device("c1"), &default_app, None, CastTracks::default());
        assert!(view.paused && !view.can_tracks);
        assert_eq!(view.title.as_deref(), Some("item1"), "falls back to the id");
        // Idle, not connected: no transport, the error shows.
        let down = Status { connected: false, error: Some("gone".into()), ..Default::default() };
        let view = from_chromecast(&Device { model: String::new(), ..device("c1") }, &down, None, CastTracks::default());
        assert!(!view.connected && !view.loaded);
        assert_eq!(view.detail, "Chromecast");
        assert_eq!(view.error.as_deref(), Some("gone"));
        assert_eq!(view.volume, Some(0.));
    }

    #[test]
    fn maps_the_airplay_engine() {
        let status = AirPlayStatus {
            state: AirPlayState::Playing,
            item_id: Some("item1".into()),
            title: "Film".into(),
            position: 10.,
            position_at: Some(Instant::now() - Duration::from_secs(1)),
            duration: 600.,
            external: true,
            volume: 0.8,
            muted: true,
            ..Default::default()
        };
        let view = from_airplay(&status);
        assert_eq!(view.kind, Some(Kind::AirPlay));
        assert_eq!(view.name, "AirPlay");
        assert_eq!(view.detail, "Apple TV or AirPlay device");
        assert!(view.loaded && !view.paused);
        assert_eq!(view.title.as_deref(), Some("Film"));
        assert!((view.position - 11.).abs() < 0.5, "{}", view.position);
        assert_eq!(view.volume, Some(80.));
        assert!(view.muted);
        assert!(view.can_seek && !view.can_skip && !view.can_tracks);
        // Paused: the position stays; a local run says so.
        let paused = AirPlayStatus { state: AirPlayState::Paused, external: false, ..status.clone() };
        let view = from_airplay(&paused);
        assert_eq!(view.position, 10.);
        assert!(view.paused);
        assert_eq!(view.detail, "No receiver picked");
        // Idle: nothing loaded, no title.
        let view = from_airplay(&AirPlayStatus::default());
        assert!(!view.loaded);
        assert_eq!(view.title, None);
        // Ended: the item is gone from the transport.
        let ended = AirPlayStatus { state: AirPlayState::Ended, ..status };
        assert!(!from_airplay(&ended).loaded);
    }
}
