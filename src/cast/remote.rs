// SPDX-License-Identifier: AGPL-3.0-or-later
//! The target that is set, as one thing to steer: [`RemoteTarget`] is what
//! the panel, the chip and the media keys ask of it, and each kind of
//! target answers in its own way. A Jellyfin session is told through the
//! server; a cast device through its connection; an AirPlay receiver
//! through the engine of this app.
//!
//! A control acts at once where it can (a cast device, AirPlay) and gives
//! back an [`Effect`] for what only the app can do: a request to the
//! server off the UI thread, a toast. `Bloom::cast_effect` runs it.

use std::time::Instant;

use gpui_kit::Task;

use super::{
    protocol::{self, PlayCommand, SessionInfo},
    target::{self, CastTracks, Kind, Target, TargetView},
};
use crate::{
    airplay::{AirPlay, SendRequest, StreamOptions},
    chromecast::{
        discovery::Device,
        jellyfin as cast_jellyfin,
        session::{Event as CastEvent, Session},
    },
    jellyfin::{Client, Item},
};

/// A request to the server for a target, with the toast of its success.
pub struct Request {
    pub work: Box<dyn FnOnce(Client) -> anyhow::Result<()> + Send>,
    pub done: Option<String>,
}

/// What a control leaves for the app to do.
#[derive(Default)]
pub struct Effect {
    /// Goes to the server off the UI thread; a failure shows as a toast.
    pub request: Option<Request>,
    /// Said at once.
    pub toast: Option<String>,
    /// The item counts as the one this app plays: the AirPlay engine is a
    /// player of this app, and it reports the item as the player does.
    pub playing: Option<Item>,
}

impl Effect {
    fn request(work: impl FnOnce(Client) -> anyhow::Result<()> + Send + 'static, done: Option<String>) -> Self {
        Self { request: Some(Request { work: Box::new(work), done }), ..Default::default() }
    }

    fn toast(text: String) -> Self {
        Self { toast: Some(text), ..Default::default() }
    }
}

/// A stream of an item that a target can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Track {
    Audio,
    Subtitle,
}

/// Items to play, with what a target may need of the session of the app.
pub struct Play<'a> {
    pub items: &'a [Item],
    pub start_secs: f64,
    /// The first item goes on from `start_secs`.
    pub resume: bool,
    pub client: &'a Client,
    pub server_id: &'a str,
    /// Bits per second a stream may take, as the user set it for the player.
    pub max_bitrate: Option<u64>,
}

/// What every kind of target does. A control the kind lacks does nothing
/// (and the panel hides it: `TargetView::can_*`).
pub trait RemoteTarget {
    /// The choice this target is, for the panel and for "is it the same
    /// device again".
    fn choice(&self) -> Target;

    /// Its state as the panel and the chip show it.
    fn view(&self) -> TargetView;

    /// The item it plays, when this app knows it: for "Play here instead"
    /// and for the tracks of the panel.
    fn item(&self) -> Option<&Item>;

    fn play(&mut self, play: Play) -> Effect;

    fn set_paused(&mut self, paused: bool) -> Effect;

    fn seek(&mut self, secs: f64) -> Effect;

    fn stop(&mut self) -> Effect;

    /// Next or previous in the queue of the target.
    fn skip(&mut self, _next: bool) -> Effect {
        Effect::default()
    }

    /// 0 to 100.
    fn set_volume(&mut self, volume: f32) -> Effect;

    fn set_muted(&mut self, muted: bool) -> Effect;

    /// A stream of the item by its index on the server; -1 turns
    /// subtitles off.
    fn set_track(&mut self, _track: Track, _index: i64) -> Effect {
        Effect::default()
    }

    /// The target is left: what this app put on it ends, where the kind
    /// says so.
    fn leave(&mut self) -> Effect {
        Effect::default()
    }
}

