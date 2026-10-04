// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shapes of remote control on the wire, both ways: the sessions the
//! server lists, the commands it sends to a session over the socket, and
//! the bodies this app sends. No part of the app is in here, so a test can
//! check every shape alone.
//!
//! Checked against the server (`Jellyfin.Api/Controllers/SessionController.cs`,
//! `Emby.Server.Implementations/Session/SessionManager.cs`) and the web
//! client (`src/plugins/sessionPlayer/plugin.js`).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::jellyfin::{Item, TICKS_PER_SECOND};

/// One session of the server, as `GET /Sessions` and the `Sessions`
/// socket message give it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct SessionInfo {
    pub id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub application_version: String,
    /// The session can take commands: it posted capabilities with media
    /// control and it has a socket open.
    #[serde(default)]
    pub supports_remote_control: bool,
    #[serde(default)]
    pub supports_media_control: bool,
    #[serde(default)]
    pub now_playing_item: Option<Item>,
    #[serde(default)]
    pub play_state: PlayerState,
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
}

/// What a session reports about its playback.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct PlayerState {
    #[serde(default)]
    pub position_ticks: Option<i64>,
    #[serde(default)]
    pub can_seek: bool,
    #[serde(default)]
    pub is_paused: bool,
    #[serde(default)]
    pub is_muted: bool,
    #[serde(default)]
    pub volume_level: Option<i64>,
    #[serde(default)]
    pub audio_stream_index: Option<i64>,
    #[serde(default)]
    pub subtitle_stream_index: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Capabilities {
    #[serde(default)]
    pub supported_commands: Vec<String>,
    #[serde(default)]
    pub supports_media_control: bool,
}

impl SessionInfo {
    /// What the session plays, for a list.
    pub fn now_playing_title(&self) -> Option<String> {
        self.now_playing_item.as_ref().map(Item::display_title)
    }

    pub fn position_secs(&self) -> f64 {
        self.play_state.position_ticks.unwrap_or(0) as f64 / TICKS_PER_SECOND as f64
    }

    pub fn duration_secs(&self) -> f64 {
        self.now_playing_item
            .as_ref()
            .and_then(Item::runtime_secs)
            .unwrap_or(0) as f64
    }
}

/// The sessions of the user this app may control: not this device, and
/// only the ones that take commands.
pub fn controllable(sessions: Vec<SessionInfo>, own_device_id: &str) -> Vec<SessionInfo> {
    let mut sessions: Vec<SessionInfo> = sessions
        .into_iter()
        .filter(|s| s.supports_remote_control && s.device_id != own_device_id)
        .collect();
    sessions.sort_by(|a, b| {
        a.device_name
            .to_lowercase()
            .cmp(&b.device_name.to_lowercase())
            .then_with(|| a.client.cmp(&b.client))
    });
    sessions
}

// ----- what this app sends, as a controller ------------------------------

/// How items go to a session: now, after the item that plays, or at the
/// end of its queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayCommand {
    PlayNow,
    PlayNext,
    PlayLast,
}

impl PlayCommand {
    pub fn name(self) -> &'static str {
        match self {
            PlayCommand::PlayNow => "PlayNow",
            PlayCommand::PlayNext => "PlayNext",
            PlayCommand::PlayLast => "PlayLast",
        }
    }

    fn parse(name: &str) -> Self {
        match name {
            "PlayNext" => PlayCommand::PlayNext,
            "PlayLast" => PlayCommand::PlayLast,
            // PlayInstantMix and PlayShuffle come as PlayNow: the server
            // builds the list itself.
            _ => PlayCommand::PlayNow,
        }
    }
}

/// Query of `POST /Sessions/{id}/Playing`. The ids go in one value with
/// commas between them (`CommaDelimitedCollectionModelBinder`).
pub fn play_query(
    item_ids: &[String],
    command: PlayCommand,
    start_secs: Option<f64>,
) -> Vec<(&'static str, String)> {
    let mut query = vec![
        ("playCommand", command.name().to_string()),
        ("itemIds", item_ids.join(",")),
    ];
    if let Some(secs) = start_secs.filter(|s| *s > 0.) {
        query.push(("startPositionTicks", secs_to_ticks(secs).to_string()));
    }
    query
}

