// SPDX-License-Identifier: AGPL-3.0-or-later
//! The JSON of the cast namespaces: what the sender says, and what the
//! device answers. A request carries a `requestId` and the answer repeats
//! it; the connection numbers a request when it sends it, so the messages
//! here have none.

use serde::Deserialize;
use serde_json::{Value, json};

pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";

/// The platform of the device, before an app runs.
pub const RECEIVER: &str = "receiver-0";
/// Our name on the connection.
pub const SENDER: &str = "sender-0";

/// The Default Media Receiver of Google: plays a URL.
pub const APP_DEFAULT_MEDIA: &str = "CC1AD845";

// ----- connection and heartbeat --------------------------------------------

pub fn connect() -> Value {
    json!({ "type": "CONNECT", "userAgent": crate::brand::FOLDER, "origin": {} })
}

pub fn close() -> Value {
    json!({ "type": "CLOSE" })
}

pub fn ping() -> Value {
    json!({ "type": "PING" })
}

pub fn pong() -> Value {
    json!({ "type": "PONG" })
}

// ----- receiver --------------------------------------------------------------

/// The status of the device on the receiver namespace, and of the item
/// on the media namespace of an app.
pub fn get_status() -> Value {
    json!({ "type": "GET_STATUS" })
}

pub fn launch(app_id: &str) -> Value {
    json!({ "type": "LAUNCH", "appId": app_id })
}

pub fn stop_app(session_id: &str) -> Value {
    json!({ "type": "STOP", "sessionId": session_id })
}

/// `level` from 0 to 1.
pub fn set_volume(level: f64) -> Value {
    json!({ "type": "SET_VOLUME", "volume": { "level": level.clamp(0., 1.) } })
}

pub fn set_muted(muted: bool) -> Value {
    json!({ "type": "SET_VOLUME", "volume": { "muted": muted } })
}

pub fn app_availability(app_ids: &[String]) -> Value {
    json!({ "type": "GET_APP_AVAILABILITY", "appId": app_ids })
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct App {
    pub app_id: String,
    pub display_name: String,
    pub session_id: String,
    pub transport_id: String,
    pub status_text: String,
    pub is_idle_screen: bool,
    #[serde(deserialize_with = "namespace_names")]
    pub namespaces: Vec<String>,
}

impl App {
    pub fn speaks(&self, namespace: &str) -> bool {
        self.namespaces.iter().any(|n| n == namespace)
    }
}

/// `[{"name": ".."}]` to the names.
fn namespace_names<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    struct Named {
        name: String,
    }
    let list = Vec::<Named>::deserialize(d)?;
    Ok(list.into_iter().map(|n| n.name).collect())
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Volume {
    pub level: f64,
    pub muted: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReceiverStatus {
    pub applications: Vec<App>,
    pub volume: Option<Volume>,
}

/// The entries of a list that read as `T`. One that does not is left out
/// with a word in the log, so one odd field of one entry does not cost the
/// whole status.
fn entries<T: serde::de::DeserializeOwned>(list: &Value, what: &str) -> Vec<T> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| match T::deserialize(entry) {
            Ok(entry) => Some(entry),
            Err(err) => {
                log::debug!("cast: {what} not read: {err}");
                None
            }
        })
        .collect()
}

impl ReceiverStatus {
    /// `RECEIVER_STATUS` to the status; `None` for another message.
    pub fn parse(payload: &Value) -> Option<Self> {
        if payload["type"] != "RECEIVER_STATUS" {
            return None;
        }
        let status = &payload["status"];
        let volume = Option::<Volume>::deserialize(&status["volume"])
            .inspect_err(|err| log::debug!("cast: the volume of the receiver status not read: {err}"))
            .unwrap_or_default();
        Some(Self { applications: entries(&status["applications"], "an app of the receiver status"), volume })
    }

    /// The app that runs, if one does and it is not the idle screen.
    pub fn running(&self) -> Option<&App> {
        self.applications.iter().find(|app| !app.is_idle_screen)
    }
}

// ----- media -----------------------------------------------------------------

/// What the Default Media Receiver loads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadMedia {
    pub url: String,
    pub content_type: String,
    pub title: String,
    pub subtitle: String,
    pub image_url: Option<String>,
    pub start_secs: f64,
    /// Length in seconds, when known; a transcoded stream has none.
    pub duration: Option<f64>,
}

pub fn load(media: &LoadMedia) -> Value {
    let mut item = json!({
        "contentId": media.url,
        "contentType": media.content_type,
        "streamType": "BUFFERED",
        "metadata": {
            "metadataType": 0,
            "title": media.title,
            "subtitle": media.subtitle,
            "images": media.image_url.iter().map(|url| json!({ "url": url })).collect::<Vec<_>>(),
        },
    });
    if let Some(duration) = media.duration {
        item["duration"] = json!(duration);
    }
    json!({
        "type": "LOAD", "media": item,
        "autoplay": true, "currentTime": media.start_secs,
    })
}

