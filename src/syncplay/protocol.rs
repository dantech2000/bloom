// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SyncPlay messages of the server and the requests of the client, as
//! Jellyfin 12 sends and takes them. Times are milliseconds since the Unix
//! epoch on the server clock; positions are ticks of 100 ns.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const TICKS_PER_MS: f64 = 10_000.;

/// A time of the server ("2026-10-03T14:43:25.1234567Z") in milliseconds.
pub fn parse_time(text: &str) -> Option<f64> {
    let time: jiff::Timestamp = text.parse().ok()?;
    Some(time.as_nanosecond() as f64 / 1_000_000.)
}

/// Milliseconds since the Unix epoch as the server reads a time.
pub fn format_time(ms: f64) -> String {
    jiff::Timestamp::from_nanosecond((ms * 1_000_000.) as i128)
        .map(|time| time.to_string())
        .unwrap_or_default()
}

mod time {
    use serde::{Deserialize, Deserializer, de::Error};

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        let text = String::deserialize(deserializer)?;
        super::parse_time(&text).ok_or_else(|| D::Error::custom(format!("not a time: {text}")))
    }
}

/// Group ids compare without regard to hyphens and case: the server writes
/// them without hyphens in most places and with them in a few.
pub fn same_id(a: &str, b: &str) -> bool {
    let plain = |id: &str| -> String {
        id.chars()
            .filter(|c| *c != '-')
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    plain(a) == plain(b)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum CommandKind {
    Unpause,
    Pause,
    Seek,
    Stop,
}

/// "At the time `when`, the position is `position_ticks`."
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Command {
    pub group_id: String,
    /// All zeros when nothing plays.
    #[serde(default)]
    pub playlist_item_id: String,
    #[serde(with = "time")]
    pub when: f64,
    #[serde(default)]
    pub position_ticks: Option<i64>,
    pub command: CommandKind,
    #[serde(with = "time")]
    pub emitted_at: f64,
}

impl Command {
    pub fn position_ms(&self) -> f64 {
        self.position_ticks.unwrap_or(0) as f64 / TICKS_PER_MS
    }

    /// Two deliveries of one command; the server sends a command again to a
    /// client that asks for the state.
    pub fn same_as(&self, other: &Command) -> bool {
        self.when == other.when
            && self.position_ticks == other.position_ticks
            && self.command == other.command
            && same_id(&self.playlist_item_id, &other.playlist_item_id)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub enum GroupState {
    #[default]
    Idle,
    Waiting,
    Paused,
    Playing,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct GroupInfo {
    pub group_id: String,
    #[serde(default)]
    pub group_name: String,
    #[serde(default)]
    pub state: GroupState,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(with = "time")]
    pub last_updated_at: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct QueueItem {
    pub item_id: String,
    pub playlist_item_id: String,
}

/// The queue of the group and the item that plays.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct PlayQueue {
    /// Why the server sends it: "NewPlaylist", "SetCurrentItem",
    /// "RemoveItems", "MoveItem", "Queue", "QueueNext", "NextItem",
    /// "PreviousItem", "RepeatMode", "ShuffleMode".
    pub reason: String,
    #[serde(with = "time")]
    pub last_update: f64,
    #[serde(default)]
    pub playlist: Vec<QueueItem>,
    /// -1 when nothing plays.
    #[serde(default)]
    pub playing_item_index: i64,
    #[serde(default)]
    pub start_position_ticks: i64,
    #[serde(default)]
    pub is_playing: bool,
    #[serde(default)]
    pub shuffle_mode: String,
    #[serde(default)]
    pub repeat_mode: String,
}

impl PlayQueue {
    pub fn current(&self) -> Option<&QueueItem> {
        usize::try_from(self.playing_item_index)
            .ok()
            .and_then(|index| self.playlist.get(index))
    }

    /// Whether the group has an item after the one that plays, as the
    /// server counts it: a repeat mode keeps the queue going.
    pub fn has_next(&self) -> bool {
        let index = self.playing_item_index;
        match self.repeat_mode.as_str() {
            "RepeatOne" | "RepeatAll" => !self.playlist.is_empty(),
            _ => index >= 0 && index + 1 < self.playlist.len() as i64,
        }
    }
}

/// News about the group.
#[derive(Clone, Debug, PartialEq)]
pub enum GroupUpdate {
    Joined(GroupInfo),
    Info(GroupInfo),
    UserJoined(String),
    UserLeft(String),
    Left,
    NotInGroup,
    State { state: GroupState, reason: String },
    Queue(PlayQueue),
    /// The server refused something: "GroupDoesNotExist",
    /// "LibraryAccessDenied", "CreateGroupDenied", "JoinGroupDenied",
    /// "SyncPlayIsDisabled".
    Denied(String),
}

/// A message of the socket that SyncPlay reads.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerMessage {
    Command(Command),
    Group { group_id: String, update: GroupUpdate },
}

/// Reads a socket message; `None` for a type SyncPlay does not use or a
/// message it cannot read.
pub fn parse(message_type: &str, data: Value) -> Option<ServerMessage> {
    match message_type {
        "SyncPlayCommand" => serde_json::from_value(data).ok().map(ServerMessage::Command),
        "SyncPlayGroupUpdate" => {
            let group_id = data.get("GroupId")?.as_str()?.to_string();
            let kind = data.get("Type")?.as_str()?.to_string();
            let inner = data.get("Data").cloned().unwrap_or(Value::Null);
            let text = || inner.as_str().unwrap_or_default().to_string();
            let update = match kind.as_str() {
                "GroupJoined" => GroupUpdate::Joined(serde_json::from_value(inner).ok()?),
                "GroupUpdate" => GroupUpdate::Info(serde_json::from_value(inner).ok()?),
                "UserJoined" => GroupUpdate::UserJoined(text()),
                "UserLeft" => GroupUpdate::UserLeft(text()),
                "GroupLeft" => GroupUpdate::Left,
                "NotInGroup" => GroupUpdate::NotInGroup,
                "StateUpdate" => GroupUpdate::State {
                    state: serde_json::from_value(inner.get("State")?.clone()).ok()?,
                    reason: inner.get("Reason")?.as_str()?.to_string(),
                },
                "PlayQueue" => GroupUpdate::Queue(serde_json::from_value(inner).ok()?),
                _ => GroupUpdate::Denied(kind),
            };
            Some(ServerMessage::Group { group_id, update })
        }
        _ => None,
    }
}

/// Where the player is, for `Ready` and `Buffering`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlayerReport {
    /// Server time of the measurement.
    pub when: String,
    pub position_ticks: i64,
    pub is_playing: bool,
    /// The item the player has loaded.
    pub playlist_item_id: String,
}

/// A request to the server. The answer says nothing about the result: that
/// comes over the socket.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    New { name: String },
    Join { group_id: String },
    Leave,
    Unpause,
    Pause,
    Stop,
    Seek { position_ticks: i64 },
    Ready(PlayerReport),
    Buffering(PlayerReport),
    SetIgnoreWait(bool),
    Ping { ms: i64 },
    SetNewQueue { item_ids: Vec<String>, index: usize, start_ticks: i64 },
    SetPlaylistItem { playlist_item_id: String },
    NextItem { playlist_item_id: String },
    PreviousItem { playlist_item_id: String },
    Queue { item_ids: Vec<String>, next: bool },
    RemoveFromPlaylist { playlist_item_ids: Vec<String> },
    MovePlaylistItem { playlist_item_id: String, new_index: usize },
    SetRepeatMode(String),
    SetShuffleMode(String),
}

impl Request {
    pub fn path(&self) -> &'static str {
        match self {
            Request::New { .. } => "/SyncPlay/New",
            Request::Join { .. } => "/SyncPlay/Join",
            Request::Leave => "/SyncPlay/Leave",
            Request::Unpause => "/SyncPlay/Unpause",
            Request::Pause => "/SyncPlay/Pause",
            Request::Stop => "/SyncPlay/Stop",
            Request::Seek { .. } => "/SyncPlay/Seek",
            Request::Ready(_) => "/SyncPlay/Ready",
            Request::Buffering(_) => "/SyncPlay/Buffering",
            Request::SetIgnoreWait(_) => "/SyncPlay/SetIgnoreWait",
            Request::Ping { .. } => "/SyncPlay/Ping",
            Request::SetNewQueue { .. } => "/SyncPlay/SetNewQueue",
            Request::SetPlaylistItem { .. } => "/SyncPlay/SetPlaylistItem",
            Request::NextItem { .. } => "/SyncPlay/NextItem",
            Request::PreviousItem { .. } => "/SyncPlay/PreviousItem",
            Request::Queue { .. } => "/SyncPlay/Queue",
            Request::RemoveFromPlaylist { .. } => "/SyncPlay/RemoveFromPlaylist",
            Request::MovePlaylistItem { .. } => "/SyncPlay/MovePlaylistItem",
            Request::SetRepeatMode(_) => "/SyncPlay/SetRepeatMode",
            Request::SetShuffleMode(_) => "/SyncPlay/SetShuffleMode",
        }
    }

    /// The JSON body; `None` for a request without one.
    pub fn body(&self) -> Option<Value> {
        Some(match self {
            Request::Leave | Request::Unpause | Request::Pause | Request::Stop => return None,
            Request::New { name } => json!({ "GroupName": name }),
            Request::Join { group_id } => json!({ "GroupId": group_id }),
            Request::Seek { position_ticks } => json!({ "PositionTicks": position_ticks }),
            Request::Ready(report) | Request::Buffering(report) => {
                serde_json::to_value(report).ok()?
            }
            Request::SetIgnoreWait(ignore) => json!({ "IgnoreWait": ignore }),
            Request::Ping { ms } => json!({ "Ping": ms }),
            Request::SetNewQueue { item_ids, index, start_ticks } => json!({
                "PlayingQueue": item_ids,
                "PlayingItemPosition": index,
                "StartPositionTicks": start_ticks,
            }),
            Request::SetPlaylistItem { playlist_item_id }
            | Request::NextItem { playlist_item_id }
            | Request::PreviousItem { playlist_item_id } => {
                json!({ "PlaylistItemId": playlist_item_id })
            }
            Request::Queue { item_ids, next } => json!({
                "ItemIds": item_ids,
                "Mode": if *next { "QueueNext" } else { "Queue" },
            }),
            Request::RemoveFromPlaylist { playlist_item_ids } => json!({
                "PlaylistItemIds": playlist_item_ids,
                "ClearPlaylist": false,
                "ClearPlayingItem": false,
            }),
            Request::MovePlaylistItem { playlist_item_id, new_index } => json!({
                "PlaylistItemId": playlist_item_id,
                "NewIndex": new_index,
            }),
            Request::SetRepeatMode(mode) | Request::SetShuffleMode(mode) => {
                json!({ "Mode": mode })
            }
        })
    }

    /// A newer request of the same kind makes this one pointless: only the
    /// last seek counts, and only the last report of the player.
    pub fn replaces(&self, older: &Request) -> bool {
        matches!(
            (self, older),
            (Request::Seek { .. }, Request::Seek { .. })
                | (Request::Ready(_), Request::Ready(_))
                | (Request::Ready(_), Request::Buffering(_))
                | (Request::Buffering(_), Request::Buffering(_))
                | (Request::Ping { .. }, Request::Ping { .. })
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_keep_their_fraction() {
        // Seven digits, as the server writes them, and none.
        let ms = parse_time("2026-10-03T14:43:25.1234567Z").unwrap();
        assert!((ms % 1000. - 123.4567).abs() < 1e-3);
        assert_eq!(parse_time("2026-10-03T14:43:25Z").unwrap() % 1000., 0.);
        assert!(parse_time("yesterday").is_none());
        let back = parse_time(&format_time(ms)).unwrap();
        assert!((back - ms).abs() < 1e-3);
    }

    #[test]
    fn reads_a_command() {
        let data = json!({
            "GroupId": "0a1b2c3d4e5f60718293a4b5c6d7e8f9",
            "PlaylistItemId": "11111111222233334444555555555555",
            "When": "2026-10-03T14:43:25.5000000Z",
            "PositionTicks": 12_345_678_i64,
            "Command": "Unpause",
            "EmittedAt": "2026-10-03T14:43:24.9000000Z",
        });
        let Some(ServerMessage::Command(command)) = parse("SyncPlayCommand", data) else {
            panic!("no command");
        };
        assert_eq!(command.command, CommandKind::Unpause);
        assert!((command.position_ms() - 1234.5678).abs() < 1e-6);
        assert!((command.when - command.emitted_at - 600.).abs() < 1e-3);
        assert!(command.same_as(&command.clone()));
    }

    #[test]
    fn reads_the_group_updates() {
        let group = |kind: &str, data: Value| {
            let message = json!({ "GroupId": "abc", "Type": kind, "Data": data });
            match parse("SyncPlayGroupUpdate", message) {
                Some(ServerMessage::Group { update, .. }) => update,
                other => panic!("{kind}: {other:?}"),
            }
        };
        let info = json!({
            "GroupId": "abc", "GroupName": "Film night", "State": "Paused",
            "Participants": ["ana", "ben"], "LastUpdatedAt": "2026-10-03T14:00:00Z",
        });
        let GroupUpdate::Joined(joined) = group("GroupJoined", info) else {
            panic!("not joined");
        };
        assert_eq!(joined.state, GroupState::Paused);
        assert_eq!(joined.participants, ["ana", "ben"]);
        assert_eq!(group("UserJoined", json!("cy")), GroupUpdate::UserJoined("cy".into()));
        assert_eq!(group("GroupLeft", json!("abc")), GroupUpdate::Left);
        assert_eq!(group("NotInGroup", json!("")), GroupUpdate::NotInGroup);
        assert_eq!(
            group("StateUpdate", json!({ "State": "Waiting", "Reason": "Buffer" })),
            GroupUpdate::State { state: GroupState::Waiting, reason: "Buffer".into() }
        );
        assert_eq!(
            group("LibraryAccessDenied", json!("")),
            GroupUpdate::Denied("LibraryAccessDenied".into())
        );
        let queue = json!({
            "Reason": "NewPlaylist", "LastUpdate": "2026-10-03T14:00:01Z",
            "Playlist": [{ "ItemId": "i1", "PlaylistItemId": "p1" }, { "ItemId": "i2", "PlaylistItemId": "p2" }],
            "PlayingItemIndex": 1, "StartPositionTicks": 50, "IsPlaying": true,
            "ShuffleMode": "Sorted", "RepeatMode": "RepeatNone",
        });
        let GroupUpdate::Queue(queue) = group("PlayQueue", queue) else {
            panic!("no queue");
        };
        assert_eq!(queue.current().unwrap().playlist_item_id, "p2");
        assert!(parse("LibraryChanged", json!({})).is_none());
    }

    #[test]
    fn nothing_plays_at_index_minus_one() {
        let queue: PlayQueue = serde_json::from_value(json!({
            "Reason": "RemoveItems", "LastUpdate": "2026-10-03T14:00:01Z",
            "Playlist": [], "PlayingItemIndex": -1,
        }))
        .unwrap();
        assert!(queue.current().is_none());
    }

    #[test]
    fn requests_have_the_bodies_the_server_reads() {
        let report = PlayerReport {
            when: format_time(1_000.),
            position_ticks: 70,
            is_playing: false,
            playlist_item_id: "p1".into(),
        };
        assert_eq!(
            Request::Ready(report.clone()).body().unwrap(),
            json!({ "When": "1970-01-01T00:00:01Z", "PositionTicks": 70, "IsPlaying": false, "PlaylistItemId": "p1" })
        );
        assert_eq!(Request::Pause.body(), None);
        assert_eq!(Request::Pause.path(), "/SyncPlay/Pause");
        assert_eq!(
            Request::SetNewQueue { item_ids: vec!["a".into()], index: 0, start_ticks: 5 }.body().unwrap(),
            json!({ "PlayingQueue": ["a"], "PlayingItemPosition": 0, "StartPositionTicks": 5 })
        );
        assert_eq!(
            Request::Queue { item_ids: vec!["a".into()], next: true }.body().unwrap()["Mode"],
            "QueueNext"
        );
        // A later seek replaces a waiting one; a pause replaces nothing.
        assert!(Request::Seek { position_ticks: 2 }.replaces(&Request::Seek { position_ticks: 1 }));
        assert!(Request::Ready(report.clone()).replaces(&Request::Buffering(report)));
        assert!(!Request::Pause.replaces(&Request::Seek { position_ticks: 1 }));
    }

    #[test]
    fn ids_compare_with_and_without_hyphens() {
        assert!(same_id(
            "0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9",
            "0a1b2c3d4e5f60718293a4b5c6d7e8f9"
        ));
        assert!(!same_id("a", "b"));
    }
}