/// Body of `POST /Sessions/{id}/Command`: a `GeneralCommand`. The server
/// sets `ControllingUserId` itself; argument values are strings.
pub fn general_command(name: &str, arguments: &[(&str, String)]) -> Value {
    let mut args = serde_json::Map::new();
    for (key, value) in arguments {
        args.insert((*key).to_string(), Value::String(value.clone()));
    }
    json!({ "Name": name, "Arguments": args })
}

pub fn secs_to_ticks(secs: f64) -> i64 {
    (secs.max(0.) * TICKS_PER_SECOND as f64).round() as i64
}

// ----- what this app posts about itself, as a target ---------------------

/// The general commands this app carries out. The server tells a
/// controller these; the rest it does not send.
pub const SUPPORTED_COMMANDS: &[&str] = &[
    "VolumeUp",
    "VolumeDown",
    "Mute",
    "Unmute",
    "ToggleMute",
    "SetVolume",
    "SetAudioStreamIndex",
    "SetSubtitleStreamIndex",
    "DisplayMessage",
    "ToggleFullscreen",
];

/// Body of `POST /Sessions/Capabilities/Full` (`ClientCapabilitiesDto`).
/// With `control` off the session takes no commands.
pub fn capabilities(control: bool) -> Value {
    json!({
        "PlayableMediaTypes": ["Video"],
        "SupportedCommands": if control { SUPPORTED_COMMANDS.to_vec() } else { Vec::new() },
        "SupportsMediaControl": control,
        "SupportsPersistentIdentifier": true,
    })
}

// ----- what the server sends to this app over the socket -----------------

/// A command another device sent to this one.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteCommand {
    /// `Play`: items to play, with where to start.
    Play {
        item_ids: Vec<String>,
        command: PlayCommand,
        start_secs: Option<f64>,
        start_index: Option<usize>,
        audio_index: Option<i64>,
        subtitle_index: Option<i64>,
    },
    /// `Playstate`: Stop, Pause, Unpause, PlayPause, NextTrack,
    /// PreviousTrack, Rewind, FastForward.
    Playstate(String),
    /// `Playstate` Seek, in seconds.
    Seek(f64),
    SetVolume(f32),
    VolumeUp,
    VolumeDown,
    Mute,
    Unmute,
    ToggleMute,
    /// The index of a stream of the item; -1 turns subtitles off.
    SetAudioStreamIndex(i64),
    SetSubtitleStreamIndex(i64),
    DisplayMessage { header: String, text: String },
    ToggleFullscreen,
    /// A general command this app does not do.
    Other(String),
}

/// Reads a socket message. None when it is not a remote command.
pub fn parse(kind: &str, data: &Value) -> Option<RemoteCommand> {
    match kind {
        "Play" => {
            let item_ids = data["ItemIds"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .map(|id| id.replace('-', ""))
                        .collect()
                })
                .unwrap_or_default();
            Some(RemoteCommand::Play {
                item_ids,
                command: PlayCommand::parse(data["PlayCommand"].as_str().unwrap_or("PlayNow")),
                start_secs: data["StartPositionTicks"]
                    .as_i64()
                    .map(|ticks| ticks as f64 / TICKS_PER_SECOND as f64),
                start_index: data["StartIndex"].as_u64().map(|n| n as usize),
                audio_index: data["AudioStreamIndex"].as_i64(),
                subtitle_index: data["SubtitleStreamIndex"].as_i64(),
            })
        }
        "Playstate" => {
            let command = data["Command"].as_str().unwrap_or_default();
            if command == "Seek" {
                let ticks = data["SeekPositionTicks"].as_i64().unwrap_or(0);
                return Some(RemoteCommand::Seek(ticks as f64 / TICKS_PER_SECOND as f64));
            }
            Some(RemoteCommand::Playstate(command.to_string()))
        }
        "GeneralCommand" => {
            let name = data["Name"].as_str().unwrap_or_default();
            let args = &data["Arguments"];
            // The web client sends numbers as strings; others may not.
            let number = |key: &str| -> Option<f64> {
                let value = &args[key];
                value
                    .as_f64()
                    .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
            };
            let text = |key: &str| args[key].as_str().unwrap_or_default().to_string();
            Some(match name {
                "SetVolume" => RemoteCommand::SetVolume(number("Volume")?.clamp(0., 100.) as f32),
                "VolumeUp" => RemoteCommand::VolumeUp,
                "VolumeDown" => RemoteCommand::VolumeDown,
                "Mute" => RemoteCommand::Mute,
                "Unmute" => RemoteCommand::Unmute,
                "ToggleMute" => RemoteCommand::ToggleMute,
                "SetAudioStreamIndex" => RemoteCommand::SetAudioStreamIndex(number("Index")? as i64),
                "SetSubtitleStreamIndex" => {
                    RemoteCommand::SetSubtitleStreamIndex(number("Index")? as i64)
                }
                "DisplayMessage" => RemoteCommand::DisplayMessage {
                    header: text("Header"),
                    text: text("Text"),
                },
                "ToggleFullscreen" => RemoteCommand::ToggleFullscreen,
                other => RemoteCommand::Other(other.to_string()),
            })
        }
        _ => None,
    }
}

