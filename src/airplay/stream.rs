// SPDX-License-Identifier: AGPL-3.0-or-later
//! The stream an Apple TV can fetch: the HLS master playlist of the server,
//! with the codecs tvOS plays in fMP4 segments. The server copies a video or
//! audio stream that already fits and encodes the rest, as it does for
//! Safari (`browserDeviceProfile.js` of jellyfin-web; the parameter names are
//! the ones the server itself writes in `StreamInfo.ToUrl`).
//!
//! The receiver fetches the playlist and its segments on its own, so the
//! token is in the URL. Never log a URL of this module as it is; use
//! [`redact`].

use anyhow::{Result, anyhow};

use crate::jellyfin::Client;

/// Choices for one stream.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamOptions {
    /// Index of the audio stream in the media source; the server's default
    /// when none.
    pub audio_index: Option<i64>,
    /// A subtitle stream to burn into the picture; AirPlay carries no
    /// subtitle track of ours.
    pub subtitle_index: Option<i64>,
    /// Let an HEVC source through as it is. An Apple TV 4K plays HEVC; an
    /// Apple TV HD does not and needs H.264.
    pub hevc: bool,
}

impl StreamOptions {
    pub fn new() -> Self {
        Self { hevc: true, ..Default::default() }
    }
}

/// A stream the server was asked for, with the session the reports and
/// the stop of the transcode use.
#[derive(Clone, Debug)]
pub struct Stream {
    pub url: String,
    pub play_session_id: String,
}

/// Upper bound for the encoder; a copy of the source stays as it is.
const VIDEO_BITRATE: u32 = 120_000_000;
const AUDIO_BITRATE: u32 = 640_000;
const AUDIO_CHANNELS: u32 = 6;

/// The master playlist URL for one item.
pub fn build_url(
    base: &str,
    item_id: &str,
    device_id: &str,
    token: &str,
    play_session_id: &str,
    options: &StreamOptions,
) -> String {
    let video = if options.hevc { "hevc,h264" } else { "h264" };
    let mut url = format!(
        "{}/Videos/{item_id}/master.m3u8?MediaSourceId={item_id}&DeviceId={device_id}\
         &PlaySessionId={play_session_id}&VideoCodec={video}&AudioCodec=aac,ac3,eac3\
         &SegmentContainer=mp4&MinSegments=2&BreakOnNonKeyFrames=true\
         &VideoBitrate={VIDEO_BITRATE}&AudioBitrate={AUDIO_BITRATE}\
         &TranscodingMaxAudioChannels={AUDIO_CHANNELS}&EnableAudioVbrEncoding=true",
        base.trim_end_matches('/')
    );
    if let Some(index) = options.audio_index {
        url.push_str(&format!("&AudioStreamIndex={index}"));
    }
    if let Some(index) = options.subtitle_index {
        url.push_str(&format!("&SubtitleStreamIndex={index}&SubtitleMethod=Encode"));
    }
    url.push_str(&format!("&ApiKey={token}"));
    url
}

/// The URL with the token taken out, for a log line.
pub fn redact(url: &str) -> String {
    let Some((path, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let mut out = String::with_capacity(url.len());
    out.push_str(path);
    out.push('?');
    for (n, part) in query.split('&').enumerate() {
        if n > 0 {
            out.push('&');
        }
        match part.split_once('=') {
            Some((key, _)) if key.eq_ignore_ascii_case("apikey") || key.eq_ignore_ascii_case("api_key") => {
                out.push_str(key);
                out.push_str("=***");
            }
            _ => out.push_str(part),
        }
    }
    out
}

impl Client {
    /// A new stream of the item for this device, under a play session of
    /// its own.
    pub fn airplay_stream(&self, item_id: &str, options: &StreamOptions) -> Result<Stream> {
        let token = self.token.as_deref().ok_or_else(|| anyhow!("not signed in"))?;
        let play_session_id = uuid::Uuid::new_v4().to_string();
        let url = build_url(&self.base, item_id, &self.device_id, token, &play_session_id, options);
        Ok(Stream { url, play_session_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://media.example.com/";

    fn url(options: &StreamOptions) -> String {
        build_url(BASE, "item1", "dev1", "secret-token", "ps1", options)
    }

    fn query(url: &str) -> Vec<(&str, &str)> {
        let (_, query) = url.split_once('?').unwrap();
        query.split('&').map(|p| p.split_once('=').unwrap()).collect()
    }

    #[test]
    fn master_playlist_with_apple_tv_codecs() {
        let url = url(&StreamOptions::new());
        assert!(url.starts_with("https://media.example.com/Videos/item1/master.m3u8?"));
        let q = query(&url);
        let get = |key: &str| q.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        assert_eq!(get("MediaSourceId"), Some("item1"));
        assert_eq!(get("DeviceId"), Some("dev1"));
        assert_eq!(get("PlaySessionId"), Some("ps1"));
        assert_eq!(get("VideoCodec"), Some("hevc,h264"));
        assert_eq!(get("AudioCodec"), Some("aac,ac3,eac3"));
        assert_eq!(get("SegmentContainer"), Some("mp4"));
        assert_eq!(get("MinSegments"), Some("2"));
        assert_eq!(get("BreakOnNonKeyFrames"), Some("true"));
        assert_eq!(get("ApiKey"), Some("secret-token"));
        assert_eq!(get("AudioStreamIndex"), None);
        assert_eq!(get("SubtitleStreamIndex"), None);
    }

    #[test]
    fn options_add_their_parameters() {
        let url = url(&StreamOptions { audio_index: Some(2), subtitle_index: Some(5), hevc: false });
        let q = query(&url);
        let get = |key: &str| q.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        assert_eq!(get("VideoCodec"), Some("h264"));
        assert_eq!(get("AudioStreamIndex"), Some("2"));
        assert_eq!(get("SubtitleStreamIndex"), Some("5"));
        assert_eq!(get("SubtitleMethod"), Some("Encode"));
    }

    #[test]
    fn redact_hides_the_token_only() {
        let url = url(&StreamOptions::new());
        let safe = redact(&url);
        assert!(!safe.contains("secret-token"), "{safe}");
        assert!(safe.ends_with("&ApiKey=***"), "{safe}");
        assert!(safe.contains("PlaySessionId=ps1"));
        assert_eq!(redact("https://x/a.m3u8?api_key=abc&b=1"), "https://x/a.m3u8?api_key=***&b=1");
        assert_eq!(redact("https://x/a.m3u8?b=1"), "https://x/a.m3u8?b=1");
    }
}
