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

/// One version of a film or an episode (a media source of the item): its
/// file, with a name to tell it from the others.
#[derive(uniffi::Record, Clone)]
pub struct MediaVersion {
    /// The media source id, which `play_info` and `download_info` take.
    pub id: String,
    /// The server's name of the version, else its file in short
    /// ("4K · HEVC · 58 GB").
    pub name: String,
    /// The file in short, when the name is not that already.
    pub detail: Option<String>,
    /// The item's own file: what a play without a choice gets.
    pub is_default: bool,
    pub file: MediaFile,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct Source {
    id: String,
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

/// The versions of an item from its answer, in the order of a menu (the
/// item's own file first; core's `Item::versions`).
fn versions(value: serde_json::Value) -> Result<Vec<MediaVersion>, BloomError> {
    let item: bloom_core::jellyfin::Item = serde_json::from_value(value.clone()).map_err(|e| BloomError::Answered { detail: e.to_string() })?;
    let answer: Answer = serde_json::from_value(value).map_err(|e| BloomError::Answered { detail: e.to_string() })?;
    let mut files: Vec<Source> = answer.media_sources;
    Ok(item
        .versions()
        .iter()
        .enumerate()
        .filter_map(|(n, source)| {
            let at = files.iter().position(|f| f.id == source.id)?;
            let name = bloom_core::versions::name(source, n);
            let technical = bloom_core::versions::technical(source);
            Some(MediaVersion {
                id: source.id.clone(),
                detail: (!technical.is_empty() && technical != name).then_some(technical),
                name,
                is_default: source.id == item.id,
                file: file(files.remove(at)),
            })
        })
        .collect())
}

#[uniffi::export]
impl Server {
    /// The file a play without a choice gets (the item's own, else the
    /// first); none for an item without a file.
    pub async fn media_file(&self, item_id: String) -> Result<Option<MediaFile>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let user = client.user()?.to_string();
            let answer: Answer = client.get(&format!("/Items/{item_id}"), &[("userId", user)])?;
            let mut sources = answer.media_sources;
            let at = sources.iter().position(|s| s.id == item_id).unwrap_or(0);
            Ok((at < sources.len()).then(|| file(sources.remove(at))))
        })
        .await
    }

    /// The versions of a film or an episode (Jellyfin 12 has them for
    /// episodes too), the item's own file first; one for an item with one
    /// file, none for an item without a file.
    pub async fn media_versions(&self, item_id: String) -> Result<Vec<MediaVersion>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let user = client.user()?.to_string();
            let value: serde_json::Value = client.get(&format!("/Items/{item_id}"), &[("userId", user)])?;
            versions(value)
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

    /// Two files: the item's own first, each with its own tracks and a
    /// name (the server's, else the file in short).
    #[test]
    fn lists_the_versions_own_file_first() {
        let value = serde_json::json!({
            "Id": "film", "Type": "Movie",
            "MediaSources": [
                { "Id": "v4k", "Size": 58_000_000_000u64, "MediaStreams": [
                    { "Type": "Video", "Index": 0, "Codec": "hevc", "Width": 3840, "Height": 2160 },
                    { "Type": "Audio", "Index": 1, "Codec": "truehd", "Language": "eng" },
                    { "Type": "Audio", "Index": 2, "Codec": "ac3", "Language": "spa" } ] },
                { "Id": "film", "Name": "1080p", "Size": 6_300_000_000u64, "MediaStreams": [
                    { "Type": "Video", "Index": 0, "Codec": "h264", "Width": 1920, "Height": 1080 },
                    { "Type": "Audio", "Index": 1, "Codec": "aac", "Language": "eng" } ] }
            ]
        });
        let versions = versions(value).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!((versions[0].id.as_str(), versions[0].name.as_str(), versions[0].is_default), ("film", "1080p", true));
        assert_eq!(versions[0].detail.as_deref(), Some("1080p · H264 · 6.3 GB"));
        assert_eq!(versions[1].name, "4K · HEVC · 58 GB");
        assert_eq!(versions[1].detail, None);
        assert_eq!(versions[1].file.tracks.len(), 2);
        assert_eq!(versions[0].file.tracks.len(), 1);
    }
}