/// What the app does for a command: one call of the playback facade, so a
/// pause in a SyncPlay group is a request to the group, as a click is.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    TogglePause,
    SeekTo(f64),
    SeekBy(f64),
    Next,
    Previous,
    Stop,
    SetVolume(f32),
    SetMuted(bool),
    ToggleMute,
    SetAudioStream(i64),
    SetSubtitleStream(i64),
    Toast { header: String, text: String },
    ToggleFullscreen,
    /// Nothing to do: the player is already in that state, or the command
    /// is not one this app does.
    Nothing,
}

/// The state of this player a command may depend on.
#[derive(Clone, Copy, Debug, Default)]
pub struct Local {
    pub player_open: bool,
    pub paused: bool,
    pub volume: f32,
    pub muted: bool,
}

/// Maps a command to the action of the facade. `Play` is not here: it
/// needs the items first.
pub fn plan(command: &RemoteCommand, local: Local) -> Action {
    let with_player = |action: Action| if local.player_open { action } else { Action::Nothing };
    match command {
        RemoteCommand::Play { .. } => Action::Nothing,
        RemoteCommand::Playstate(name) => match name.as_str() {
            "Stop" => with_player(Action::Stop),
            "Pause" if !local.paused => with_player(Action::TogglePause),
            "Unpause" if local.paused => with_player(Action::TogglePause),
            "PlayPause" => with_player(Action::TogglePause),
            "NextTrack" => with_player(Action::Next),
            "PreviousTrack" => with_player(Action::Previous),
            "Rewind" => with_player(Action::SeekBy(-10.)),
            "FastForward" => with_player(Action::SeekBy(30.)),
            _ => Action::Nothing,
        },
        RemoteCommand::Seek(secs) => with_player(Action::SeekTo(*secs)),
        RemoteCommand::SetVolume(volume) => Action::SetVolume(*volume),
        RemoteCommand::VolumeUp => Action::SetVolume((local.volume + 5.).min(100.)),
        RemoteCommand::VolumeDown => Action::SetVolume((local.volume - 5.).max(0.)),
        RemoteCommand::Mute if !local.muted => Action::SetMuted(true),
        RemoteCommand::Unmute if local.muted => Action::SetMuted(false),
        RemoteCommand::Mute | RemoteCommand::Unmute => Action::Nothing,
        RemoteCommand::ToggleMute => Action::ToggleMute,
        RemoteCommand::SetAudioStreamIndex(index) => with_player(Action::SetAudioStream(*index)),
        RemoteCommand::SetSubtitleStreamIndex(index) => {
            with_player(Action::SetSubtitleStream(*index))
        }
        RemoteCommand::DisplayMessage { header, text } => Action::Toast {
            header: if header.is_empty() { "Message".to_string() } else { header.clone() },
            text: text.clone(),
        },
        RemoteCommand::ToggleFullscreen => Action::ToggleFullscreen,
        RemoteCommand::Other(_) => Action::Nothing,
    }
}