/// The target that is set.
pub enum Remote {
    Jellyfin(JellyfinRemote),
    Chromecast(ChromecastRemote),
    AirPlay(AirPlayRemote),
}

impl Remote {
    pub fn target(&self) -> &dyn RemoteTarget {
        match self {
            Remote::Jellyfin(remote) => remote,
            Remote::Chromecast(remote) => remote,
            Remote::AirPlay(remote) => remote,
        }
    }

    pub fn target_mut(&mut self) -> &mut dyn RemoteTarget {
        match self {
            Remote::Jellyfin(remote) => remote,
            Remote::Chromecast(remote) => remote,
            Remote::AirPlay(remote) => remote,
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Remote::Jellyfin(_) => Kind::Jellyfin,
            Remote::Chromecast(_) => Kind::Chromecast,
            Remote::AirPlay(_) => Kind::AirPlay,
        }
    }
}

// ----- another session of the server ---------------------------------------

/// A Jellyfin session: its last known state, which a command of this app
/// changes at once (the panel shows the new state; the report of the
/// session confirms it).
pub struct JellyfinRemote {
    pub session: SessionInfo,
    /// When the state came, so the position can move between reports.
    at: Instant,
    /// Reads the sessions on a timer while this is the target.
    pub poll: Option<Task<()>>,
}

impl JellyfinRemote {
    pub fn new(session: SessionInfo) -> Self {
        Self { session, at: Instant::now(), poll: None }
    }

    /// A report of the session came.
    pub fn update(&mut self, fresh: SessionInfo) {
        self.session = fresh;
        self.at = Instant::now();
    }

    /// Items for the session: now, next, or at the end of its queue.
    pub fn send_play(&self, item_ids: Vec<String>, command: PlayCommand, start_secs: Option<f64>) -> Effect {
        let (id, name) = (self.session.id.clone(), &self.session.device_name);
        let query = protocol::play_query(&item_ids, command, start_secs);
        let what = match command {
            PlayCommand::PlayNow => format!("Playing on {name}."),
            PlayCommand::PlayNext => format!("Plays next on {name}."),
            PlayCommand::PlayLast => format!("Added to the queue of {name}."),
        };
        Effect::request(move |client| client.call("POST", &format!("/Sessions/{id}/Playing"), &query), Some(what))
    }

    /// A playstate command: Pause, Unpause, PlayPause, Stop, NextTrack,
    /// PreviousTrack, or Seek with a position.
    pub fn playstate(&mut self, command: &str, seek_secs: Option<f64>) -> Effect {
        let position = self.view().position;
        let state = &mut self.session.play_state;
        match command {
            "Pause" => state.is_paused = true,
            "Unpause" => state.is_paused = false,
            "PlayPause" => state.is_paused = !state.is_paused,
            "Seek" => state.position_ticks = seek_secs.map(protocol::secs_to_ticks),
            "Stop" => self.session.now_playing_item = None,
            _ => {}
        }
        if command != "Seek" {
            self.session.play_state.position_ticks = Some(protocol::secs_to_ticks(position));
        }
        self.at = Instant::now();
        let mut query = Vec::new();
        if let Some(secs) = seek_secs.filter(|_| command == "Seek") {
            query.push(("seekPositionTicks", protocol::secs_to_ticks(secs).to_string()));
        }
        let path = format!("/Sessions/{}/Playing/{command}", self.session.id);
        Effect::request(move |client| client.call("POST", &path, &query), None)
    }

