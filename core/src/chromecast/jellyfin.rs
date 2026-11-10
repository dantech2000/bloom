// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two ways to play a Jellyfin item on a cast device.
//!
//! 1. The Jellyfin receiver app (github.com/jellyfin/jellyfin-chromecast).
//!    The sender tells it the server, the user, the token and the items on
//!    the namespace `urn:x-cast:com.connectsdk`; the app asks the server
//!    for the streams itself and reports the playback. This is what
//!    jellyfin-web does (`src/plugins/chromecastPlayer/plugin.js`), and the
//!    messages here copy its fields. The app id comes from the user's
//!    configuration on the server (`CastReceiverId`); the server lists
//!    "F007D354" (Stable) and "6F511C87" (Unstable).
//! 2. The Default Media Receiver of Google with a stream URL of the
//!    server. The device fetches the stream itself, so the token must be in
//!    the URL. The profile is the one every cast device plays: H.264 and
//!    AAC in HLS.

use serde_json::{Value, json};

use super::{
    messages::LoadMedia,
    session::{JellyfinLoad, LoadRequest},
};
use crate::jellyfin::{Client, Item, ItemKind, TICKS_PER_SECOND};

pub const APP_STABLE: &str = "F007D354";
pub const APP_UNSTABLE: &str = "6F511C87";
pub const NAMESPACE: &str = "urn:x-cast:com.connectsdk";

/// The receiver app ids, in the order to try them.
pub fn app_ids() -> Vec<String> {
    vec![APP_STABLE.into(), super::messages::APP_DEFAULT_MEDIA.into()]
}

pub fn is_jellyfin_app(app_id: &str) -> bool {
    app_id == APP_STABLE || app_id == APP_UNSTABLE
}

/// Who the receiver app speaks to the server as.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub server_address: String,
    pub server_id: String,
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
}

impl Identity {
    /// From the session of the app; `None` before sign-in.
    pub fn from_client(client: &Client, server_id: &str) -> Option<Self> {
        Some(Self {
            server_address: client.base.to_string(),
            server_id: server_id.to_string(),
            user_id: client.user_id.as_deref()?.to_string(),
            device_id: client.device_id.to_string(),
            access_token: client.token.as_deref()?.to_string(),
        })
    }
}

/// The small form of an item the receiver app takes.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemStub {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub media_type: String,
    pub is_folder: bool,
}

impl ItemStub {
    pub fn from_item(item: &Item) -> Self {
        Self {
            id: item.id.clone(),
            name: item.name.clone(),
            kind: item.kind.to_string(),
            media_type: if item.kind == ItemKind::Audio { "Audio" } else { "Video" }.to_string(),
            is_folder: matches!(item.kind, ItemKind::Series | ItemKind::Season | ItemKind::Folder | ItemKind::BoxSet | ItemKind::Playlist | ItemKind::MusicAlbum),
        }
    }

    fn json(&self, server_id: &str) -> Value {
        json!({
            "Id": self.id, "ServerId": server_id, "Name": self.name, "Type": self.kind,
            "MediaType": self.media_type, "IsFolder": self.is_folder,
        })
    }
}

/// A message for the receiver app: the command, its options and the
/// identity jellyfin-web adds to every message. The app does not know the
/// version of the server, and the receiver plays without it: the field
/// goes empty, as it always has.
pub fn message(identity: &Identity, receiver_name: &str, command: &str, options: Value) -> Value {
    json!({
        "command": command,
        "options": options,
        "userId": identity.user_id,
        "deviceId": identity.device_id,
        "accessToken": identity.access_token,
        "serverAddress": identity.server_address,
        "serverId": identity.server_id,
        "serverVersion": "",
        "receiverName": receiver_name,
    })
}