pub fn play(media_session_id: i64) -> Value {
    json!({ "type": "PLAY", "mediaSessionId": media_session_id })
}

pub fn pause(media_session_id: i64) -> Value {
    json!({ "type": "PAUSE", "mediaSessionId": media_session_id })
}

pub fn media_stop(media_session_id: i64) -> Value {
    json!({ "type": "STOP", "mediaSessionId": media_session_id })
}

/// Without `resumeState` the receiver keeps its state: a seek while paused
/// stays paused, as a seek in the player does.
pub fn seek(media_session_id: i64, secs: f64) -> Value {
    json!({
        "type": "SEEK", "mediaSessionId": media_session_id,
        "currentTime": secs.max(0.),
    })
}

pub fn edit_tracks(media_session_id: i64, active: &[i64]) -> Value {
    json!({
        "type": "EDIT_TRACKS_INFO", "mediaSessionId": media_session_id,
        "activeTrackIds": active,
    })
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaItem {
    pub content_id: String,
    pub duration: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaStatus {
    pub media_session_id: i64,
    /// IDLE, BUFFERING, PLAYING, PAUSED.
    pub player_state: String,
    pub current_time: f64,
    pub idle_reason: Option<String>,
    pub media: Option<MediaItem>,
    pub active_track_ids: Vec<i64>,
}

impl MediaStatus {
    /// `MEDIA_STATUS` to its entries; `Some(empty)` when nothing is
    /// loaded, `None` for another message.
    pub fn parse(payload: &Value) -> Option<Vec<Self>> {
        if payload["type"] != "MEDIA_STATUS" {
            return None;
        }
        Some(entries(&payload["status"], "an entry of the media status"))
    }
}

/// The `requestId` of an answer; 0 or none for a broadcast.
pub fn request_id(payload: &Value) -> Option<u64> {
    payload["requestId"].as_u64().filter(|id| *id != 0)
}

/// An error answer, as one line.
pub fn error_text(payload: &Value) -> Option<String> {
    let kind = payload["type"].as_str()?;
    if !matches!(kind, "LAUNCH_ERROR" | "INVALID_REQUEST" | "LOAD_FAILED" | "LOAD_CANCELLED" | "ERROR") {
        return None;
    }
    let reason = payload["reason"]
        .as_str()
        .or_else(|| payload["detailedErrorCode"].as_u64().map(|_| "see detailedErrorCode"))
        .unwrap_or("");
    Some(if reason.is_empty() { kind.to_string() } else { format!("{kind}: {reason}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_carry_the_fields_of_the_spec() {
        assert_eq!(get_status(), json!({"type": "GET_STATUS"}));
        assert_eq!(launch("F007D354"), json!({"type": "LAUNCH", "appId": "F007D354"}));
        assert_eq!(stop_app("s1"), json!({"type": "STOP", "sessionId": "s1"}));
        assert_eq!(set_volume(1.5), json!({"type": "SET_VOLUME", "volume": {"level": 1.0}}));
        assert_eq!(set_muted(true), json!({"type": "SET_VOLUME", "volume": {"muted": true}}));
        assert_eq!(
            app_availability(&["A".into(), "B".into()]),
            json!({"type": "GET_APP_AVAILABILITY", "appId": ["A", "B"]})
        );
        assert_eq!(seek(4, -2.), json!({"type": "SEEK", "mediaSessionId": 4, "currentTime": 0.0}));
        assert_eq!(pause(4)["type"], "PAUSE");
        assert_eq!(play(4)["mediaSessionId"], 4);
        assert_eq!(media_stop(4)["type"], "STOP");
        assert_eq!(edit_tracks(4, &[2])["activeTrackIds"], json!([2]));
        assert_eq!(connect()["type"], "CONNECT");
        assert_eq!(ping(), json!({"type": "PING"}));
    }

    #[test]
    fn load_names_the_stream() {
        let media = LoadMedia {
            url: "https://s/v.m3u8".into(),
            content_type: "application/x-mpegURL".into(),
            title: "Film".into(),
            subtitle: "2001".into(),
            image_url: Some("https://s/i.jpg".into()),
            start_secs: 42.5,
            duration: Some(600.),
        };
        let message = load(&media);
        assert_eq!(message["type"], "LOAD");
        assert_eq!(message["autoplay"], true);
        assert_eq!(message["currentTime"], 42.5);
        let item = &message["media"];
        assert_eq!(item["contentId"], "https://s/v.m3u8");
        assert_eq!(item["streamType"], "BUFFERED");
        assert_eq!(item["duration"], 600.0);
        assert_eq!(item["metadata"]["title"], "Film");
        assert_eq!(item["metadata"]["images"][0]["url"], "https://s/i.jpg");
        let bare = load(&LoadMedia::default());
        assert!(bare["media"].get("duration").is_none());
        assert_eq!(bare["media"]["metadata"]["images"], json!([]));
    }

    /// One entry with a field of a kind the app does not expect (a null
    /// where a text goes) is left out; the rest of the status still reads.
    /// Before, the whole status was dropped without a word.
    #[test]
    fn one_odd_entry_does_not_cost_the_status() {
        let payload = json!({
            "type": "RECEIVER_STATUS",
            "status": {
                "applications": [
                    {"appId": "ODD", "statusText": null, "transportId": "t0"},
                    {"appId": "F007D354", "sessionId": "s-1", "transportId": "web-7"},
                ],
                "volume": {"level": 0.5, "muted": true},
            },
        });
        let status = ReceiverStatus::parse(&payload).expect("the status reads");
        assert_eq!(status.running().map(|app| app.transport_id.as_str()), Some("web-7"));
        assert_eq!(status.volume, Some(Volume { level: 0.5, muted: true }));
        let media = json!({"type": "MEDIA_STATUS", "status": [
            {"mediaSessionId": "odd"},
            {"mediaSessionId": 3, "playerState": "PLAYING"},
        ]});
        let entries = MediaStatus::parse(&media).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].media_session_id, 3);
    }

    #[test]
    fn parses_a_receiver_status() {
        let payload = json!({
            "type": "RECEIVER_STATUS", "requestId": 2,
            "status": {
                "applications": [{
                    "appId": "F007D354", "displayName": "Jellyfin", "sessionId": "s-1",
                    "transportId": "web-7", "statusText": "Ready",
                    "namespaces": [{"name": NS_MEDIA}, {"name": "urn:x-cast:com.connectsdk"}],
                }],
                "volume": {"controlType": "attenuation", "level": 0.35, "muted": false, "stepInterval": 0.05},
            },
        });
        let status = ReceiverStatus::parse(&payload).unwrap();
        let app = status.running().unwrap();
        assert_eq!(app.transport_id, "web-7");
        assert!(app.speaks(NS_MEDIA));
        assert_eq!(status.volume.unwrap().level, 0.35);
        assert_eq!(request_id(&payload), Some(2));
        // The idle screen is not an app that runs.
        let idle = json!({"type": "RECEIVER_STATUS", "status": {"applications": [{"appId": "E8C28D3C", "isIdleScreen": true}], "volume": {"level": 1}}});
        assert!(ReceiverStatus::parse(&idle).unwrap().running().is_none());
        assert_eq!(request_id(&idle), None);
        assert!(ReceiverStatus::parse(&json!({"type": "MEDIA_STATUS"})).is_none());
    }

    #[test]
    fn parses_a_media_status() {
        let payload = json!({
            "type": "MEDIA_STATUS", "requestId": 0,
            "status": [{
                "mediaSessionId": 3, "playbackRate": 1, "playerState": "PAUSED", "currentTime": 12.25,
                "supportedMediaCommands": 12303, "volume": {"level": 1, "muted": false},
                "media": {"contentId": "https://s/v.m3u8", "contentType": "application/x-mpegURL", "duration": 600.5},
                "activeTrackIds": [1], "idleReason": null,
            }],
        });
        let status = MediaStatus::parse(&payload).unwrap();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].player_state, "PAUSED");
        assert_eq!(status[0].current_time, 12.25);
        assert_eq!(status[0].media.as_ref().unwrap().duration, Some(600.5));
        assert_eq!(status[0].active_track_ids, vec![1]);
        let idle = json!({"type": "MEDIA_STATUS", "requestId": 5, "status": []});
        assert_eq!(MediaStatus::parse(&idle).unwrap(), vec![]);
        let ended = json!({"type": "MEDIA_STATUS", "status": [{"mediaSessionId": 3, "playerState": "IDLE", "idleReason": "FINISHED"}]});
        assert_eq!(MediaStatus::parse(&ended).unwrap()[0].idle_reason.as_deref(), Some("FINISHED"));
    }

    #[test]
    fn names_the_errors() {
        assert_eq!(error_text(&json!({"type": "LAUNCH_ERROR", "reason": "NOT_FOUND"})).unwrap(), "LAUNCH_ERROR: NOT_FOUND");
        assert_eq!(error_text(&json!({"type": "LOAD_FAILED", "requestId": 4})).unwrap(), "LOAD_FAILED");
        assert!(error_text(&json!({"type": "PONG"})).is_none());
    }
}