    /// A general command, with its arguments.
    pub fn general(&mut self, name: &str, arguments: &[(&str, String)]) -> Effect {
        let state = &mut self.session.play_state;
        match (name, arguments.first()) {
            ("SetVolume", Some((_, value))) => {
                state.volume_level = value.parse().ok();
                state.is_muted = false;
            }
            ("ToggleMute", _) => state.is_muted = !state.is_muted,
            ("Mute", _) => state.is_muted = true,
            ("Unmute", _) => state.is_muted = false,
            ("SetAudioStreamIndex", Some((_, value))) => state.audio_stream_index = value.parse().ok(),
            ("SetSubtitleStreamIndex", Some((_, value))) => state.subtitle_stream_index = value.parse().ok(),
            _ => {}
        }
        let body = protocol::general_command(name, arguments);
        let path = format!("/Sessions/{}/Command", self.session.id);
        Effect::request(move |client| client.post(&path, &body).map(drop), None)
    }
}

impl RemoteTarget for JellyfinRemote {
    fn choice(&self) -> Target {
        Target::JellyfinSession(self.session.clone())
    }

    fn view(&self) -> TargetView {
        target::from_session(&self.session, Some(self.at))
    }

    fn item(&self) -> Option<&Item> {
        self.session.now_playing_item.as_ref()
    }

    fn play(&mut self, play: Play) -> Effect {
        let ids = play.items.iter().map(|item| item.id.clone()).collect();
        self.send_play(ids, PlayCommand::PlayNow, play.resume.then_some(play.start_secs))
    }

    fn set_paused(&mut self, paused: bool) -> Effect {
        self.playstate(if paused { "Pause" } else { "Unpause" }, None)
    }

    fn seek(&mut self, secs: f64) -> Effect {
        self.playstate("Seek", Some(secs))
    }

    fn stop(&mut self) -> Effect {
        self.playstate("Stop", None)
    }

    fn skip(&mut self, next: bool) -> Effect {
        self.playstate(if next { "NextTrack" } else { "PreviousTrack" }, None)
    }

    fn set_volume(&mut self, volume: f32) -> Effect {
        self.general("SetVolume", &[("Volume", format!("{}", volume.round()))])
    }

    fn set_muted(&mut self, muted: bool) -> Effect {
        if self.session.play_state.is_muted == muted {
            return Effect::default();
        }
        self.general("ToggleMute", &[])
    }

    fn set_track(&mut self, track: Track, index: i64) -> Effect {
        let name = match track {
            Track::Audio => "SetAudioStreamIndex",
            Track::Subtitle => "SetSubtitleStreamIndex",
        };
        self.general(name, &[("Index", index.to_string())])
    }
}

// ----- a cast device ---------------------------------------------------------

/// What this app loaded on a cast device: the device reports only the id
/// of the item.
#[derive(Default)]
pub struct CastLoad {
    pub item: Option<Item>,
    /// The number the connection gave the load of that item; the end of
    /// the item comes with it (`CastEvent::Ended`).
    load: u64,
    /// The streams the Jellyfin receiver app says it plays.
    pub tracks: CastTracks,
}

impl CastLoad {
    fn sent(&mut self, item: Item, load: u64) {
        *self = Self { item: Some(item), load, tracks: CastTracks::default() };
    }

    /// An event of the connection: a report of the Jellyfin receiver app
    /// names the streams it plays, and the end of a load takes its item.
    /// Only the load of the item the panel shows counts: a new item makes
    /// the device change its app, and the end of the app before it comes
    /// with the number of the item before it.
    pub fn note(&mut self, event: &CastEvent) {
        match event {
            CastEvent::Message { namespace, payload } if namespace == cast_jellyfin::NAMESPACE => {
                if let Some(report) = cast_jellyfin::parse_report(payload) {
                    if report.audio_index.is_some() {
                        self.tracks.audio_index = report.audio_index;
                    }
                    if report.subtitle_index.is_some() {
                        self.tracks.subtitle_index = report.subtitle_index;
                    }
                }
            }
            CastEvent::Ended { load } if *load == self.load => self.item = None,
            _ => {}
        }
    }
}

/// A cast device with its connection. Dropping it closes the connection.
pub struct ChromecastRemote {
    pub device: Device,
    pub session: Session,
    pub loaded: CastLoad,
    /// Reads the events of the connection; it ends with this.
    pub _task: Task<()>,
}

