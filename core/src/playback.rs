// SPDX-License-Identifier: AGPL-3.0-or-later
//! How the server is asked to play an item (`PlaybackInfo`): what the app
//! tells it about mpv (the device profile), the body of the request, and
//! the parts of its answer that need no player to read. The app resolves
//! the answer into a load of its player (`src/stream.rs`).

use serde::Deserialize;

use crate::jellyfin::{Client, MediaSource, MediaStream, StreamKind, TICKS_PER_SECOND};

// ----- the answer of the server ------------------------------------------------

/// How the item gets to the player: the file itself, or the stream the
/// server makes of it (`TranscodingUrl`). The second is a transcode for
/// every rule of the app, also when the server only repacks the file (a
/// remux): the server runs a job for it that must end, and the stream has
/// the one audio track the server chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayMethod {
    DirectPlay,
    Transcode,
}

impl PlayMethod {
    /// The name of the method as the app sees it.
    pub fn as_str(self) -> &'static str {
        match self {
            PlayMethod::DirectPlay => "DirectPlay",
            PlayMethod::Transcode => "Transcode",
        }
    }

    /// The name for the playback reports. The server has a third one,
    /// "DirectStream", for a stream it makes with the video of the file
    /// as it is (`remux`); its dashboard and its activity show the name.
    pub fn reported(self, remux: bool) -> &'static str {
        match (self, remux) {
            (PlayMethod::Transcode, true) => "DirectStream",
            _ => self.as_str(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackInfo {
    #[serde(default)]
    pub media_sources: Vec<MediaSource>,
    #[serde(default)]
    pub play_session_id: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
}

impl PlaybackInfo {
    /// The source of the item: the one with its id, else the first.
    pub fn source(&self, item_id: &str) -> Option<&MediaSource> {
        self.media_sources
            .iter()
            .find(|s| s.id == item_id)
            .or_else(|| self.media_sources.first())
    }
}

/// The name of a stream in a menu.
pub fn stream_title(stream: &MediaStream) -> String {
    stream
        .display_title
        .clone()
        .unwrap_or_else(|| format!("Track {}", stream.index))
}

/// mpv's id of a stream of the file, by its server index: mpv numbers the
/// tracks of a kind (audio, subtitles) from 1 in file order, and the
/// server numbers every stream of the source in one list. A stream in a
/// file of its own is not in mpv's list, so it has none. Every choice of
/// a track by its server index goes through this.
pub fn mpv_id(streams: &[MediaStream], kind: StreamKind, index: i64) -> Option<i64> {
    streams
        .iter()
        .filter(|s| s.kind == kind && !s.is_external)
        .position(|s| s.index == index)
        .map(|place| place as i64 + 1)
}

/// What the server encodes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Video and audio together, in bits per second.
    pub bitrate: u64,
    pub video_codec: String,
    pub audio_codec: String,
    pub container: String,
    pub protocol: String,
}

/// The track after `now` in a list, round and round; none when that is
/// `now` itself or the list is empty.
pub fn next_track(tracks: &[i64], now: Option<i64>) -> Option<i64> {
    let place = now.and_then(|now| tracks.iter().position(|t| *t == now));
    let next = *tracks.get(place.map_or(0, |p| (p + 1) % tracks.len()))?;
    (Some(next) != now).then_some(next)
}
// ----- the request -------------------------------------------------------------

/// One load: the item and the choices for it.
#[derive(Clone, Debug)]
pub struct Load {
    pub item_id: String,
    pub title: String,
    pub start_secs: f64,
    pub paused: bool,
    /// Comes back in the `Loaded` or `LoadFailed` event.
    pub token: u64,
    /// Server index of the audio stream the user chose; none leaves the
    /// choice to the server.
    pub audio: Option<i64>,
    /// Server index of the subtitle; -1 for none; `None` leaves it to the
    /// server.
    pub subtitle: Option<i64>,
    /// The subtitle is burned into the transcode, whatever the profile
    /// says: after its file did not come (`subtitle_file_failed`).
    pub burn_in: bool,
}

/// The limit sent for "Auto": above every file. The server still applies
/// its own limit for a client outside its network.
const NO_LIMIT: u64 = 1_000_000_000;

/// What mpv plays: every container and codec (the profile names none, so
/// the server matches them all), and for a transcode HEVC or H.264 with
/// aac, ac3 or eac3 in fragmented MP4 segments over HLS. HEVC first: a
/// file the server must transcode (over its limit for a remote client)
/// keeps its codec, which gives more picture for the same bitrate, and the
/// server's QSV encoder makes both. Measured on 2026-10-20 with Jellyfin
/// 12.1.0: a Dolby Vision file (The Agency, 4K at 25.8 Mbps over a 25 Mbps
/// limit) came as HEVC instead of H.264, but still tone mapped to SDR
/// (bt.709) with the HDR ranges declared below; the server tone maps a
/// Dolby Vision source whatever the client says. Text subtitles come as
/// files, and so do PGS and VobSub tracks (`GRAPHICAL`): the server
/// extracts an embedded one to a `.sup` or an `.mks` and serves the file,
/// which mpv takes on the side, so an image track on a transcode is not
/// burned in and a switch of it does not load the item again. The server
/// burns in the rest (DVB, teletext, an external `.idx/.sub` pair).
pub fn device_profile(max_bitrate: Option<u64>) -> serde_json::Value {
    const TEXT: [&str; 4] = ["srt", "ass", "ssa", "vtt"];
    // The server names the two image formats so; "vobsub" needs the
    // container, since ffmpeg cannot write an `.idx/.sub` pair (the
    // web client's profile has the same two entries).
    const GRAPHICAL: [(&str, Option<&str>); 2] = [("pgssub", None), ("vobsub", Some("mks"))];
    const EMBEDDED: [&str; 24] = [
        "srt", "subrip", "ass", "ssa", "vtt", "webvtt", "mov_text", "text", "ttml", "sami",
        "microdvd", "subviewer", "eia_608", "dvb_teletext", "pgssub", "pgs",
        "hdmv_pgs_subtitle", "dvdsub", "dvd_subtitle", "dvbsub", "dvb_subtitle", "vobsub",
        "xsub", "arib_caption",
    ];
    let mut subtitles: Vec<serde_json::Value> = EMBEDDED
        .iter()
        .map(|format| serde_json::json!({ "Format": format, "Method": "Embed" }))
        .collect();
    subtitles.extend(
        TEXT.iter()
            .map(|format| serde_json::json!({ "Format": format, "Method": "External" })),
    );
    subtitles.extend(GRAPHICAL.iter().map(|(format, container)| {
        let mut entry = serde_json::json!({ "Format": format, "Method": "External" });
        if let Some(container) = container {
            entry["Container"] = serde_json::Value::from(*container);
        }
        entry
    }));
    let mut profile = serde_json::json!({
        "Name": crate::config::APP_NAME,
        "MaxStaticBitrate": 120_000_000u64,
        "MusicStreamingTranscodingBitrate": 384_000u64,
        "DirectPlayProfiles": [
            { "Type": "Video" },
            { "Type": "Audio" }
        ],
        "TranscodingProfiles": [{
            "Container": "mp4",
            "Type": "Video",
            "VideoCodec": "hevc,h264",
            "AudioCodec": "aac,ac3,eac3,mp3",
            "Context": "Streaming",
            "Protocol": "hls",
            "MaxAudioChannels": "6",
            "MinSegments": "2",
            "BreakOnNonKeyFrames": true,
            // A transcode is at most 1080p. The server tone maps it to
            // 8-bit SDR in any case, and on the screens of a Mac a 1080p
            // HEVC picture at the same bitrate is as good as a 4K one
            // while the encoder does a quarter of the work and the stream
            // is easier on a weak link. A direct play is not touched.
            // Measured 2026-10-20: the server answers with MaxWidth=1920
            // and scales (scale_vaapi 1920x802 for a 3832x1600 source); the
            // bitrate stays at the cap asked for.
            "Conditions": [
                { "Condition": "LessThanEqual", "Property": "Width", "Value": "1920", "IsRequired": false },
                { "Condition": "LessThanEqual", "Property": "Height", "Value": "1080", "IsRequired": false }
            ]
        }],
        // The HDR ranges the client plays, for an HEVC transcode: a server
        // tone maps to SDR what the client does not name. Every type the
        // server knows is here, so a direct play is judged as before. (The
        // server of 2026-10-20 tone mapped its Dolby Vision sources all the
        // same; see `device_profile`.)
        "CodecProfiles": [{
            "Type": "Video",
            "Codec": "hevc",
            "Conditions": [{
                "Condition": "EqualsAny",
                "Property": "VideoRangeType",
                "Value": "SDR|HDR10|HLG|HDR10Plus|DOVI|DOVIWithHDR10|DOVIWithHDR10Plus|DOVIWithHLG|DOVIWithSDR|DOVIWithEL|DOVIWithELHDR10|DOVIInvalid",
                "IsRequired": false
            }]
        }],
        "ContainerProfiles": [],
        "SubtitleProfiles": subtitles,
    });
    // "Auto" must name a limit too: without one the server takes 8 Mbps,
    // and every file above that is transcoded.
    profile["MaxStreamingBitrate"] = serde_json::Value::from(max_bitrate.unwrap_or(NO_LIMIT));
    profile
}

/// The body of the `PlaybackInfo` request.
pub fn request_body(client: &Client, load: &Load, max_bitrate: Option<u64>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "UserId": client.user_id.as_deref().unwrap_or_default(),
        "DeviceProfile": device_profile(max_bitrate),
        "StartTimeTicks": (load.start_secs.max(0.) * TICKS_PER_SECOND as f64) as i64,
        "MediaSourceId": load.item_id,
        "EnableDirectPlay": true,
        "EnableDirectStream": true,
        "EnableTranscoding": true,
        "AllowVideoStreamCopy": true,
        "AllowAudioStreamCopy": true,
        "AutoOpenLiveStream": true,
        "AlwaysBurnInSubtitleWhenTranscoding": load.burn_in,
    });
    body["MaxStreamingBitrate"] = serde_json::Value::from(max_bitrate.unwrap_or(NO_LIMIT));
    if let Some(audio) = load.audio {
        body["AudioStreamIndex"] = serde_json::Value::from(audio);
    }
    if let Some(subtitle) = load.subtitle {
        body["SubtitleStreamIndex"] = serde_json::Value::from(subtitle);
    }
    body
}
/// The value of one name in the query of a URL.
pub fn query_value<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    let (_, query) = url.split_once('?')?;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}
/// A URL path without its `ApiKey` (or `api_key`) query value.
pub fn without_key(path: &str) -> String {
    let Some((head, query)) = path.split_once('?') else {
        return path.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or_default();
            !name.eq_ignore_ascii_case("ApiKey") && !name.eq_ignore_ascii_case("api_key")
        })
        .collect();
    format!("{head}?{}", kept.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM: &str = "00112233445566778899aabbccddeeff";

    fn client() -> Client {
        Client::new("https://media.example.com", "device-1").with_session("tok", "user-1")
    }

    /// The reports name a remux as the server does; the app itself has
    /// two methods.
    #[test]
    fn the_reports_name_a_remux_a_direct_stream() {
        assert_eq!(PlayMethod::Transcode.reported(true), "DirectStream");
        assert_eq!(PlayMethod::Transcode.reported(false), "Transcode");
        assert_eq!(PlayMethod::DirectPlay.reported(false), "DirectPlay");
        assert_eq!(PlayMethod::DirectPlay.reported(true), "DirectPlay");
        assert_eq!(PlayMethod::Transcode.as_str(), "Transcode");
    }

    #[test]
    fn profile_describes_mpv() {
        let profile = device_profile(Some(1_500_000));
        assert_eq!(profile["MaxStreamingBitrate"], 1_500_000);
        assert_eq!(profile["DirectPlayProfiles"][0]["Type"], "Video");
        let hls = &profile["TranscodingProfiles"][0];
        assert_eq!(hls["Protocol"], "hls");
        assert_eq!(hls["VideoCodec"], "hevc,h264");
        assert_eq!(hls["Container"], "mp4");
        assert_eq!(hls["BreakOnNonKeyFrames"], true);
        assert_eq!(hls["Conditions"][0]["Property"], "Width");
        assert_eq!(hls["Conditions"][0]["Value"], "1920");
        let hdr = &profile["CodecProfiles"][0];
        assert_eq!(hdr["Codec"], "hevc");
        assert_eq!(hdr["Conditions"][0]["Property"], "VideoRangeType");
        assert!(hdr["Conditions"][0]["Value"].as_str().unwrap().contains("DOVIWithHDR10Plus"));
        let subtitles = profile["SubtitleProfiles"].as_array().unwrap();
        let has = |format: &str, method: &str| subtitles.iter().any(|p| p["Format"] == format && p["Method"] == method);
        assert!(has("pgssub", "Embed"));
        // An image track comes as a file too: PGS as it is, VobSub in an
        // `.mks` (the container is the server's condition for that).
        assert!(has("pgssub", "External"));
        let vobsub = subtitles.iter().find(|p| p["Format"] == "vobsub" && p["Method"] == "External").unwrap();
        assert_eq!(vobsub["Container"], "mks");
        assert!(has("srt", "External"));
        assert_eq!(device_profile(None)["MaxStreamingBitrate"], NO_LIMIT);
        let load = Load {
            item_id: ITEM.into(),
            title: String::new(),
            start_secs: 90.,
            paused: false,
            token: 0,
            burn_in: false,
            audio: Some(1),
            subtitle: None,
        };
        let body = request_body(&client(), &load, None);
        assert_eq!(body["StartTimeTicks"], 900_000_000);
        assert_eq!(body["AudioStreamIndex"], 1);
        assert!(body.get("SubtitleStreamIndex").is_none());
        assert_eq!(body["UserId"], "user-1");
    }

    #[test]
    fn a_server_index_maps_to_the_track_of_mpv() {
        let stream = |kind: &str, index, is_external| MediaStream {
            kind: kind.into(),
            index,
            is_external,
            ..Default::default()
        };
        // The external subtitle comes first in the list of the server.
        let streams = [
            stream("Subtitle", 0, true),
            stream("Video", 1, false),
            stream("Audio", 2, false),
            stream("Audio", 3, false),
            stream("Subtitle", 4, false),
            stream("Subtitle", 5, false),
        ];
        assert_eq!(mpv_id(&streams, StreamKind::Audio, 2), Some(1));
        assert_eq!(mpv_id(&streams, StreamKind::Audio, 3), Some(2));
        assert_eq!(mpv_id(&streams, StreamKind::Subtitle, 4), Some(1));
        assert_eq!(mpv_id(&streams, StreamKind::Subtitle, 5), Some(2));
        assert_eq!(mpv_id(&streams, StreamKind::Subtitle, 0), None, "a file of its own is not a track of the file");
        assert_eq!(mpv_id(&streams, StreamKind::Audio, 9), None);
        assert_eq!(mpv_id(&streams, StreamKind::Subtitle, -1), None);
    }

    #[test]
    fn the_next_track_goes_round() {
        assert_eq!(next_track(&[-1, 2, 3], Some(-1)), Some(2));
        assert_eq!(next_track(&[-1, 2, 3], Some(3)), Some(-1));
        assert_eq!(next_track(&[1, 2], None), Some(1));
        assert_eq!(next_track(&[1, 2], Some(9)), Some(1));
        assert_eq!(next_track(&[1], Some(1)), None);
        assert_eq!(next_track(&[], None), None);
    }

    #[test]
    fn the_query_of_a_url() {
        let url = "/videos/x/master.m3u8?VideoCodec=h264&ApiKey=secret&AudioBitrate=384000";
        assert_eq!(query_value(url, "videocodec"), Some("h264"));
        assert_eq!(query_value(url, "Missing"), None);
        assert_eq!(query_value("/no/query", "a"), None);
        assert_eq!(without_key(url), "/videos/x/master.m3u8?VideoCodec=h264&AudioBitrate=384000");
        assert_eq!(without_key("/a?api_key=k"), "/a?");
        assert_eq!(without_key("/a"), "/a");
    }
}