/// Plays the items from the start position; the receiver asks the server
/// for the streams.
pub fn play_now(
    identity: &Identity,
    receiver_name: &str,
    items: &[ItemStub],
    start_secs: f64,
    audio_index: Option<i64>,
    subtitle_index: Option<i64>,
) -> Value {
    let mut options = json!({
        "items": items.iter().map(|item| item.json(&identity.server_id)).collect::<Vec<_>>(),
        "startPositionTicks": (start_secs.max(0.) * TICKS_PER_SECOND as f64) as i64,
    });
    if let Some(index) = audio_index {
        options["audioStreamIndex"] = json!(index);
    }
    if let Some(index) = subtitle_index {
        options["subtitleStreamIndex"] = json!(index);
    }
    let mut message = message(identity, receiver_name, "PlayNow", options);
    // The receiver reads these only with items; jellyfin-web sends them so.
    message["subtitleBurnIn"] = json!("");
    message
}

pub fn identify(identity: &Identity, receiver_name: &str) -> Value {
    message(identity, receiver_name, "Identify", json!({}))
}

pub fn pause(identity: &Identity, receiver_name: &str) -> Value {
    message(identity, receiver_name, "Pause", json!({}))
}

pub fn unpause(identity: &Identity, receiver_name: &str) -> Value {
    message(identity, receiver_name, "Unpause", json!({}))
}

pub fn stop(identity: &Identity, receiver_name: &str) -> Value {
    message(identity, receiver_name, "Stop", json!({}))
}

/// The position in seconds; the receiver turns it into ticks.
pub fn seek(identity: &Identity, receiver_name: &str, secs: f64) -> Value {
    message(identity, receiver_name, "Seek", json!({ "position": secs.max(0.) }))
}

/// The index of a stream of the item; -1 turns subtitles off.
pub fn set_subtitle(identity: &Identity, receiver_name: &str, index: i64) -> Value {
    message(identity, receiver_name, "SetSubtitleStreamIndex", json!({ "index": index }))
}

pub fn set_audio(identity: &Identity, receiver_name: &str, index: i64) -> Value {
    message(identity, receiver_name, "SetAudioStreamIndex", json!({ "index": index }))
}

/// What the receiver app reports back: `playbackstart`, `playbackprogress`,
/// `playbackstop`, with the play state of the session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub kind: String,
    pub position: f64,
    pub paused: bool,
    pub duration: Option<f64>,
    pub item_id: Option<String>,
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
}

pub fn parse_report(payload: &Value) -> Option<Report> {
    let kind = payload["type"].as_str()?;
    if !kind.starts_with("playback") && kind != "volumechange" {
        return None;
    }
    let data = &payload["data"];
    let state = &data["PlayState"];
    let ticks = |value: &Value| value.as_f64().map(|t| t / TICKS_PER_SECOND as f64);
    Some(Report {
        kind: kind.to_string(),
        position: ticks(&state["PositionTicks"]).unwrap_or(0.),
        paused: state["IsPaused"].as_bool().unwrap_or(false),
        duration: ticks(&data["NowPlayingItem"]["RunTimeTicks"]),
        item_id: data["NowPlayingItem"]["Id"]
            .as_str()
            .or_else(|| data["ItemId"].as_str())
            .map(str::to_string),
        audio_index: state["AudioStreamIndex"].as_i64(),
        subtitle_index: state["SubtitleStreamIndex"].as_i64(),
    })
}

/// The HLS stream of an item for the Default Media Receiver: H.264 up to
/// 1080p and stereo AAC, which every cast device plays. The server
/// transcodes only what the file is not already. The token is in the
/// query, as the device fetches the stream itself. The stream is the
/// whole item: the place to start at goes in the load (`LoadMedia`), so
/// the positions the device reports are positions in the item.
pub fn stream_url(client: &Client, item_id: &str, play_session_id: &str) -> Option<String> {
    let token = client.token.as_deref()?;
    Some(format!(
        "{}/Videos/{item_id}/master.m3u8?MediaSourceId={item_id}&DeviceId={}&PlaySessionId={play_session_id}\
         &VideoCodec=h264&AudioCodec=aac&MaxAudioChannels=2&VideoBitrate=8000000&AudioBitrate=192000\
         &MaxWidth=1920&MaxHeight=1080&Level=41&Profile=high&TranscodingProtocol=hls&SegmentContainer=ts\
         &TranscodeReasons=ContainerNotSupported&SubtitleMethod=Encode&EnableAdaptiveBitrateStreaming=false\
         &ApiKey={token}",
        client.base, client.device_id,
    ))
}