impl RemoteTarget for ChromecastRemote {
    fn choice(&self) -> Target {
        Target::Chromecast(self.device.clone())
    }

    fn view(&self) -> TargetView {
        let title = self.loaded.item.as_ref().map(Item::display_title);
        target::from_chromecast(&self.device, &self.session.status(), title, self.loaded.tracks)
    }

    fn item(&self) -> Option<&Item> {
        self.loaded.item.as_ref()
    }

    /// The Jellyfin receiver app with the identity of this session takes
    /// the items as its queue, and the Default Media Receiver the stream
    /// URL of the first one when that app does not launch.
    fn play(&mut self, play: Play) -> Effect {
        let Some(first) = play.items.first() else {
            return Effect::default();
        };
        let request = cast_jellyfin::load_request(play.client, play.server_id, play.items, play.start_secs);
        let load = self.session.load(request);
        self.loaded.sent(first.clone(), load);
        Effect::toast(format!("Playing on {}.", self.device.name))
    }

    fn set_paused(&mut self, paused: bool) -> Effect {
        if paused { self.session.pause() } else { self.session.play() }
        Effect::default()
    }

    fn seek(&mut self, secs: f64) -> Effect {
        self.session.seek(secs);
        Effect::default()
    }

    fn stop(&mut self) -> Effect {
        self.session.stop_media();
        Effect::default()
    }

    fn set_volume(&mut self, volume: f32) -> Effect {
        self.session.set_volume(f64::from(volume) / 100.);
        Effect::default()
    }

    fn set_muted(&mut self, muted: bool) -> Effect {
        self.session.set_muted(muted);
        Effect::default()
    }

    fn set_track(&mut self, track: Track, index: i64) -> Effect {
        match track {
            Track::Audio => {
                self.loaded.tracks.audio_index = Some(index);
                self.session.set_audio(index);
            }
            Track::Subtitle => {
                self.loaded.tracks.subtitle_index = Some(index);
                self.session.set_subtitle((index >= 0).then_some(index));
            }
        }
        Effect::default()
    }

    /// The device keeps what another sender put on it; what this app
    /// loaded ends with the target.
    fn leave(&mut self) -> Effect {
        log::info!("cast: leaving {:?}", self.device.name);
        if self.loaded.item.is_some() && self.session.status().loaded() {
            self.session.stop_media();
        }
        Effect::default()
    }
}

// ----- AirPlay ---------------------------------------------------------------

/// What a send to the AirPlay engine took of the session of the app, for
/// the same item again with another stream.
struct Sent {
    client: Client,
    max_bitrate: Option<u64>,
}

/// The AirPlay engine of this app, with the receiver the user picked in
/// the system picker.
pub struct AirPlayRemote {
    airplay: AirPlay,
    /// The item the engine has, when it came through the app.
    pub item: Option<Item>,
    sent: Option<Sent>,
    /// The streams the user chose in the panel: the stream the receiver
    /// fetches is made with them.
    tracks: CastTracks,
}

impl AirPlayRemote {
    /// `item` is what the engine plays already, when the app knows it.
    pub fn new(airplay: AirPlay, item: Option<Item>) -> Self {
        Self { airplay, item, sent: None, tracks: CastTracks::default() }
    }

    /// Sends the item to the engine, with the streams that are chosen.
    fn send(&self, item: &Item, start_secs: f64, sent: &Sent) {
        self.airplay.send(send_request(&sent.client, item, start_secs, sent.max_bitrate, self.tracks));
    }
}

