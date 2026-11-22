//! What the iOS app's sign-in and account parts ask of a server beyond the
//! plain calls: the profiles of the sign-in screen with their pictures,
//! and two read-only views of the dashboard's Overview (what came in this
//! week, and the playbacks the server converts).

use bloom_core::jellyfin::Item;
use serde::Deserialize;
use serde_json::Value;

use crate::{BloomError, MediaItem, Server, flag, media, run};

/// A user the server lists on its sign-in page.
#[derive(uniffi::Record)]
pub struct PublicProfile {
    pub id: String,
    pub name: String,
    /// The user's picture, when they set one.
    pub picture: Option<String>,
}

/// An item added to the libraries lately, with the date it came.
#[derive(uniffi::Record)]
pub struct RecentAddition {
    pub item: MediaItem,
    /// The server's date ("2026-11-14T10:20:30.1234567Z").
    pub added: String,
}

/// A playback the server converts (a transcode or a remux) now.
#[derive(uniffi::Record)]
pub struct ActiveTranscode {
    pub user: String,
    pub device: String,
    pub client: String,
    pub title: String,
    /// "Video converted to h264 · Audio as it is · mp4"
    pub what: String,
    /// Why, in words, as the server gives it; empty when it does not say.
    pub reasons: Vec<String>,
    /// 0 to 100, when the server says.
    pub progress: Option<f64>,
    /// "Hardware (videotoolbox)" or "Software"; none when the server does not say.
    pub encoder: Option<String>,
    pub paused: bool,
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn opt(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string)
}

/// A Jellyfin `TranscodeReason` in plain words; an unknown one split at
/// its capitals ("SomethingNew" -> "Something new").
pub(crate) fn reason_words(reason: &str) -> String {
    let known = match reason {
        "ContainerNotSupported" => "The device does not play this file type",
        "VideoCodecNotSupported" => "The device does not play this video format",
        "AudioCodecNotSupported" => "The device does not play this audio format",
        "SubtitleCodecNotSupported" => "Subtitles burned into the picture",
        "AudioIsExternal" => "The audio is in a separate file",
        "SecondaryAudioNotSupported" => "A second audio track is not supported",
        "VideoProfileNotSupported" => "Video profile not supported",
        "VideoLevelNotSupported" => "Video level not supported",
        "VideoResolutionNotSupported" => "Resolution too high for the device",
        "VideoBitDepthNotSupported" => "Video bit depth not supported",
        "VideoFramerateNotSupported" => "Frame rate not supported",
        "RefFramesNotSupported" => "Reference frames not supported",
        "AnamorphicVideoNotSupported" => "Anamorphic video not supported",
        "InterlacedVideoNotSupported" => "Interlaced video not supported",
        "AudioChannelsNotSupported" => "Too many audio channels for the device",
        "AudioProfileNotSupported" => "Audio profile not supported",
        "AudioSampleRateNotSupported" => "Audio sample rate not supported",
        "AudioBitDepthNotSupported" => "Audio bit depth not supported",
        "ContainerBitrateExceedsLimit" => "Over the bitrate limit",
        "VideoBitrateNotSupported" => "Video bitrate over the limit",
        "AudioBitrateNotSupported" => "Audio bitrate over the limit",
        "UnknownVideoStreamInfo" => "The server does not know the video format",
        "UnknownAudioStreamInfo" => "The server does not know the audio format",
        "DirectPlayError" => "Direct play failed",
        "VideoRangeTypeNotSupported" => "HDR type not supported",
        "VideoCodecTagNotSupported" => "Video codec tag not supported",
        "StreamCountExceedsLimit" => "Too many streams in the file",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let mut words = String::new();
    for (i, c) in reason.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            words.push(' ');
            words.extend(c.to_lowercase());
        } else {
            words.push(c);
        }
    }
    words
}

/// The conversion of one session's TranscodingInfo, in words.
fn transcode(session: &Value) -> Option<ActiveTranscode> {
    let item = session.get("NowPlayingItem")?;
    let info = session.get("TranscodingInfo").filter(|i| i.is_object())?;
    let title = match (opt(item, "SeriesName"), opt(item, "Name")) {
        (Some(series), Some(name)) => format!("{series} · {name}"),
        (_, name) => name.unwrap_or_default(),
    };
    let part = |direct: &str, codec: &str, kind: &str| {
        if flag(info, direct) {
            format!("{kind} as it is")
        } else {
            match opt(info, codec) {
                Some(codec) => format!("{kind} converted to {codec}"),
                None => format!("{kind} converted"),
            }
        }
    };
    let mut what = vec![part("IsVideoDirect", "VideoCodec", "Video"), part("IsAudioDirect", "AudioCodec", "Audio")];
    if let Some(container) = opt(info, "Container") {
        what.push(container);
    }
    let reasons = match info.get("TranscodeReasons") {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(reason_words).collect(),
        // Some versions send one comma-separated string.
        Some(Value::String(text)) => text.split(',').map(str::trim).filter(|r| !r.is_empty()).map(reason_words).collect(),
        _ => Vec::new(),
    };
    let encoder = match opt(info, "HardwareAccelerationType") {
        Some(kind) if kind != "none" => Some(format!("Hardware ({kind})")),
        Some(_) => Some("Software".to_string()),
        None => None,
    };
    Some(ActiveTranscode {
        user: s(session, "UserName"),
        device: s(session, "DeviceName"),
        client: s(session, "Client"),
        title,
        what: what.join(" · "),
        reasons,
        progress: info.get("CompletionPercentage").and_then(Value::as_f64),
        encoder,
        paused: flag(&session["PlayState"], "IsPaused"),
    })
}

