// SPDX-License-Identifier: AGPL-3.0-or-later
//! How an item gets to the player. The server is asked how to play it
//! (`PlaybackInfo`): it gets what mpv can play and the bitrate the user
//! allows, and answers with the file itself (direct play) or with a
//! transcode over HLS. This module keeps the quality choice, maps the
//! tracks of the server to the tracks of mpv while a transcode plays, ends
//! a transcode that is not needed any more, and shows what plays.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result, anyhow};
use gpui_kit::{
    Context, Div, ParentElement as _, SharedString, Styled, Window, div, px, rgba,
};
use serde::Deserialize;

use crate::{
    app::Bloom,
    jellyfin::{Client, Item, TICKS_PER_SECOND},
    player::{PlayRequest, PlayState, Player, SubtitleFile},
    ui::{glass::glass, menu::MenuItem},
};

// ----- the quality ladder ----------------------------------------------------

/// One choice of the quality menu: bits per second, its name, and the
/// largest picture the web client pairs with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rung {
    pub bitrate: u64,
    pub label: &'static str,
    pub height: &'static str,
}

/// The ladder of the web client (`qualityOptions.js`), highest first.
pub const LADDER: [Rung; 14] = [
    Rung { bitrate: 120_000_000, label: "120 Mbps", height: "4K" },
    Rung { bitrate: 80_000_000, label: "80 Mbps", height: "4K" },
    Rung { bitrate: 60_000_000, label: "60 Mbps", height: "4K" },
    Rung { bitrate: 40_000_000, label: "40 Mbps", height: "4K" },
    Rung { bitrate: 20_000_000, label: "20 Mbps", height: "4K" },
    Rung { bitrate: 15_000_000, label: "15 Mbps", height: "1440p" },
    Rung { bitrate: 10_000_000, label: "10 Mbps", height: "1440p" },
    Rung { bitrate: 8_000_000, label: "8 Mbps", height: "1080p" },
    Rung { bitrate: 6_000_000, label: "6 Mbps", height: "1080p" },
    Rung { bitrate: 4_000_000, label: "4 Mbps", height: "720p" },
    Rung { bitrate: 3_000_000, label: "3 Mbps", height: "720p" },
    Rung { bitrate: 1_500_000, label: "1.5 Mbps", height: "720p" },
    Rung { bitrate: 720_000, label: "720 kbps", height: "480p" },
    Rung { bitrate: 420_000, label: "420 kbps", height: "360p" },
];

/// The rungs under the bitrate of a file: a cap above it changes nothing.
/// A file in a codec that packs more into its bits (hevc, av1, vp9) needs
/// more bits as h264, so the web client counts it one and a half times.
/// With no bitrate known, the whole ladder.
pub fn ladder(source_bitrate: Option<u64>, video_codec: Option<&str>) -> Vec<Rung> {
    let Some(bitrate) = source_bitrate.filter(|b| *b > 0) else {
        return LADDER.to_vec();
    };
    let efficient = matches!(video_codec, Some("hevc" | "h265" | "av1" | "vp9"));
    let reference = if efficient && bitrate <= 20_000_000 { bitrate * 3 / 2 } else { bitrate };
    LADDER.iter().copied().filter(|rung| rung.bitrate < reference).collect()
}

/// "1.5 Mbps" for a bitrate; the name of the rung when it is one.
pub fn bitrate_label(bitrate: u64) -> String {
    if let Some(rung) = LADDER.iter().find(|r| r.bitrate == bitrate) {
        return rung.label.to_string();
    }
    if bitrate >= 1_000_000 {
        format!("{:.1} Mbps", bitrate as f64 / 1_000_000.)
    } else {
        format!("{} kbps", bitrate / 1000)
    }
}

// ----- what the app holds ----------------------------------------------------

/// The bitrate the user allows, in bits per second; 0 is no limit ("Auto").
/// A global, because the SyncPlay session loads items without the app.
static MAX_BITRATE: AtomicU64 = AtomicU64::new(0);

pub fn max_bitrate() -> Option<u64> {
    match MAX_BITRATE.load(Ordering::Relaxed) {
        0 => None,
        bitrate => Some(bitrate),
    }
}

/// The last answer of the server, for the player and for the end of the
/// app (a transcode left on the server would run on).
static CURRENT: Mutex<Option<Arc<Resolved>>> = Mutex::new(None);

/// What plays now, as the server resolved it; none before the first load.
pub fn current() -> Option<Arc<Resolved>> {
    CURRENT.lock().unwrap().clone()
}

/// True while the item in the player is a transcode: the server chose
/// its tracks, so the player must not choose them again.
pub fn transcoding() -> bool {
    current().is_some_and(|r| r.play_method == PlayMethod::Transcode)
}

/// What the detail page chose for the next item.
#[derive(Default)]
pub struct State {
    /// Place of the audio track among the audio streams, from 0.
    pub wanted_audio: Option<usize>,
    /// The same for the subtitle; `Some(None)` is none.
    pub wanted_subtitle: Option<Option<usize>>,
}

/// Takes the settings of the config and ends a transcode when the app
/// quits. The subscription must live as long as the app.
pub fn install(config: &crate::config::Config, cx: &mut gpui_kit::App) -> gpui_kit::Subscription {
    MAX_BITRATE.store(config.max_bitrate.unwrap_or(0), Ordering::Relaxed);
    crate::adaptive::install(config);
    cx.on_app_quit(|_| {
        end_current();
        async {}
    })
}