/// An item for the AirPlay engine. The limit the user set for the player
/// holds for the receiver too. An audio stream that is chosen is the one
/// of the stream; a subtitle that is chosen is burned into the picture
/// (AirPlay carries no subtitle track of ours).
pub fn send_request(
    client: &Client,
    item: &Item,
    start_secs: f64,
    max_bitrate: Option<u64>,
    tracks: CastTracks,
) -> SendRequest {
    SendRequest {
        client: client.clone(),
        item_id: item.id.clone(),
        title: item.display_title(),
        start_secs,
        options: StreamOptions {
            max_bitrate,
            audio_index: tracks.audio_index,
            subtitle_index: tracks.subtitle_index.filter(|index| *index >= 0),
            ..StreamOptions::new()
        },
    }
}

impl RemoteTarget for AirPlayRemote {
    fn choice(&self) -> Target {
        Target::AirPlay
    }

    fn view(&self) -> TargetView {
        let mut view = target::from_airplay(&self.airplay.status());
        // The streams can change for an item that came through the app:
        // the item goes to the engine again from where it is.
        view.can_tracks = view.loaded && self.item.is_some() && self.sent.is_some();
        view.audio_index = self.tracks.audio_index;
        view.subtitle_index = self.tracks.subtitle_index;
        view
    }

    fn item(&self) -> Option<&Item> {
        self.item.as_ref()
    }

    fn play(&mut self, play: Play) -> Effect {
        let Some(first) = play.items.first() else {
            return Effect::default();
        };
        let sent = Sent { client: play.client.clone(), max_bitrate: play.max_bitrate };
        // A new item starts with the streams of the server's choice.
        self.tracks = CastTracks::default();
        self.send(first, play.start_secs, &sent);
        self.item = Some(first.clone());
        self.sent = Some(sent);
        Effect { playing: Some(first.clone()), ..Default::default() }
    }

    fn set_paused(&mut self, paused: bool) -> Effect {
        if paused { self.airplay.pause() } else { self.airplay.play() }
        Effect::default()
    }

    fn seek(&mut self, secs: f64) -> Effect {
        self.airplay.seek(secs);
        Effect::default()
    }

    fn stop(&mut self) -> Effect {
        self.airplay.stop();
        Effect::default()
    }

    fn set_volume(&mut self, volume: f32) -> Effect {
        self.airplay.set_volume(volume / 100.);
        Effect::default()
    }

    fn set_muted(&mut self, muted: bool) -> Effect {
        self.airplay.set_muted(muted);
        Effect::default()
    }

    /// The stream of an item is made for one audio stream and with one
    /// subtitle in the picture: another choice is the item sent again,
    /// from where it is.
    fn set_track(&mut self, track: Track, index: i64) -> Effect {
        match track {
            Track::Audio => self.tracks.audio_index = Some(index),
            Track::Subtitle => self.tracks.subtitle_index = Some(index),
        }
        if let (Some(item), Some(sent)) = (&self.item, &self.sent) {
            self.send(item, self.view().position, sent);
        }
        Effect::default()
    }