/// The civil date of a day count since 1970 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The server's form of a moment `days` ago, in UTC: a date the server
/// sends compares with it as text.
fn days_ago(days: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let at = now - days * 86_400;
    let (y, m, d) = civil(at.div_euclid(86_400));
    let secs = at.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

#[uniffi::export]
impl Server {
    /// The users of the sign-in page, with their pictures. Needs no sign-in.
    pub async fn public_profiles(&self) -> Result<Vec<PublicProfile>, BloomError> {
        let client = self.client()?;
        run(move || {
            let base = client.base.trim_end_matches('/').to_string();
            Ok(client
                .public_users()?
                .into_iter()
                .map(|u| PublicProfile {
                    // 240 px: a 80-point disc on a 3x screen.
                    picture: u.primary_image_tag.as_deref().map(|tag| format!("{base}/Users/{}/Images/Primary?maxWidth=240&tag={tag}", u.id)),
                    id: u.id,
                    name: u.name,
                })
                .collect())
        })
        .await
    }

    /// Films and episodes added in the last `days` days, newest first (at
    /// most `limit`). The libraries of the signed-in user.
    pub async fn admin_recent_additions(&self, days: u32, limit: u32) -> Result<Vec<RecentAddition>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            #[derive(Deserialize)]
            struct Added {
                #[serde(rename = "DateCreated", default)]
                added: String,
                #[serde(flatten)]
                item: Item,
            }
            #[derive(Deserialize)]
            struct Page {
                #[serde(rename = "Items", default)]
                items: Vec<Added>,
            }
            let user = client.user()?.to_string();
            let page: Page = client.get(
                "/Items",
                &[
                    ("userId", user),
                    ("includeItemTypes", "Movie,Episode".to_string()),
                    ("recursive", "true".to_string()),
                    ("sortBy", "DateCreated".to_string()),
                    ("sortOrder", "Descending".to_string()),
                    ("fields", "DateCreated,PrimaryImageAspectRatio".to_string()),
                    ("enableImageTypes", "Primary,Thumb,Backdrop".to_string()),
                    ("limit", limit.to_string()),
                ],
            )?;
            let since = days_ago(i64::from(days));
            Ok(page
                .items
                .into_iter()
                .filter(|a| a.added.as_str() >= since.as_str())
                .map(|a| RecentAddition { item: media(&client, &a.item), added: a.added })
                .collect())
        })
        .await
    }

    /// The playbacks the server converts now, with why.
    pub async fn admin_transcodes(&self) -> Result<Vec<ActiveTranscode>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let sessions: Value = client.sessions(&[("ActiveWithinSeconds", "960".to_string())])?;
            Ok(sessions.as_array().into_iter().flatten().filter_map(transcode).collect())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_406), (2025, 11, 14));
        assert_eq!(civil(-1), (1969, 12, 31));
    }

    #[test]
    fn a_transcode_in_words() {
        let session = json!({
            "UserName": "ana", "DeviceName": "TV", "Client": "Jellyfin Web",
            "NowPlayingItem": {"Name": "Pilot", "SeriesName": "The Show"},
            "PlayState": {"IsPaused": false},
            "TranscodingInfo": {"IsVideoDirect": false, "VideoCodec": "h264", "IsAudioDirect": true, "Container": "ts",
                                "TranscodeReasons": ["VideoCodecNotSupported", "SomethingNew"], "HardwareAccelerationType": "none",
                                "CompletionPercentage": 12.5}
        });
        let t = transcode(&session).unwrap();
        assert_eq!(t.title, "The Show · Pilot");
        assert_eq!(t.what, "Video converted to h264 · Audio as it is · ts");
        assert_eq!(t.reasons, ["The device does not play this video format", "Something new"]);
        assert_eq!(t.encoder.as_deref(), Some("Software"));
        assert_eq!(t.progress, Some(12.5));
        // A direct play has no TranscodingInfo.
        assert!(transcode(&json!({"NowPlayingItem": {"Name": "x"}})).is_none());
        // A string of reasons, as some versions send it.
        let one = json!({"NowPlayingItem": {"Name": "x"}, "TranscodingInfo": {"TranscodeReasons": "ContainerNotSupported, AudioIsExternal"}});
        assert_eq!(transcode(&one).unwrap().reasons.len(), 2);
    }
}