/// The track of mpv for a stream index of the item. mpv numbers the
/// tracks of a kind from 1 in file order; the server numbers every
/// stream of the source in one list. Subtitles the server keeps in files
/// of their own are not in mpv's list, so they get none.
pub fn mpv_track(item: &Item, kind: &str, stream_index: i64) -> Option<i64> {
    item.streams(kind)
        .iter()
        .position(|stream| stream.index == stream_index)
        .map(|place| place as i64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(json: Value) -> SessionInfo {
        serde_json::from_value(json).expect("session")
    }

    #[test]
    fn reads_a_session_with_its_play_state() {
        let s = session(json!({
            "Id": "abc", "UserId": "u1", "UserName": "jellyui-test-user",
            "Client": "jellyui", "DeviceName": "mac", "DeviceId": "dev-2",
            "SupportsRemoteControl": true, "SupportsMediaControl": true,
            "NowPlayingItem": { "Id": "item1", "Name": "Film", "Type": "Movie", "RunTimeTicks": 600_000_000_i64 },
            "PlayState": { "PositionTicks": 150_000_000_i64, "IsPaused": true, "VolumeLevel": 40, "IsMuted": false, "AudioStreamIndex": 1 },
            "Capabilities": { "SupportedCommands": ["SetVolume"], "SupportsMediaControl": true }
        }));
        assert_eq!(s.now_playing_title().as_deref(), Some("Film"));
        assert_eq!(s.position_secs(), 15.);
        assert_eq!(s.duration_secs(), 60.);
        assert!(s.play_state.is_paused);
        assert_eq!(s.play_state.volume_level, Some(40));
        assert_eq!(s.play_state.audio_stream_index, Some(1));
        assert!(s.capabilities.unwrap().supports_media_control);
        // A session with nothing playing still reads.
        let idle = session(json!({ "Id": "x", "PlayState": {} }));
        assert_eq!(idle.now_playing_title(), None);
        assert_eq!(idle.position_secs(), 0.);
    }

    #[test]
    fn keeps_the_sessions_that_take_commands_and_not_this_device() {
        let list = vec![
            session(json!({ "Id": "1", "DeviceId": "me", "DeviceName": "This", "SupportsRemoteControl": true })),
            session(json!({ "Id": "2", "DeviceId": "b", "DeviceName": "Zed", "SupportsRemoteControl": true })),
            session(json!({ "Id": "3", "DeviceId": "c", "DeviceName": "Web", "SupportsRemoteControl": false })),
            session(json!({ "Id": "4", "DeviceId": "d", "DeviceName": "apple", "SupportsRemoteControl": true })),
        ];
        let ids: Vec<String> = controllable(list, "me").into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["4", "2"]);
    }

    #[test]
    fn builds_the_play_query() {
        let ids = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            play_query(&ids, PlayCommand::PlayNow, Some(12.5)),
            vec![
                ("playCommand", "PlayNow".to_string()),
                ("itemIds", "a,b".to_string()),
                ("startPositionTicks", "125000000".to_string()),
            ]
        );
        // No start position when the item starts at its beginning.
        assert_eq!(play_query(&ids, PlayCommand::PlayLast, Some(0.)).len(), 2);
        assert_eq!(play_query(&ids, PlayCommand::PlayNext, None)[0].1, "PlayNext");
    }

    #[test]
    fn builds_general_commands_and_capabilities() {
        let body = general_command("SetVolume", &[("Volume", "35".to_string())]);
        assert_eq!(body, json!({ "Name": "SetVolume", "Arguments": { "Volume": "35" } }));
        assert_eq!(general_command("ToggleMute", &[]), json!({ "Name": "ToggleMute", "Arguments": {} }));
        let caps = capabilities(true);
        assert_eq!(caps["PlayableMediaTypes"], json!(["Video"]));
        assert_eq!(caps["SupportsMediaControl"], json!(true));
        assert!(caps["SupportedCommands"].as_array().unwrap().iter().any(|c| c == "SetVolume"));
        let off = capabilities(false);
        assert_eq!(off["SupportsMediaControl"], json!(false));
        assert_eq!(off["SupportedCommands"], json!([]));
    }

    #[test]
    fn parses_play_messages() {
        let data = json!({
            "ItemIds": ["0123456789abcdef0123456789abcdef", "fedcba98-7654-3210-fedc-ba9876543210"],
            "PlayCommand": "PlayNext", "StartPositionTicks": 50_000_000_i64,
            "StartIndex": 1, "AudioStreamIndex": 2, "SubtitleStreamIndex": -1,
            "ControllingUserId": "u"
        });
        assert_eq!(
            parse("Play", &data),
            Some(RemoteCommand::Play {
                item_ids: vec![
                    "0123456789abcdef0123456789abcdef".into(),
                    "fedcba9876543210fedcba9876543210".into()
                ],
                command: PlayCommand::PlayNext,
                start_secs: Some(5.),
                start_index: Some(1),
                audio_index: Some(2),
                subtitle_index: Some(-1),
            })
        );
        // The least a Play carries.
        assert_eq!(
            parse("Play", &json!({ "ItemIds": ["a"], "PlayCommand": "PlayNow" })),
            Some(RemoteCommand::Play {
                item_ids: vec!["a".into()],
                command: PlayCommand::PlayNow,
                start_secs: None,
                start_index: None,
                audio_index: None,
                subtitle_index: None,
            })
        );
        // A shuffle comes as PlayNow with the list made by the server.
        let Some(RemoteCommand::Play { command, .. }) =
            parse("Play", &json!({ "ItemIds": [], "PlayCommand": "PlayShuffle" }))
        else {
            panic!("no play");
        };
        assert_eq!(command, PlayCommand::PlayNow);
    }

    #[test]
    fn parses_playstate_and_general_commands() {
        assert_eq!(
            parse("Playstate", &json!({ "Command": "Pause", "ControllingUserId": "u" })),
            Some(RemoteCommand::Playstate("Pause".into()))
        );
        assert_eq!(
            parse("Playstate", &json!({ "Command": "Seek", "SeekPositionTicks": 300_000_000_i64 })),
            Some(RemoteCommand::Seek(30.))
        );
        let general = |name: &str, args: Value| {
            parse("GeneralCommand", &json!({ "Name": name, "Arguments": args, "ControllingUserId": "u" }))
        };
        assert_eq!(general("SetVolume", json!({ "Volume": "35" })), Some(RemoteCommand::SetVolume(35.)));
        assert_eq!(general("SetVolume", json!({ "Volume": 80 })), Some(RemoteCommand::SetVolume(80.)));
        assert_eq!(general("SetVolume", json!({ "Volume": "250" })), Some(RemoteCommand::SetVolume(100.)));
        assert_eq!(general("SetVolume", json!({})), None);
        assert_eq!(general("ToggleMute", json!({})), Some(RemoteCommand::ToggleMute));
        assert_eq!(general("Mute", Value::Null), Some(RemoteCommand::Mute));
        assert_eq!(
            general("SetAudioStreamIndex", json!({ "Index": "2" })),
            Some(RemoteCommand::SetAudioStreamIndex(2))
        );
        assert_eq!(
            general("SetSubtitleStreamIndex", json!({ "Index": -1 })),
            Some(RemoteCommand::SetSubtitleStreamIndex(-1))
        );
        assert_eq!(
            general("DisplayMessage", json!({ "Header": "Hi", "Text": "There" })),
            Some(RemoteCommand::DisplayMessage { header: "Hi".into(), text: "There".into() })
        );
        assert_eq!(general("ToggleFullscreen", json!({})), Some(RemoteCommand::ToggleFullscreen));
        assert_eq!(general("GoHome", json!({})), Some(RemoteCommand::Other("GoHome".into())));
        // Messages of other kinds are not commands.
        assert_eq!(parse("Sessions", &json!([])), None);
        assert_eq!(parse("LibraryChanged", &json!({})), None);
    }

    #[test]
    fn maps_commands_to_the_facade() {
        let playing = Local { player_open: true, paused: false, volume: 50., muted: false };
        let paused = Local { paused: true, ..playing };
        let closed = Local { player_open: false, ..playing };
        let state = |name: &str| RemoteCommand::Playstate(name.into());
        // Pause and Unpause are one toggle of the facade, and only when
        // they change something.
        assert_eq!(plan(&state("Pause"), playing), Action::TogglePause);
        assert_eq!(plan(&state("Pause"), paused), Action::Nothing);
        assert_eq!(plan(&state("Unpause"), paused), Action::TogglePause);
        assert_eq!(plan(&state("Unpause"), playing), Action::Nothing);
        assert_eq!(plan(&state("PlayPause"), playing), Action::TogglePause);
        assert_eq!(plan(&state("Stop"), playing), Action::Stop);
        assert_eq!(plan(&state("NextTrack"), playing), Action::Next);
        assert_eq!(plan(&state("PreviousTrack"), playing), Action::Previous);
        assert_eq!(plan(&state("Rewind"), playing), Action::SeekBy(-10.));
        assert_eq!(plan(&state("FastForward"), playing), Action::SeekBy(30.));
        assert_eq!(plan(&RemoteCommand::Seek(42.), playing), Action::SeekTo(42.));
        // With no player open, playback commands do nothing.
        assert_eq!(plan(&state("Pause"), closed), Action::Nothing);
        assert_eq!(plan(&state("Stop"), closed), Action::Nothing);
        assert_eq!(plan(&RemoteCommand::Seek(1.), closed), Action::Nothing);
        // Volume and mute work without a player.
        assert_eq!(plan(&RemoteCommand::SetVolume(20.), closed), Action::SetVolume(20.));
        assert_eq!(plan(&RemoteCommand::VolumeUp, playing), Action::SetVolume(55.));
        assert_eq!(plan(&RemoteCommand::VolumeDown, Local { volume: 3., ..playing }), Action::SetVolume(0.));
        assert_eq!(plan(&RemoteCommand::Mute, playing), Action::SetMuted(true));
        assert_eq!(plan(&RemoteCommand::Mute, Local { muted: true, ..playing }), Action::Nothing);
        assert_eq!(plan(&RemoteCommand::Unmute, Local { muted: true, ..playing }), Action::SetMuted(false));
        assert_eq!(plan(&RemoteCommand::Unmute, playing), Action::Nothing);
        assert_eq!(plan(&RemoteCommand::ToggleMute, playing), Action::ToggleMute);
        assert_eq!(plan(&RemoteCommand::SetAudioStreamIndex(2), playing), Action::SetAudioStream(2));
        assert_eq!(plan(&RemoteCommand::SetSubtitleStreamIndex(-1), playing), Action::SetSubtitleStream(-1));
        assert_eq!(
            plan(&RemoteCommand::DisplayMessage { header: String::new(), text: "Hi".into() }, closed),
            Action::Toast { header: "Message".into(), text: "Hi".into() }
        );
        assert_eq!(plan(&RemoteCommand::ToggleFullscreen, closed), Action::ToggleFullscreen);
        assert_eq!(plan(&RemoteCommand::Other("GoHome".into()), playing), Action::Nothing);
    }

    #[test]
    fn finds_the_mpv_track_of_a_stream() {
        let item: Item = serde_json::from_value(json!({
            "Id": "i", "Name": "Film", "Type": "Movie",
            "MediaSources": [{ "MediaStreams": [
                { "Type": "Video", "Index": 0 },
                { "Type": "Audio", "Index": 1 },
                { "Type": "Audio", "Index": 2 },
                { "Type": "Subtitle", "Index": 3 },
                { "Type": "Subtitle", "Index": 4 }
            ]}]
        }))
        .unwrap();
        assert_eq!(mpv_track(&item, "Audio", 1), Some(1));
        assert_eq!(mpv_track(&item, "Audio", 2), Some(2));
        assert_eq!(mpv_track(&item, "Subtitle", 4), Some(2));
        assert_eq!(mpv_track(&item, "Subtitle", 1), None);
        assert_eq!(mpv_track(&item, "Audio", 9), None);
    }
}
