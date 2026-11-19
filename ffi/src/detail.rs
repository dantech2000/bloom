//! The file of a film or an episode, for the detail page's "File" section
//! and the friendly names of its tracks: the first media source as the
//! server describes it (resolution, range, codecs, channels, size, and the
//! flags of each track). Read with its own small struct, so the fields the
//! core's `MediaStream` does not keep (forced, hearing impaired, size)
//! come through without a change to the core.

use serde::Deserialize;

use crate::{BloomError, Server, TrackKind, run};

/// The video of a file.
#[derive(uniffi::Record, Clone)]
pub struct VideoInfo {
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// "hevc", "h264", "av1"…
    pub codec: Option<String>,
    /// "SDR", "HDR10", "HDR10Plus", "HLG", "DOVI" and its variants.
    pub range_type: Option<String>,
}

/// An audio or subtitle track with the flags its friendly name needs.
#[derive(uniffi::Record, Clone)]
pub struct TrackInfo {
    pub index: i64,
    pub kind: TrackKind,
    /// Three-letter code, such as "eng".
    pub language: Option<String>,
    pub codec: Option<String>,
    /// The title in the file ("Commentary", "SDH"), when it has one.
    pub title: Option<String>,
    /// The server's composed title, as `streams` gives it.
    pub display_title: Option<String>,
    pub channels: Option<i32>,
    pub channel_layout: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
    pub is_external: bool,
    /// A text subtitle (SubRip, ASS, VTT); false for an image track.
    pub is_text: bool,
}

#[derive(uniffi::Record, Clone)]
pub struct MediaFile {
    /// "mkv", "mp4"…
    pub container: Option<String>,
    pub size_bytes: Option<u64>,
    /// Bits per second.
    pub bitrate: Option<u64>,
    pub video: Option<VideoInfo>,
    pub tracks: Vec<TrackInfo>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct Stream {
    #[serde(rename = "Type")]
    kind: String,
    index: i64,
    language: Option<String>,
    codec: Option<String>,
    title: Option<String>,
    display_title: Option<String>,
    channels: Option<i32>,
    channel_layout: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    video_range_type: Option<String>,
    is_default: bool,
    is_forced: bool,
    is_hearing_impaired: bool,
    is_external: bool,
    is_text_subtitle_stream: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct Source {
    container: Option<String>,
    size: Option<u64>,
    bitrate: Option<u64>,
    media_streams: Vec<Stream>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct Answer {
    media_sources: Vec<Source>,
}

fn file(source: Source) -> MediaFile {
    let video = source.media_streams.iter().find(|s| s.kind == "Video").map(|s| VideoInfo {
        width: s.width,
        height: s.height,
        codec: s.codec.clone(),
        range_type: s.video_range_type.clone(),
    });
    let tracks = source
        .media_streams
        .into_iter()
        .filter_map(|s| {
            let kind = match s.kind.as_str() {
                "Audio" => TrackKind::Audio,
                "Subtitle" => TrackKind::Subtitle,
                _ => return None,
            };
            Some(TrackInfo {
                index: s.index,
                kind,
                language: s.language,
                codec: s.codec,
                title: s.title,
                display_title: s.display_title,
                channels: s.channels,
                channel_layout: s.channel_layout,
                is_default: s.is_default,
                is_forced: s.is_forced,
                is_hearing_impaired: s.is_hearing_impaired,
                is_external: s.is_external,
                is_text: s.is_text_subtitle_stream,
            })
        })
        .collect();
    MediaFile { container: source.container, size_bytes: source.size, bitrate: source.bitrate, video, tracks }
}

#[uniffi::export]
impl Server {
    /// The item's first media source; none for an item without a file.
    pub async fn media_file(&self, item_id: String) -> Result<Option<MediaFile>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let user = client.user()?.to_string();
            let answer: Answer = client.get(&format!("/Items/{item_id}"), &[("userId", user)])?;
            Ok(answer.media_sources.into_iter().next().map(file))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_flags_and_the_video() {
        let answer: Answer = serde_json::from_str(
            r#"{"MediaSources":[{"Container":"mkv","Size":19700000000,"MediaStreams":[
                {"Type":"Video","Index":0,"Codec":"hevc","Width":3840,"Height":2160,"VideoRangeType":"DOVIWithHDR10"},
                {"Type":"Audio","Index":1,"Codec":"eac3","Language":"eng","Channels":6,"IsDefault":true},
                {"Type":"Subtitle","Index":2,"Codec":"PGSSUB","Language":"eng","IsForced":true},
                {"Type":"Subtitle","Index":3,"Codec":"subrip","Language":"eng","IsHearingImpaired":true,"IsTextSubtitleStream":true}
            ]}]}"#,
        )
        .unwrap();
        let file = file(answer.media_sources.into_iter().next().unwrap());
        assert_eq!(file.size_bytes, Some(19_700_000_000));
        let video = file.video.unwrap();
        assert_eq!((video.width, video.height), (Some(3840), Some(2160)));
        assert_eq!(file.tracks.len(), 3);
        assert!(file.tracks[1].is_forced && !file.tracks[1].is_text);
        assert!(file.tracks[2].is_hearing_impaired && file.tracks[2].is_text);
    }
}