/// Ends the transcode of the item that plays, on the server. Blocks for
/// the request; it is for the end of the app.
pub fn end_current() {
    let Some(resolved) = current() else { return };
    if resolved.play_method != PlayMethod::Transcode {
        return;
    }
    let id = &resolved.play_session_id;
    match resolved.client.stop_encoding(id) {
        Ok(()) => log::info!("ended the transcode of session {} at quit", &id[..id.len().min(8)]),
        Err(err) => log::warn!("stop encoding at quit: {err:#}"),
    }
}

// ----- the answer of the server ------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayMethod {
    DirectPlay,
    DirectStream,
    Transcode,
}

impl PlayMethod {
    /// The name the server uses in playback reports.
    pub fn as_str(self) -> &'static str {
        match self {
            PlayMethod::DirectPlay => "DirectPlay",
            PlayMethod::DirectStream => "DirectStream",
            PlayMethod::Transcode => "Transcode",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PlayMethod::DirectPlay => "Direct play",
            PlayMethod::DirectStream => "Direct stream",
            PlayMethod::Transcode => "Transcoding",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackInfo {
    #[serde(default)]
    pub media_sources: Vec<Source>,
    #[serde(default)]
    pub play_session_id: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Source {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub supports_direct_play: bool,
    #[serde(default)]
    pub supports_direct_stream: bool,
    #[serde(default)]
    pub supports_transcoding: bool,
    /// Relative to the server, with the token in its query.
    #[serde(default)]
    pub transcoding_url: Option<String>,
    #[serde(default)]
    pub transcoding_container: Option<String>,
    #[serde(default)]
    pub transcoding_sub_protocol: Option<String>,
    #[serde(default)]
    pub bitrate: Option<u64>,
    #[serde(default)]
    pub media_streams: Vec<Stream>,
    #[serde(default)]
    pub default_audio_stream_index: Option<i64>,
    #[serde(default)]
    pub default_subtitle_stream_index: Option<i64>,
}

/// One stream of the file, as the server describes it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Stream {
    /// "Video", "Audio" or "Subtitle".
    #[serde(rename = "Type", default)]
    pub kind: String,
    #[serde(default)]
    pub index: i64,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub display_title: Option<String>,
    #[serde(default)]
    pub is_external: bool,
    /// How a subtitle gets to the player: "Embed" (in the stream),
    /// "External" (a file at `delivery_url`), "Encode" (burned in).
    #[serde(default)]
    pub delivery_method: Option<String>,
    #[serde(default)]
    pub delivery_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

impl Stream {
    pub fn title(&self) -> String {
        self.display_title
            .clone()
            .unwrap_or_else(|| format!("Track {}", self.index))
    }
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

/// A subtitle in a file of its own that mpv adds to the item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalSub {
    /// Index of the stream on the server.
    pub index: i64,
    pub url: String,
    pub title: String,
    pub language: String,
}

/// The decision of the server for one load.
#[derive(Clone)]
pub struct Resolved {
    pub client: Client,
    pub url: String,
    pub play_method: PlayMethod,
    pub play_session_id: String,
    pub media_source_id: String,
    /// Why the server transcodes, as it names them
    /// ("ContainerBitrateExceedsLimit").
    pub reasons: Vec<String>,
    /// Bits per second of the file.
    pub source_bitrate: Option<u64>,
    pub target: Option<Target>,
    pub streams: Vec<Stream>,
    /// The audio stream in a transcode (server index); none for direct play.
    pub audio_index: Option<i64>,
    /// The subtitle burned into a transcode (server index); none otherwise.
    pub burned_subtitle: Option<i64>,
    /// Subtitles mpv adds after the load, in the order of their ids in mpv.
    pub external_subs: Vec<ExternalSub>,
    /// The external subtitle that starts selected, by server index.
    pub selected_external: Option<i64>,
}

impl Resolved {
    /// The source video stream.
    pub fn video(&self) -> Option<&Stream> {
        self.streams.iter().find(|s| s.kind == "Video")
    }

    /// The streams of one kind that are in the file itself, in the order
    /// mpv numbers them (from 1).
    pub fn embedded(&self, kind: &str) -> Vec<&Stream> {
        self.streams
            .iter()
            .filter(|s| s.kind == kind && !s.is_external)
            .collect()
    }

    /// The server index of the audio track mpv plays as its track `aid`.
    pub fn audio_of(&self, aid: i64) -> Option<i64> {
        self.embedded("Audio")
            .get((aid - 1).max(0) as usize)
            .map(|s| s.index)
    }

    /// The server index of the subtitle mpv shows as its track `sid`: the
    /// ones in the file come first, then the ones added from files.
    pub fn subtitle_of(&self, sid: i64) -> Option<i64> {
        let embedded = if self.play_method == PlayMethod::Transcode {
            Vec::new()
        } else {
            self.embedded("Subtitle")
        };
        let place = (sid - 1).max(0) as usize;
        if let Some(stream) = embedded.get(place) {
            return Some(stream.index);
        }
        self.external_subs
            .get(place - embedded.len())
            .map(|s| s.index)
    }

    /// The id mpv gives an external subtitle, by server index.
    pub fn sid_of_external(&self, index: i64) -> Option<i64> {
        let embedded = if self.play_method == PlayMethod::Transcode {
            0
        } else {
            self.embedded("Subtitle").len()
        };
        self.external_subs
            .iter()
            .position(|s| s.index == index)
            .map(|place| (embedded + place) as i64 + 1)
    }
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
}

/// The limit sent for "Auto": above every file. The server still applies
/// its own limit for a client outside its network.
const NO_LIMIT: u64 = 1_000_000_000;

/// What mpv plays: every container and codec (the profile names none, so
/// the server matches them all), and for a transcode h264 with aac or
/// ac3 in MPEG-TS segments over HLS, as the web client asks for on
/// macOS. Text subtitles can come as files; the server burns in the rest.
pub fn device_profile(max_bitrate: Option<u64>) -> serde_json::Value {
    const TEXT: [&str; 4] = ["srt", "ass", "ssa", "vtt"];
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
    let mut profile = serde_json::json!({
        "Name": crate::config::APP_NAME,
        "MaxStaticBitrate": 120_000_000u64,
        "MusicStreamingTranscodingBitrate": 384_000u64,
        "DirectPlayProfiles": [
            { "Type": "Video" },
            { "Type": "Audio" }
        ],
        "TranscodingProfiles": [{
            "Container": "ts",
            "Type": "Video",
            "VideoCodec": "h264",
            "AudioCodec": "aac,ac3,eac3,mp3",
            "Context": "Streaming",
            "Protocol": "hls",
            "MaxAudioChannels": "6",
            "MinSegments": "2",
            "BreakOnNonKeyFrames": true
        }],
        "CodecProfiles": [],
        "ContainerProfiles": [],
        "SubtitleProfiles": subtitles,
    });
    // "Auto" must name a limit too: without one the server takes 8 Mbps,
    // and every file above that is transcoded.
    profile["MaxStreamingBitrate"] = serde_json::Value::from(max_bitrate.unwrap_or(NO_LIMIT));
    profile
}

/// The body of the `PlaybackInfo` request.
fn request_body(client: &Client, load: &Load, max_bitrate: Option<u64>) -> serde_json::Value {
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
        "AlwaysBurnInSubtitleWhenTranscoding": false,
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

impl Client {
    /// Asks the server how to play an item.
    pub fn playback_info(&self, load: &Load, max_bitrate: Option<u64>) -> Result<PlaybackInfo> {
        let path = format!("/Items/{}/PlaybackInfo", load.item_id);
        let mut body = self.post(&path, &request_body(self, load, max_bitrate))?;
        body.read_json().context("decode PlaybackInfo")
    }

    /// Ends the transcode of a play session on the server.
    pub fn stop_encoding(&self, play_session_id: &str) -> Result<()> {
        self.call(
            "DELETE",
            "/Videos/ActiveEncodings",
            &[
                ("deviceId", self.device_id.to_string()),
                ("playSessionId", play_session_id.to_string()),
            ],
        )
    }

    /// The audio language the user prefers, when the user does not play
    /// the default track of the file whatever its language.
    fn audio_language(&self) -> Option<String> {
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Me {
            #[serde(default)]
            configuration: Audio,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Audio {
            #[serde(default)]
            audio_language_preference: Option<String>,
            #[serde(default)]
            play_default_audio_track: bool,
        }
        let audio = self.get::<Me>("/Users/Me", &[]).ok()?.configuration;
        audio
            .audio_language_preference
            .filter(|language| !language.is_empty() && !audio.play_default_audio_track)
    }
}

/// The value of one name in the query of a URL.
fn query_value<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    let (_, query) = url.split_once('?')?;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

/// Turns the answer of the server into what the player loads. `wanted`
/// is the subtitle the user chose, as a server index, when there is one.
pub fn resolve(
    client: &Client,
    item_id: &str,
    info: &PlaybackInfo,
    wanted_subtitle: Option<i64>,
) -> Result<Resolved> {
    if let Some(code) = &info.error_code {
        return Err(anyhow!("the server cannot play this item: {code}"));
    }
    let source = info
        .media_sources
        .iter()
        .find(|s| s.id == item_id)
        .or_else(|| info.media_sources.first())
        .ok_or_else(|| anyhow!("the server has no media source for this item"))?;
    let play_session_id = info
        .play_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let (url, play_method) = if source.supports_direct_play {
        (client.stream_url(item_id, &source.id), PlayMethod::DirectPlay)
    } else if let Some(path) = source
        .transcoding_url
        .as_deref()
        .filter(|_| source.supports_transcoding || source.supports_direct_stream)
    {
        let method = if source.supports_direct_stream {
            PlayMethod::DirectStream
        } else {
            PlayMethod::Transcode
        };
        // mpv sends the token in a header, so the key leaves the URL and
        // cannot show in the statistics of the player or in a log.
        (format!("{}{}", client.base, without_key(path)), method)
    } else {
        return Err(anyhow!("the server offers no way to play this item"));
    };

    let transcoding = play_method == PlayMethod::Transcode;
    let path = source.transcoding_url.as_deref().unwrap_or_default();
    let reasons: Vec<String> = query_value(path, "TranscodeReasons")
        .filter(|_| transcoding)
        .map(|list| list.split(',').filter(|r| !r.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let number = |name: &str| query_value(path, name).and_then(|v| v.parse::<i64>().ok());
    let target = transcoding.then(|| Target {
        bitrate: (number("VideoBitrate").unwrap_or(0) + number("AudioBitrate").unwrap_or(0))
            .max(0) as u64,
        video_codec: query_value(path, "VideoCodec").unwrap_or("h264").to_string(),
        audio_codec: query_value(path, "AudioCodec")
            .unwrap_or_default()
            .split(',')
            .next()
            .unwrap_or_default()
            .to_string(),
        container: source
            .transcoding_container
            .clone()
            .or_else(|| query_value(path, "SegmentContainer").map(str::to_string))
            .unwrap_or_default(),
        protocol: source.transcoding_sub_protocol.clone().unwrap_or_default(),
    });
    let audio_index = transcoding
        .then(|| number("AudioStreamIndex").or(source.default_audio_stream_index))
        .flatten();
    let burned_subtitle = transcoding.then(|| number("SubtitleStreamIndex")).flatten();

    // The subtitle that starts on: the one the user chose, else the one
    // the server names for the user.
    let chosen = wanted_subtitle
        .or(source.default_subtitle_stream_index)
        .filter(|i| *i >= 0);
    let external_subs: Vec<ExternalSub> = source
        .media_streams
        .iter()
        .filter(|s| s.kind == "Subtitle" && s.delivery_method.as_deref() == Some("External"))
        .filter_map(|s| {
            let url = s.delivery_url.as_deref()?;
            Some(ExternalSub {
                index: s.index,
                url: format!("{}{url}", client.base),
                title: s.title(),
                language: s.language.clone().unwrap_or_default(),
            })
        })
        .collect();
    let selected_external = chosen.filter(|i| external_subs.iter().any(|s| s.index == *i));

    Ok(Resolved {
        client: client.clone(),
        url,
        play_method,
        play_session_id,
        media_source_id: source.id.clone(),
        reasons,
        source_bitrate: source.bitrate,
        target,
        streams: source.media_streams.clone(),
        audio_index,
        burned_subtitle,
        external_subs,
        selected_external,
    })
}

/// Asks the server and resolves. For a transcode without a choice of the
/// user, a track in the audio language of the user takes the place of the
/// one the server names: the server can name the track of an earlier
/// playback instead (the rule of the timeline, see `timeline_info`).
pub fn negotiate(client: &Client, load: &Load) -> Result<Resolved> {
    let mut max_bitrate = max_bitrate();
    let mut info = client.playback_info(load, max_bitrate)?;
    // "Auto": the first answer names the bitrate of the file; when the
    // connection does not carry it, the server is asked again with the
    // rung that fits (see `adaptive`). A manual limit skips this.
    if max_bitrate.is_none() {
        let source = info
            .media_sources
            .iter()
            .find(|s| s.id == load.item_id)
            .or_else(|| info.media_sources.first());
        let file = source.and_then(|s| s.bitrate);
        let codec = source.and_then(|s| {
            s.media_streams.iter().find(|m| m.kind == "Video").and_then(|m| m.codec.clone())
        });
        if let Some(cap) = crate::adaptive::auto_cap(file, codec.as_deref()) {
            max_bitrate = Some(cap);
            info = client.playback_info(load, max_bitrate)?;
        }
    }
    let resolved = resolve(client, &load.item_id, &info, load.subtitle)?;
    if resolved.play_method != PlayMethod::Transcode || load.audio.is_some() {
        return Ok(resolved);
    }
    let Some(language) = client.audio_language() else {
        return Ok(resolved);
    };
    let preferred = resolved
        .embedded("Audio")
        .iter()
        .find(|s| s.language.as_deref() == Some(language.as_str()))
        .map(|s| s.index);
    match preferred {
        Some(index) if Some(index) != resolved.audio_index => {
            let again = Load { audio: Some(index), ..load.clone() };
            let info = client.playback_info(&again, max_bitrate)?;
            resolve(client, &load.item_id, &info, load.subtitle)
        }
        _ => Ok(resolved),
    }
}

/// A URL path without its `ApiKey` (or `api_key`) query value.
fn without_key(path: &str) -> String {
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

/// Starts an item in the player: the player shows it as opening at once,
/// the server is asked on a thread of its own, and the player loads what
/// the server answered. A load that comes later wins over this one.
pub fn start(player: &Player, client: Client, load: Load) {
    let generation = player.prepare(&load.title, load.start_secs, load.paused);
    // A downloaded item plays from its file on this Mac, with no question
    // to the server, so it also plays offline.
    if let Some(path) = crate::downloads::local_source(player, client.server_id.as_deref(), &load.item_id) {
        *CURRENT.lock().unwrap() = None;
        player.play_prepared(
            generation,
            PlayRequest {
                client,
                item_id: load.item_id,
                url: path,
                title: load.title,
                start_secs: load.start_secs,
                paused: load.paused,
                token: load.token,
                play_session_id: None,
                play_method: PlayMethod::DirectPlay.as_str().to_string(),
                media_source_id: String::new(),
                subtitles: Vec::new(),
            },
        );
        return;
    }
    let player = player.clone();
    let _ = std::thread::Builder::new()
        .name("playback-info".into())
        .spawn(move || match negotiate(&client, &load) {
            Ok(resolved) => {
                log::info!(
                    "play {}: {} ({})",
                    load.item_id,
                    resolved.play_method.as_str(),
                    resolved.reasons.join(",")
                );
                let resolved = Arc::new(resolved);
                let subtitles = resolved
                    .external_subs
                    .iter()
                    .map(|sub| SubtitleFile {
                        url: sub.url.clone(),
                        title: sub.title.clone(),
                        lang: sub.language.clone(),
                        select: resolved.selected_external == Some(sub.index),
                    })
                    .collect();
                let request = PlayRequest {
                    client,
                    item_id: load.item_id,
                    url: resolved.url.clone(),
                    title: load.title,
                    start_secs: load.start_secs,
                    paused: load.paused,
                    token: load.token,
                    play_session_id: Some(resolved.play_session_id.clone()),
                    play_method: resolved.play_method.as_str().to_string(),
                    media_source_id: resolved.media_source_id.clone(),
                    subtitles,
                };
                hand_over(&player, generation, resolved, request);
            }
            Err(err) => {
                log::warn!("playback info for {}: {err:#}", load.item_id);
                player.fail_load(generation, load.token, &format!("{err:#}"));
            }
        });
}

/// Hands the answer of the server to the player and remembers it, or ends
/// a transcode that nobody will ask for because a later load came first.
fn hand_over(player: &Player, generation: u64, resolved: Arc<Resolved>, request: PlayRequest) {
    let accepted = {
        let mut current = CURRENT.lock().unwrap();
        let accepted = player.play_prepared(generation, request);
        if accepted {
            *current = Some(resolved.clone());
        }
        accepted
    };
    // The request to the server goes with `CURRENT` free: the UI reads it
    // at every poll of the player, and a slow server must not hold it.
    if !accepted && resolved.play_method == PlayMethod::Transcode {
        let _ = resolved.client.stop_encoding(&resolved.play_session_id);
    }
}

// ----- the app ---------------------------------------------------------------------

impl Bloom {
    /// Starts an item for the app: with the tracks the detail page chose,
    /// when it chose some.
    pub fn stream_begin(&mut self, item: &Item, start_secs: f64) {
        let Some(session) = &self.session else { return };
        let audio = self.stream.wanted_audio.take().and_then(|place| {
            item.streams("Audio").get(place).map(|s| s.index)
        });
        let subtitle = self.stream.wanted_subtitle.take().map(|choice| match choice {
            Some(place) => item.streams("Subtitle").get(place).map_or(-1, |s| s.index),
            None => -1,
        });
        start(
            &self.player,
            session.client.clone(),
            Load {
                item_id: item.id.clone(),
                title: item.display_title(),
                start_secs,
                paused: false,
                token: 0,
                audio,
                subtitle,
            },
        );
    }

    /// Loads the item again at its position, with the tracks that play
    /// now, after a change of the quality or of a track the server picks.
    /// `audio` and `subtitle` are server indexes that replace the ones
    /// that play.
    pub fn stream_reload(&mut self, audio: Option<i64>, subtitle: Option<i64>) {
        let (Some(item), Some(resolved), Some(session)) =
            (self.playing.as_ref(), current(), self.session.as_ref())
        else {
            return;
        };
        if self.player_status.state != PlayState::Playing {
            return;
        }
        let status = &self.player_status;
        let selected = |kind: &str| status.tracks.iter().find(|t| t.kind == kind && t.selected);
        let audio = audio.or_else(|| match resolved.play_method {
            PlayMethod::Transcode => resolved.audio_index,
            _ => selected("audio").and_then(|t| resolved.audio_of(t.id)),
        });
        let subtitle = subtitle.or_else(|| {
            resolved.burned_subtitle.or_else(|| {
                Some(selected("sub").and_then(|t| resolved.subtitle_of(t.id)).unwrap_or(-1))
            })
        });
        start(
            &self.player,
            session.client.clone(),
            Load {
                item_id: item.id.clone(),
                title: item.display_title(),
                start_secs: status.position.max(0.),
                paused: status.paused,
                token: 0,
                audio,
                subtitle,
            },
        );
    }

    /// Sets the bitrate the user allows (none is "Auto"), keeps it in the
    /// config, and reloads the item that plays.
    pub fn set_max_bitrate(&mut self, bitrate: Option<u64>, cx: &mut Context<Self>) {
        let bitrate = bitrate.filter(|b| *b > 0);
        if self.config.max_bitrate == bitrate && max_bitrate() == bitrate {
            return;
        }
        self.config.max_bitrate = bitrate;
        MAX_BITRATE.store(bitrate.unwrap_or(0), Ordering::Relaxed);
        self.save_config(cx);
        if self.player_open {
            self.stream_reload(None, None);
        }
        self.rebuild_track_menus(cx);
        cx.notify();
    }

    /// The "Quality" entry of the player settings menu: "Auto" and the
    /// rungs of the ladder under the bitrate of the file.
    pub fn quality_menu_item(&self, cx: &mut Context<Self>) -> MenuItem {
        let this = cx.weak_entity();
        let resolved = current();
        let cap = max_bitrate();
        let source = resolved.as_ref().and_then(|r| r.source_bitrate);
        let codec = resolved
            .as_ref()
            .and_then(|r| r.video().and_then(|v| v.codec.clone()));
        let mut items = Vec::new();
        let handle = this.clone();
        let auto_detail = match source {
            Some(bitrate) => format!("The file as it is · {}", bitrate_label(bitrate)),
            None => "The file as it is".to_string(),
        };
        items.push(
            MenuItem::new(
                "settings.quality.auto",
                SharedString::from(format!("Auto · {}", crate::adaptive::auto_label())),
            )
                .detail(auto_detail)
                .radio(cap.is_none())
                .on_click(move |_, _, cx| {
                    handle.update(cx, |t, cx| t.set_max_bitrate(None, cx)).ok();
                }),
        );
        for rung in ladder(source, codec.as_deref()) {
            let handle = this.clone();
            items.push(
                MenuItem::new(
                    SharedString::from(format!("settings.quality.{}", rung.bitrate)),
                    rung.label,
                )
                .detail(rung.height)
                .radio(cap == Some(rung.bitrate))
                .on_click(move |_, _, cx| {
                    handle
                        .update(cx, |t, cx| t.set_max_bitrate(Some(rung.bitrate), cx))
                        .ok();
                }),
            );
        }
        let now = match (cap, resolved.as_ref()) {
            (Some(bitrate), _) => bitrate_label(bitrate),
            (None, Some(_)) => format!("Auto · {}", crate::adaptive::auto_label()),
            (None, None) => "Auto".to_string(),
        };
        MenuItem::submenu("settings.quality", "Quality", items).detail(now)
    }

    /// The track entries of a menu while a transcode plays: the server
    /// picks the audio and burns a subtitle in, so these are its streams
    /// and a choice loads the item again. None with direct play, where
    /// mpv switches its own tracks.
    pub fn stream_track_items(&self, kind: &str, cx: &mut Context<Self>) -> Option<Vec<MenuItem>> {
        let resolved = current().filter(|r| r.play_method == PlayMethod::Transcode)?;
        let this = cx.weak_entity();
        let mut items = Vec::new();
        if kind == "audio" {
            for stream in resolved.embedded("Audio") {
                let (index, handle) = (stream.index, this.clone());
                items.push(
                    MenuItem::new(
                        SharedString::from(format!("track.audio.{index}")),
                        stream.title(),
                    )
                    .radio(resolved.audio_index == Some(index))
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |t, _| t.stream_reload(Some(index), None)).ok();
                    }),
                );
            }
        } else {
            let shown = self
                .player_status
                .tracks
                .iter()
                .find(|t| t.kind == "sub" && t.selected)
                .and_then(|t| resolved.subtitle_of(t.id))
                .or(resolved.burned_subtitle);
            let handle = this.clone();
            items.push(
                MenuItem::new("track.sub.none", "Off")
                    .radio(shown.is_none())
                    .on_click(move |_, _, cx| {
                        handle
                            .update(cx, |t, _| t.stream_subtitle(-1))
                            .ok();
                    }),
            );
            for stream in resolved.streams.iter().filter(|s| s.kind == "Subtitle") {
                let (index, handle) = (stream.index, this.clone());
                let burned = resolved.sid_of_external(index).is_none();
                items.push(
                    MenuItem::new(SharedString::from(format!("track.sub.{index}")), stream.title())
                        .detail(if burned { "Burned in" } else { "" })
                        .radio(shown == Some(index))
                        .on_click(move |_, _, cx| {
                            handle.update(cx, |t, _| t.stream_subtitle(index)).ok();
                        }),
                );
            }
        }
        if items.is_empty() {
            items.push(
                MenuItem::new(
                    SharedString::from(format!("track.{kind}.empty")),
                    "No tracks",
                )
                .disabled(true),
            );
        }
        Some(items)
    }

    /// Shows a subtitle (server index; -1 for none) while a transcode
    /// plays: a text subtitle switches in mpv, any other one is burned in
    /// by the server, so the item loads again.
    pub fn stream_subtitle(&mut self, index: i64) {
        let Some(resolved) = current() else { return };
        let burned = resolved.burned_subtitle.is_some();
        if index < 0 {
            if burned {
                self.stream_reload(None, Some(-1));
            } else {
                self.player.set_subtitle(None);
            }
            return;
        }
        match resolved.sid_of_external(index) {
            Some(sid) if !burned => self.player.set_subtitle(Some(sid)),
            _ => self.stream_reload(None, Some(index)),
        }
    }

    /// The card of the "Playback info" overlay: how the item plays.
    pub fn render_stream_info(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.playback_info || !self.player_open {
            return None;
        }
        let _ = cx;
        let resolved = current();
        let ink = rgba(0xf5f5f7ff);
        let quiet = rgba(0xf5f5f7b3);
        let line = |text: String| div().text_size(px(12.)).text_color(quiet).child(text);
        let mut lines: Vec<Div> = Vec::new();
        let title = match &resolved {
            Some(r) => r.play_method.label().to_string(),
            None => "Opening".to_string(),
        };
        if let Some(r) = &resolved {
            if !r.reasons.is_empty() {
                lines.push(line(r.reasons.join(", ")));
            }
            if let Some(video) = r.video() {
                let mut parts = Vec::new();
                if let (Some(w), Some(h)) = (video.width, video.height) {
                    parts.push(format!("{w}×{h}"));
                }
                if let Some(codec) = &video.codec {
                    parts.push(codec.to_uppercase());
                }
                if let Some(bitrate) = r.source_bitrate {
                    parts.push(bitrate_label(bitrate));
                }
                lines.push(line(format!("Source: {}", parts.join(" · "))));
            }
            if let Some(target) = &r.target {
                lines.push(line(format!(
                    "Target: {} · {}/{} · {} over {}",
                    bitrate_label(target.bitrate),
                    target.video_codec,
                    target.audio_codec,
                    target.container,
                    target.protocol
                )));
            }
        }
        let status = &self.player_status;
        if status.video_w > 0 && status.video_h > 0 {
            lines.push(line(format!("Decoding: {}×{}", status.video_w, status.video_h)));
        }
        lines.push(line(format!(
            "Limit: {}",
            max_bitrate().map_or_else(
                || format!("Auto · {}", crate::adaptive::auto_label()),
                bitrate_label
            )
        )));
        lines.push(line(format!("Connection: {}", crate::adaptive::link_line())));
        if let Some(r) = &resolved {
            lines.push(line(format!(
                "Session {}",
                &r.play_session_id[..r.play_session_id.len().min(8)]
            )));
        }
        Some(
            div()
                .absolute()
                .top(px(70.))
                .right(px(24.))
                .w(px(300.))
                .p(px(14.))
                .rounded(px(16.))
                .border_1()
                .border_color(rgba(0xf5f5f733))
                .child(glass(px(16.), rgba(0x1c1c1cd9)))
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(ink)
                        .child(title),
                )
                .children(lines),
        )
    }

    /// The `quality` debug command: `state`, `set <kbps|auto>`, `menu`,
    /// `info`.
    pub fn debug_quality(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" | "state" => {
                let status = &self.player_status;
                let cap = max_bitrate().map_or("auto".to_string(), |b| (b / 1000).to_string());
                let link = crate::adaptive::describe();
                match current() {
                    Some(r) => format!(
                        "method={} cap={cap} session={} reasons=[{}] source={} target={} decoded={}x{} \
                         audio={:?} burned={:?} external_subs={} url={} pos={:.1} paused={} {link}",
                        r.play_method.as_str(),
                        if r.play_session_id.is_empty() { "no" } else { "yes" },
                        r.reasons.join(","),
                        r.source_bitrate.unwrap_or(0),
                        r.target.as_ref().map_or(0, |t| t.bitrate),
                        status.video_w,
                        status.video_h,
                        r.audio_index,
                        r.burned_subtitle,
                        r.external_subs.len(),
                        if r.play_method == PlayMethod::DirectPlay { "static" } else { "hls" },
                        status.position,
                        status.paused,
                    ),
                    None => format!("method=none cap={cap} session=no {link}"),
                }
            }
            "set" => {
                let bitrate = match arg {
                    "auto" | "" => None,
                    kbps => match kbps.parse::<u64>() {
                        Ok(kbps) => Some(kbps * 1000),
                        Err(_) => return "error: usage: quality set <kbps|auto>".into(),
                    },
                };
                self.set_max_bitrate(bitrate, cx);
                format!("cap={}", arg)
            }
            "menu" => {
                self.rebuild_track_menus(cx);
                self.settings_menu
                    .update(cx, |menu, cx| menu.open_submenu("Quality", window, cx));
                "menu".into()
            }
            "info" => {
                self.toggle_playback_info(cx);
                format!("info={}", self.playback_info)
            }
            // Quits the app as the Quit menu does, for a test of the end
            // of a transcode at quit.
            "quit" => {
                cx.quit();
                "quit".into()
            }
            // The measurement and the stalls: see `adaptive`.
            _ => self.debug_adaptive(verb, arg, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    const DIRECT: &str = include_str!("../fixtures/playback-info-direct.json");
    const TRANSCODE: &str = include_str!("../fixtures/playback-info-transcode.json");
    const ITEM: &str = "00112233445566778899aabbccddeeff";

    fn client() -> Client {
        Client::new("https://media.example.com", "device-1").with_session("tok", "user-1")
    }

    #[test]
    fn ladder_stops_under_the_source_bitrate() {
        let rungs = ladder(Some(3_794_694), Some("h264"));
        assert_eq!(rungs.len(), 4);
        assert_eq!(rungs[0].label, "3 Mbps");
        assert_eq!(rungs[3].label, "420 kbps");
        assert_eq!(rungs[0].height, "720p");
        // An efficient codec counts one and a half times.
        let rungs = ladder(Some(3_000_000), Some("hevc"));
        assert_eq!(rungs[0].label, "4 Mbps");
        assert_eq!(ladder(None, None).len(), 14);
        assert_eq!(ladder(Some(0), None).len(), 14);
        assert_eq!(bitrate_label(1_500_000), "1.5 Mbps");
        assert_eq!(bitrate_label(3_794_694), "3.8 Mbps");
        assert_eq!(bitrate_label(640_000), "640 kbps");
    }

    #[test]
    fn profile_describes_mpv() {
        let profile = device_profile(Some(1_500_000));
        assert_eq!(profile["MaxStreamingBitrate"], 1_500_000);
        assert_eq!(profile["DirectPlayProfiles"][0]["Type"], "Video");
        let hls = &profile["TranscodingProfiles"][0];
        assert_eq!(hls["Protocol"], "hls");
        assert_eq!(hls["VideoCodec"], "h264");
        assert_eq!(hls["Container"], "ts");
        assert_eq!(hls["BreakOnNonKeyFrames"], true);
        assert!(profile["SubtitleProfiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["Format"] == "pgssub" && p["Method"] == "Embed"));
        assert_eq!(device_profile(None)["MaxStreamingBitrate"], NO_LIMIT);
        let load = Load {
            item_id: ITEM.into(),
            title: String::new(),
            start_secs: 90.,
            paused: false,
            token: 0,
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
    fn direct_play_keeps_the_static_url() {
        let info: PlaybackInfo = serde_json::from_str(DIRECT).unwrap();
        let resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert_eq!(resolved.play_method, PlayMethod::DirectPlay);
        assert_eq!(
            resolved.url,
            format!("https://media.example.com/Videos/{ITEM}/stream?static=true&mediaSourceId={ITEM}")
        );
        assert_eq!(resolved.play_session_id, "aaaaaaaabbbbccccddddeeeeffff0001");
        assert_eq!(resolved.media_source_id, ITEM);
        assert!(resolved.reasons.is_empty());
        assert!(resolved.target.is_none());
        assert_eq!(resolved.source_bitrate, Some(3_794_694));
        assert!(resolved.external_subs.is_empty());
        // mpv's first audio track is stream 1; its first subtitle is stream 2.
        assert_eq!(resolved.audio_of(1), Some(1));
        assert_eq!(resolved.subtitle_of(1), Some(2));
    }

    #[test]
    fn transcode_takes_the_hls_url_and_the_text_subtitle() {
        let info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        let resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert_eq!(resolved.play_method, PlayMethod::Transcode);
        assert!(resolved.url.starts_with("https://media.example.com/videos/"));
        assert!(resolved.url.contains("master.m3u8?"));
        assert_eq!(resolved.play_session_id, "aaaaaaaabbbbccccddddeeeeffff0002");
        assert_eq!(resolved.reasons, vec!["ContainerBitrateExceedsLimit"]);
        let target = resolved.target.as_ref().unwrap();
        assert_eq!(target.bitrate, 1_116_000 + 384_000);
        assert_eq!((target.video_codec.as_str(), target.audio_codec.as_str()), ("h264", "aac"));
        assert_eq!((target.container.as_str(), target.protocol.as_str()), ("ts", "hls"));
        assert_eq!(resolved.audio_index, Some(1));
        assert_eq!(resolved.burned_subtitle, None);
        // The text subtitle comes as a file and starts on: the server
        // names it as the default.
        assert_eq!(resolved.external_subs.len(), 1);
        assert_eq!(resolved.external_subs[0].index, 2);
        assert!(resolved.external_subs[0].url.starts_with("https://media.example.com/Videos/"));
        assert_eq!(resolved.selected_external, Some(2));
        assert_eq!(resolved.sid_of_external(2), Some(1));
        assert_eq!(resolved.subtitle_of(1), Some(2));
        // The user chose no subtitle.
        let none = resolve(&client(), ITEM, &info, Some(-1)).unwrap();
        assert_eq!(none.selected_external, None);
    }

    /// Finding 9 of the review: the URL of a direct play names the media
    /// source the server resolved, also when its id is not the item id.
    #[test]
    fn direct_play_names_the_resolved_source() {
        let mut info: PlaybackInfo = serde_json::from_str(DIRECT).unwrap();
        info.media_sources[0].id = "ffeeddccbbaa99887766554433221100".into();
        let resolved = resolve(&client(), ITEM, &info, None).unwrap();
        assert_eq!(resolved.media_source_id, "ffeeddccbbaa99887766554433221100");
        assert_eq!(
            resolved.url,
            format!("https://media.example.com/Videos/{ITEM}/stream?static=true&mediaSourceId=ffeeddccbbaa99887766554433221100")
        );
    }

    /// Finding 4 of the review: ending the transcode of a load that lost to
    /// a later one must not hold `CURRENT`, which the UI reads at each poll.
    #[test]
    fn a_stale_transcode_ends_without_holding_current() {
        use std::io::{Read as _, Write as _};
        // A server that says when the DELETE reached it, and answers it
        // 1.5 s later.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (arrived_tx, arrived) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = socket.read(&mut buf).unwrap_or(0);
            let _ = arrived_tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            std::thread::sleep(Duration::from_millis(1500));
            let _ = socket.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        });
        let client = Client::new(&format!("http://127.0.0.1:{port}"), "device-1").with_session("tok", "user-1");
        let info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        let resolved = Arc::new(resolve(&client, ITEM, &info, None).unwrap());
        assert_eq!(resolved.play_method, PlayMethod::Transcode);
        let player = Player::default();
        let stale = player.prepare("first", 0., false);
        let _later = player.prepare("second", 0., false);
        let request = PlayRequest {
            client: client.clone(),
            item_id: ITEM.into(),
            url: resolved.url.clone(),
            title: "first".into(),
            start_secs: 0.,
            paused: false,
            token: 1,
            play_session_id: Some(resolved.play_session_id.clone()),
            play_method: "Transcode".into(),
            media_source_id: ITEM.into(),
            subtitles: Vec::new(),
        };
        let installer = {
            let player = player.clone();
            std::thread::spawn(move || hand_over(&player, stale, resolved, request))
        };
        // The DELETE is in flight (the server has it and waits), and the
        // UI asks what plays.
        let request_line = arrived.recv_timeout(Duration::from_secs(5)).expect("the DELETE reached the server");
        assert!(request_line.starts_with("DELETE "), "not a DELETE: {request_line}");
        let started = Instant::now();
        let _ = current();
        let waited = started.elapsed();
        installer.join().unwrap();
        server.join().unwrap();
        assert!(waited < Duration::from_millis(500), "current() waited {waited:?} for the server");
    }

    #[test]
    fn errors_of_the_server_are_errors() {
        let info: PlaybackInfo =
            serde_json::from_str(r#"{"MediaSources":[],"ErrorCode":"NoCompatibleStream"}"#).unwrap();
        assert!(resolve(&client(), ITEM, &info, None).is_err());
        let mut info: PlaybackInfo = serde_json::from_str(TRANSCODE).unwrap();
        info.media_sources[0].supports_transcoding = false;
        assert!(resolve(&client(), ITEM, &info, None).is_err());
    }
}