    fn leave(&mut self) -> Effect {
        self.airplay.disconnect();
        Effect::default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::chromecast::session::Status;

    fn item(id: &str) -> Item {
        serde_json::from_value(json!({ "Id": id, "Name": id, "Type": "Movie" })).unwrap()
    }

    fn session() -> SessionInfo {
        serde_json::from_value(json!({
            "Id": "s1", "DeviceName": "TV", "Client": "Jellyfin Web", "SupportsRemoteControl": true,
            "NowPlayingItem": { "Id": "item1", "Name": "Film", "Type": "Movie", "RunTimeTicks": 6_000_000_000_i64 },
            "PlayState": { "PositionTicks": 300_000_000_i64, "IsPaused": false, "VolumeLevel": 40, "IsMuted": false },
        }))
        .unwrap()
    }

    /// A new item makes the device change its app: the app before goes,
    /// and its end comes with the number of the item before. That is not
    /// the end of the new item; the end with its own number is. A status
    /// with no app (the launch takes a moment; a key on the remote of the
    /// TV sends one) says nothing of the item.
    #[test]
    fn the_end_of_the_item_before_keeps_the_new_item() {
        let mut loaded = CastLoad::default();
        loaded.sent(item("m2"), 2);
        loaded.note(&CastEvent::Ended { load: 1 });
        assert!(loaded.item.is_some(), "the end of the item before dropped the new item");
        loaded.note(&CastEvent::Status(Status::default()));
        assert!(loaded.item.is_some(), "a status during the launch dropped the item");
        loaded.note(&CastEvent::Ended { load: 2 });
        assert!(loaded.item.is_none(), "the item stays though its app is gone");
    }

    /// A report of the Jellyfin receiver app names the streams it plays;
    /// a new item starts with none known.
    #[test]
    fn a_report_of_the_receiver_names_the_streams() {
        let mut loaded = CastLoad::default();
        loaded.sent(item("m1"), 1);
        let report = json!({ "type": "playbackprogress", "data": { "PlayState": { "AudioStreamIndex": 2, "SubtitleStreamIndex": -1 } } });
        loaded.note(&CastEvent::Message { namespace: cast_jellyfin::NAMESPACE.into(), payload: report });
        assert_eq!(loaded.tracks, CastTracks { audio_index: Some(2), subtitle_index: Some(-1) });
        loaded.sent(item("m2"), 2);
        assert_eq!(loaded.tracks, CastTracks::default());
    }

    /// A control of a Jellyfin session shows in the panel at once, and
    /// leaves the request for the server to the app.
    #[test]
    fn a_jellyfin_session_changes_at_once_and_asks_the_server() {
        let mut remote = JellyfinRemote::new(session());
        let target: &mut dyn RemoteTarget = &mut remote;
        assert!(target.set_paused(true).request.is_some());
        assert!(target.view().paused);
        assert!(target.seek(120.).request.is_some());
        assert_eq!(target.view().position, 120.);
        assert!(target.set_volume(35.4).request.is_some());
        assert_eq!(target.view().volume, Some(35.));
        // Mute is a toggle on the wire: nothing goes out for the state
        // the session is in already.
        assert!(target.set_muted(false).request.is_none());
        assert!(target.set_muted(true).request.is_some());
        assert!(target.view().muted);
        assert!(target.set_track(Track::Subtitle, -1).request.is_some());
        assert_eq!(target.view().subtitle_index, Some(-1));
        assert!(target.skip(true).request.is_some());
        assert_eq!(target.item().map(|item| item.id.as_str()), Some("item1"));
        assert!(target.stop().request.is_some());
        assert!(!target.view().loaded);
        assert!(matches!(target.choice(), Target::JellyfinSession(s) if s.id == "s1"));
        // A play names the device in its toast.
        let play = remote.send_play(vec!["a".into()], PlayCommand::PlayNext, None);
        assert_eq!(play.request.and_then(|r| r.done).as_deref(), Some("Plays next on TV."));
    }

    /// The stream for AirPlay is made with the streams that are chosen:
    /// the audio stream by its index, a subtitle burned in, "off" as none.
    #[test]
    fn the_stream_for_airplay_takes_the_chosen_streams() {
        let client = Client::new("https://jellyfin.example/", "dev-1").with_session("t", "u1");
        let tracks = CastTracks { audio_index: Some(2), subtitle_index: Some(4) };
        let request = send_request(&client, &item("m1"), 30., Some(8_000_000), tracks);
        assert_eq!((request.item_id.as_str(), request.start_secs), ("m1", 30.));
        assert_eq!(request.options.audio_index, Some(2));
        assert_eq!(request.options.subtitle_index, Some(4));
        assert_eq!(request.options.max_bitrate, Some(8_000_000));
        assert!(request.options.hevc);
        let off = CastTracks { audio_index: None, subtitle_index: Some(-1) };
        let request = send_request(&client, &item("m1"), 0., None, off);
        assert_eq!((request.options.audio_index, request.options.subtitle_index), (None, None));
    }
}