pub const STREAM_CONTENT_TYPE: &str = "application/x-mpegURL";

/// A URL with the token of the server taken out, for a log line: a stream
/// address carries it as `ApiKey` (or `api_key`).
pub fn redact(url: &str) -> String {
    let Some((path, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let query: Vec<String> = query
        .split('&')
        .map(|part| match part.split_once('=') {
            Some((key, _)) if key.eq_ignore_ascii_case("apikey") || key.eq_ignore_ascii_case("api_key") => {
                format!("{key}=***")
            }
            _ => part.to_string(),
        })
        .collect();
    format!("{path}?{}", query.join("&"))
}

/// The items both ways in one request: all of them for the Jellyfin
/// receiver app, which keeps a queue, and the stream of the first one for
/// the Default Media Receiver, which plays one URL. Nothing to load (an
/// empty request) before sign-in or with no item.
pub fn load_request(client: &Client, server_id: &str, items: &[Item], start_secs: f64) -> LoadRequest {
    let Some(first) = items.first() else {
        return LoadRequest::default();
    };
    let jellyfin = Identity::from_client(client, server_id).map(|identity| JellyfinLoad {
        identity,
        items: items.iter().map(ItemStub::from_item).collect(),
        start_secs,
        audio_index: None,
        subtitle_index: None,
    });
    let play_session_id = uuid::Uuid::new_v4().simple().to_string();
    let media = stream_url(client, &first.id, &play_session_id).map(|url| LoadMedia {
        url,
        content_type: STREAM_CONTENT_TYPE.into(),
        title: first.display_title(),
        subtitle: first.series_name.clone().unwrap_or_default(),
        image_url: Some(client.image_url(&first.id, "Primary", None, 480)),
        start_secs,
        duration: first.run_time_ticks.map(|t| t as f64 / TICKS_PER_SECOND as f64),
    });
    LoadRequest { jellyfin, media }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity {
            server_address: "https://jellyfin.example".into(),
            server_id: "srv1".into(),
            user_id: "u1".into(),
            device_id: "d1".into(),
            access_token: "fake-token".into(),
        }
    }

    #[test]
    fn play_now_carries_the_identity_and_the_item_stubs() {
        let item = ItemStub {
            id: "i1".into(),
            name: "Film".into(),
            kind: "Movie".into(),
            media_type: "Video".into(),
            is_folder: false,
        };
        let message = play_now(&identity(), "Living room", &[item], 90.5, Some(1), Some(-1));
        assert_eq!(message["command"], "PlayNow");
        assert_eq!(message["serverAddress"], "https://jellyfin.example");
        assert_eq!(message["accessToken"], "fake-token");
        assert_eq!(message["userId"], "u1");
        assert_eq!(message["deviceId"], "d1");
        assert_eq!(message["serverId"], "srv1");
        assert_eq!(message["receiverName"], "Living room");
        assert_eq!(message["subtitleBurnIn"], "");
        let options = &message["options"];
        assert_eq!(options["startPositionTicks"], 905_000_000_i64);
        assert_eq!(options["audioStreamIndex"], 1);
        assert_eq!(options["subtitleStreamIndex"], -1);
        assert_eq!(
            options["items"][0],
            json!({"Id": "i1", "ServerId": "srv1", "Name": "Film", "Type": "Movie", "MediaType": "Video", "IsFolder": false})
        );
    }

    #[test]
    fn the_small_commands_match_jellyfin_web() {
        let id = identity();
        assert_eq!(seek(&id, "tv", 12.5)["options"], json!({"position": 12.5}));
        assert_eq!(seek(&id, "tv", -3.)["options"]["position"], 0.0);
        assert_eq!(pause(&id, "tv")["command"], "Pause");
        assert_eq!(pause(&id, "tv")["options"], json!({}));
        assert_eq!(unpause(&id, "tv")["command"], "Unpause");
        assert_eq!(stop(&id, "tv")["command"], "Stop");
        assert_eq!(identify(&id, "tv")["command"], "Identify");
        assert_eq!(set_subtitle(&id, "tv", 3)["options"], json!({"index": 3}));
        assert_eq!(set_audio(&id, "tv", 2)["command"], "SetAudioStreamIndex");
    }

    #[test]
    fn parses_a_report_of_the_receiver() {
        let payload = json!({
            "type": "playbackprogress",
            "data": {
                "ItemId": "i1",
                "PlayState": {
                    "PositionTicks": 1_234_560_000_i64, "IsPaused": true, "VolumeLevel": 35,
                    "IsMuted": false, "AudioStreamIndex": 1, "SubtitleStreamIndex": -1, "CanSeek": true,
                },
                "NowPlayingItem": {"Id": "i1", "RunTimeTicks": 60_000_000_000_i64, "Name": "Film"},
            },
        });
        let report = parse_report(&payload).unwrap();
        assert_eq!(report.kind, "playbackprogress");
        assert!((report.position - 123.456).abs() < 1e-6);
        assert!(report.paused);
        assert_eq!(report.duration, Some(6000.));
        assert_eq!(report.item_id.as_deref(), Some("i1"));
        assert_eq!(report.subtitle_index, Some(-1));
        assert!(parse_report(&json!({"type": "error", "message": "x"})).is_none());
    }

    #[test]
    fn the_stream_url_asks_for_the_cast_profile() {
        let client = Client::new("https://jellyfin.example/", "dev-1").with_session("fake-token", "u1");
        let url = stream_url(&client, "i1", "ps1").unwrap();
        assert!(url.starts_with("https://jellyfin.example/Videos/i1/master.m3u8?MediaSourceId=i1&DeviceId=dev-1&PlaySessionId=ps1"));
        for part in ["VideoCodec=h264", "AudioCodec=aac", "MaxWidth=1920", "TranscodingProtocol=hls", "ApiKey=fake-token"] {
            assert!(url.contains(part), "{part} missing in {url}");
        }
        assert!(stream_url(&Client::new("https://x", "d"), "i1", "p").is_none(), "no token, no URL");
    }

    #[test]
    fn redact_hides_the_token_only() {
        assert_eq!(redact("https://x/a.m3u8?PlaySessionId=ps1&ApiKey=secret"), "https://x/a.m3u8?PlaySessionId=ps1&ApiKey=***");
        assert_eq!(redact("https://x/a.m3u8?api_key=abc&b=1"), "https://x/a.m3u8?api_key=***&b=1");
        assert_eq!(redact("https://x/a.m3u8?b=1"), "https://x/a.m3u8?b=1");
        assert_eq!(redact("no query"), "no query");
    }

    /// A queue goes to the Jellyfin receiver app whole, in its order; the
    /// media receiver gets the stream of the first item.
    #[test]
    fn a_load_request_carries_every_item() {
        let item = |id: &str| -> Item {
            serde_json::from_value(json!({ "Id": id, "Name": id, "Type": "Episode", "RunTimeTicks": 6_000_000_000_i64 })).unwrap()
        };
        let client = Client::new("https://jellyfin.example/", "dev-1").with_session("fake-token", "u1");
        let request = load_request(&client, "srv1", &[item("e1"), item("e2"), item("e3")], 12.);
        let jellyfin = request.jellyfin.unwrap();
        assert_eq!(jellyfin.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["e1", "e2", "e3"]);
        assert_eq!(jellyfin.start_secs, 12.);
        let media = request.media.unwrap();
        assert!(media.url.contains("/Videos/e1/master.m3u8"), "{}", media.url);
        assert_eq!(media.duration, Some(600.));
        let nothing = load_request(&client, "srv1", &[], 0.);
        assert!(nothing.jellyfin.is_none() && nothing.media.is_none());
    }
}
