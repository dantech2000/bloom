// SPDX-License-Identifier: AGPL-3.0-or-later
//! Quality tags of the Jellyfin Enhanced plugin: resolution, source, dynamic
//! range, special format, video codec and audio. The rules are those of
//! `js/tags/qualitytags.js` of the plugin (tag 12.9.0.0), `getEnhancedQuality`
//! and `insertOverlay`, so the app shows the tags the web client shows.
//!
//! The plugin derives every tag from fields of the media streams, the names
//! and the file path. It does not derive REMUX or WEB-DL. The source tag is
//! only for a media stub (a `.disc` file): BluRay, HD DVD, DVD, VHS, HDTV or
//! Physical.

use std::{collections::HashMap, sync::Mutex};

use crate::jellyfin::{Item, MediaSource, MediaStream};

/// The six kinds of tag, in the default stack order of the plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Resolution,
    Source,
    DynamicRange,
    SpecialFormat,
    VideoCodec,
    Audio,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::Resolution,
        Category::Source,
        Category::DynamicRange,
        Category::SpecialFormat,
        Category::VideoCodec,
        Category::Audio,
    ];

    /// Place in [`Category::ALL`]; also the default stack order minus one.
    fn index(self) -> usize {
        self as usize
    }

    /// Name of the switch in the settings of the plugin.
    fn setting(self) -> &'static str {
        match self {
            Category::Resolution => "ShowResolutionTag",
            Category::Source => "ShowSourceTag",
            Category::DynamicRange => "ShowDynamicRangeTag",
            Category::SpecialFormat => "ShowSpecialFormatTag",
            Category::VideoCodec => "ShowVideoCodecTag",
            Category::Audio => "ShowAudioInfoTag",
        }
    }

    fn order_setting(self) -> &'static str {
        match self {
            Category::Resolution => "ResolutionTagOrder",
            Category::Source => "SourceTagOrder",
            Category::DynamicRange => "DynamicRangeTagOrder",
            Category::SpecialFormat => "SpecialFormatTagOrder",
            Category::VideoCodec => "VideoCodecTagOrder",
            Category::Audio => "AudioInfoTagOrder",
        }
    }

    /// The tags of the category, the more important first.
    fn items(self) -> &'static [&'static str] {
        match self {
            Category::Resolution => &[
                "8K", "4K", "1440p", "1080p", "720p", "576p", "480p", "LOW-RES", "SD",
            ],
            Category::Source => &["BluRay", "HD DVD", "DVD", "VHS", "HDTV", "Physical"],
            Category::DynamicRange => &["Dolby Vision", "HDR10+", "HDR10", "HDR"],
            Category::SpecialFormat => &["IMAX", "3D"],
            Category::VideoCodec => &[
                "AV1", "HEVC", "H265", "VP9", "H264", "VP8", "XVID", "DIVX", "WMV", "MPEG2",
                "MPEG4", "MJPEG", "THEORA",
            ],
            Category::Audio => &["ATMOS", "DTS-X", "TRUEHD", "DTS", "Dolby Digital+", "7.1", "5.1"],
        }
    }
}

/// The choices of the user: which categories show, and their stack order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    pub show: [bool; 6],
    pub order: [i64; 6],
}

impl Default for Prefs {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Prefs {
    /// True when the tags of a category show.
    pub fn shows(&self, category: Category) -> bool {
        self.show[category.index()]
    }

    /// Every category on, in the stack order of the plugin.
    const DEFAULT: Prefs = Prefs { show: [true; 6], order: [1, 2, 3, 4, 5, 6] };

    /// Reads the choices: the user's value, else the server default, else
    /// "on" and the default order (as `JE.loadSettings` does).
    pub fn from_settings(own: &serde_json::Value, public: &serde_json::Value) -> Self {
        let mut prefs = Self::default();
        for category in Category::ALL {
            let i = category.index();
            let pick = |name: &str| {
                own.get(name)
                    .filter(|v| !v.is_null())
                    .or_else(|| public.get(name).filter(|v| !v.is_null()))
            };
            if let Some(on) = pick(category.setting()).and_then(|v| v.as_bool()) {
                prefs.show[i] = on;
            }
            if let Some(order) = pick(category.order_setting()).and_then(|v| v.as_i64()) {
                prefs.order[i] = order;
            }
        }
        prefs
    }
}

/// The choices in force, and the categories the debug channel switches on
/// although the plugin has them off. A static, as the master switch in
/// `jellyfin.rs`: a card reads it while it is built.
static CHOICES: Mutex<(Prefs, [bool; 6])> = Mutex::new((Prefs::DEFAULT, [false; 6]));

pub fn set_prefs(prefs: Prefs) {
    CHOICES.lock().unwrap().0 = prefs;
}

pub fn prefs() -> Prefs {
    let (mut prefs, forced) = *CHOICES.lock().unwrap();
    for (show, forced) in prefs.show.iter_mut().zip(forced) {
        *show |= forced;
    }
    prefs
}

/// For the debug channel: shows the categories named in a list such as
/// "codec,audio" regardless of the plugin setting. An empty list clears it.
pub fn force_categories(names: &str) -> Result<(), String> {
    let mut forced = [false; 6];
    for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        let category = match name {
            "resolution" => Category::Resolution,
            "source" => Category::Source,
            "range" => Category::DynamicRange,
            "special" => Category::SpecialFormat,
            "codec" => Category::VideoCodec,
            "audio" => Category::Audio,
            "all" => {
                forced = [true; 6];
                continue;
            }
            other => return Err(format!("no tag category {other:?}")),
        };
        forced[category.index()] = true;
    }
    CHOICES.lock().unwrap().1 = forced;
    Ok(())
}

/// The master switch of the tags: the value of the user, else the server
/// default, else off.
pub fn master_switch(own: &serde_json::Value, public: &serde_json::Value) -> bool {
    let pick = |settings: &serde_json::Value| settings.get("QualityTagsEnabled").and_then(|v| v.as_bool());
    pick(own).or_else(|| pick(public)).unwrap_or(false)
}

// ----- the rules of the plugin -------------------------------------------------

/// A character of a regex word, for `\b`.
fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True when `text` has one of `needles` as a whole word (`\bneedle\b`). A
/// needle may hold a separator, such as "7.1". Case is ignored.
fn has_word(text: &str, needles: &[&str]) -> bool {
    let text = text.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    needles.iter().any(|needle| {
        let needle: Vec<char> = needle.to_lowercase().chars().collect();
        if needle.is_empty() || needle.len() > chars.len() {
            return false;
        }
        (0..=chars.len() - needle.len()).any(|start| {
            chars[start..start + needle.len()] == needle[..]
                && (start == 0 || !word(chars[start - 1]))
                && chars.get(start + needle.len()).is_none_or(|c| !word(*c))
        })
    })
}

/// The first word of `text` that is one of `words`, ignoring case.
fn first_word<'a>(text: &str, words: &[&'a str]) -> Option<&'a str> {
    text.split(|c: char| !word(c))
        .find_map(|token| words.iter().copied().find(|w| w.eq_ignore_ascii_case(token)))
}

fn lower(text: &Option<String>) -> String {
    text.as_deref().unwrap_or("").to_lowercase()
}

/// Everything the detection reads of one item.
#[derive(Default)]
pub struct Input<'a> {
    pub streams: Vec<&'a MediaStream>,
    pub sources: &'a [MediaSource],
    pub name: &'a str,
    pub original_title: &'a str,
}

impl<'a> Input<'a> {
    pub fn of(item: &'a Item) -> Self {
        // The plugin reads the streams of the item and those of the first
        // source; the first video stream is the one it analyses.
        let mut streams: Vec<&MediaStream> = item.media_streams.iter().collect();
        if let Some(source) = item.media_sources.first() {
            streams.extend(source.media_streams.iter());
        }
        Self {
            streams,
            sources: &item.media_sources,
            name: &item.name,
            original_title: item.original_title.as_deref().unwrap_or(""),
        }
    }
}

fn resolution(video: &MediaStream) -> Option<&'static str> {
    let title = video.display_title.as_deref().unwrap_or("");
    let height = video.height.unwrap_or(0);
    let width = video.width.unwrap_or(0);
    let found = first_word(
        title,
        &[
            "8k", "4320p", "4k", "2160p", "1440p", "1080p", "720p", "576p", "480p", "360p",
            "404p", "384p", "520p",
        ],
    )
    .map(str::to_lowercase);
    if let Some(found) = found {
        let false_4k = matches!(found.as_str(), "4k" | "2160p") && height > 0 && height < 1250;
        if !false_4k {
            return Some(match found.as_str() {
                "8k" | "4320p" => "8K",
                "4k" | "2160p" => "4K",
                "1440p" => "1440p",
                "1080p" => "1080p",
                "720p" => "720p",
                "576p" => "576p",
                "480p" => "480p",
                _ => "LOW-RES",
            });
        }
    }
    // The size of the picture decides when the title has no word for it.
    Some(if height >= 3000 || width >= 7000 {
        "8K"
    } else if height >= 1550 || width >= 3500 {
        "4K"
    } else if height >= 1250 {
        "1440p"
    } else if height >= 1000 {
        "1080p"
    } else if height >= 700 {
        "720p"
    } else if height >= 528 {
        // PAL DVD is 720x576, NTSC DVD is 720x480; 528 splits the two.
        "576p"
    } else if height >= 400 {
        "480p"
    } else if height > 0 {
        "LOW-RES"
    } else {
        return None;
    })
}

fn video_codec(video: &MediaStream) -> Option<&'static str> {
    let codec = lower(&video.codec);
    let tag = lower(&video.codec_tag);
    let from = |text: &str, tag: &str| -> Option<&'static str> {
        let has = |s: &str| text.contains(s);
        Some(if has("hevc") {
            "HEVC"
        } else if has("h265") {
            "H265"
        } else if has("h264") || has("avc") || tag.contains("avc") {
            "H264"
        } else if has("av1") {
            "AV1"
        } else if has("vp9") {
            "VP9"
        } else if has("vp8") {
            "VP8"
        } else if has("xvid") {
            "XVID"
        } else if has("divx") {
            "DIVX"
        } else if has("wmv") || has("vc1") {
            "WMV"
        } else if has("mpeg2") {
            "MPEG2"
        } else if has("mpeg4") {
            "MPEG4"
        } else if has("mjpeg") {
            "MJPEG"
        } else if has("theora") {
            "THEORA"
        } else {
            return None;
        })
    };
    from(&codec, &tag).or_else(|| from(&lower(&video.display_title), ""))
}

fn dynamic_range(video: &MediaStream) -> Option<&'static str> {
    let title = lower(&video.display_title);
    let range = lower(&video.video_range_type);
    let either = |f: &dyn Fn(&str) -> bool| f(&title) || f(&range);
    // `dolby\s*vision|dv`, anywhere in the text.
    let dolby = |s: &str| {
        s.contains("dv") || {
            let squeezed: String = s.split_whitespace().collect::<Vec<_>>().join("");
            squeezed.contains("dolbyvision")
        }
    };
    if either(&dolby) {
        Some("Dolby Vision")
    } else if either(&|s| s.contains("hdr10plus")) {
        Some("HDR10+")
    } else if either(&|s| s.contains("hdr10")) {
        Some("HDR10")
    } else if either(&|s| has_word(s, &["hdr"])) {
        Some("HDR")
    } else {
        None
    }
}

/// The best channel layout of the audio streams: "7.1", "5.1" or "2.0".
fn channel_tag(audio: &[&MediaStream]) -> Option<&'static str> {
    let rank = |tag: &str| match tag {
        "7.1" => 3,
        "5.1" => 2,
        _ => 1,
    };
    let mut max_channels = 0;
    let mut found: Option<&'static str> = None;
    for stream in audio {
        max_channels = max_channels.max(stream.channels.unwrap_or(0));
        let signals = format!(
            "{} {}",
            stream.channel_layout.as_deref().unwrap_or(""),
            stream.display_title.as_deref().unwrap_or("")
        );
        let tag = if has_word(&signals, &["7.1", "7 1", "71"]) {
            Some("7.1")
        } else if has_word(&signals, &["5.1", "5 1", "51"]) {
            Some("5.1")
        } else if has_word(&signals, &["stereo", "2.0", "2 0", "20"]) {
            Some("2.0")
        } else {
            None
        };
        if let Some(tag) = tag
            && found.is_none_or(|best| rank(tag) > rank(best))
        {
            found = Some(tag);
        }
    }
    found.or(if max_channels >= 8 {
        Some("7.1")
    } else if max_channels >= 6 {
        Some("5.1")
    } else if max_channels >= 2 {
        Some("2.0")
    } else {
        None
    })
}

fn audio(audio: &[&MediaStream]) -> Option<String> {
    let mut tag: Option<&'static str> = None;
    // The title of a stream first; the first stream with a hit decides.
    for stream in audio {
        let title = stream.display_title.as_deref().unwrap_or("");
        let lower_title = title.to_lowercase();
        tag = if lower_title.contains("atmos") {
            Some("ATMOS")
        } else if lower_title.contains("truehd") {
            Some("TRUEHD")
        } else if lower_title.contains("dts-x") {
            Some("DTS-X")
        } else if has_word(title, &["dts"]) {
            Some("DTS")
        } else if lower_title.replace(char::is_whitespace, "").contains("dolbydigital+") {
            Some("Dolby Digital+")
        } else {
            None
        };
        if tag.is_some() {
            break;
        }
    }
    if tag.is_none() {
        for stream in audio {
            let codec = lower(&stream.codec);
            let profile = lower(&stream.profile);
            if codec.contains("truehd") || profile.contains("truehd") {
                tag = Some(if codec.contains("atmos") || profile.contains("atmos") {
                    "ATMOS"
                } else {
                    "TRUEHD"
                });
                break;
            } else if codec.contains("dts") {
                tag = Some(if codec.contains('x') || profile.contains('x') {
                    "DTS-X"
                } else {
                    "DTS"
                });
                break;
            } else if codec.contains("eac3") || codec.contains("ddp") {
                tag = Some("Dolby Digital+");
                break;
            }
        }
    }
    let channels = channel_tag(audio);
    match (tag, channels) {
        (Some(tag), Some(channels)) if !tag.contains(channels) => Some(format!("{tag} {channels}")),
        (Some(tag), _) => Some(tag.to_string()),
        (None, Some(c @ ("7.1" | "5.1"))) => Some(c.to_string()),
        _ => None,
    }
}

/// `\bIMAX(?:[ ._-]?ENHANCED)?\b`, without `\bNON[ ._-]?IMAX\b`.
fn is_imax(text: &str) -> bool {
    let text = text.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    // No list per call: this runs twice for each character of the text.
    let at = |i: usize, s: &str| {
        let len = s.chars().count();
        chars.len() >= i + len && chars[i..i + len].iter().copied().eq(s.chars())
    };
    let left = |i: usize| i == 0 || !word(chars[i - 1]);
    let sep = |c: char| matches!(c, ' ' | '.' | '_' | '-');
    let (mut imax, mut non_imax) = (false, false);
    for i in 0..chars.len() {
        if at(i, "imax") && left(i) {
            let end = i + 4;
            let plain = chars.get(end).is_none_or(|c| !word(*c));
            let enhanced = (at(end, "enhanced") && chars.get(end + 8).is_none_or(|c| !word(*c)))
                || (chars.get(end).is_some_and(|c| sep(*c))
                    && at(end + 1, "enhanced")
                    && chars.get(end + 9).is_none_or(|c| !word(*c)));
            if plain || enhanced {
                imax = true;
            }
        }
        if at(i, "non") && left(i) {
            let mut j = i + 3;
            if chars.get(j).is_some_and(|c| sep(*c)) {
                j += 1;
            }
            if at(j, "imax") && chars.get(j + 4).is_none_or(|c| !word(*c)) {
                non_imax = true;
            }
        }
    }
    imax && !non_imax
}

/// One tag and the category it was derived for.
#[derive(Clone, PartialEq, Eq)]
pub struct Tag {
    pub category: Category,
    pub label: String,
}

impl Tag {
    fn new(category: Category, label: impl Into<String>) -> Self {
        Self { category, label: label.into() }
    }
}

/// A tag prints as its label, in the lists of the debug channel.
impl std::fmt::Debug for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.label)
    }
}

/// The tags the plugin derives, before the choices of the user apply. Order
/// of the plugin: IMAX, resolution, codec, range, audio, 3D, stub.
pub fn detect(input: &Input) -> Vec<Tag> {
    let mut tags: Vec<Tag> = Vec::new();
    let video = input.streams.iter().find(|s| s.kind == "Video").copied();
    let audio_streams: Vec<&MediaStream> = input
        .streams
        .iter()
        .filter(|s| s.kind == "Audio")
        .copied()
        .collect();

    // IMAX: the names, the paths and the titles of the streams.
    let mut signals: Vec<&str> = vec![input.name, input.original_title];
    for source in input.sources {
        signals.push(source.path.as_deref().unwrap_or(""));
        signals.push(source.name.as_deref().unwrap_or(""));
    }
    for stream in &input.streams {
        signals.push(stream.display_title.as_deref().unwrap_or(""));
        signals.push(stream.title.as_deref().unwrap_or(""));
    }
    let context = signals
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" | ");
    if is_imax(&context) {
        tags.push(Tag::new(Category::SpecialFormat, "IMAX"));
    }

    if let Some(video) = video {
        tags.extend(resolution(video).map(|tag| Tag::new(Category::Resolution, tag)));
        tags.extend(video_codec(video).map(|tag| Tag::new(Category::VideoCodec, tag)));
        tags.extend(dynamic_range(video).map(|tag| Tag::new(Category::DynamicRange, tag)));
    }
    tags.extend(audio(&audio_streams).map(|tag| Tag::new(Category::Audio, tag)));

    // 3D: a path with "3d" and a 3D layout word.
    let has_3d = input.sources.iter().any(|source| {
        let path = lower(&source.path);
        path.contains("3d")
            && ["hsbs", "fsbs", "htab", "ftab", "mvc"]
                .iter()
                .any(|w| path.contains(w))
    });
    if has_3d {
        tags.push(Tag::new(Category::SpecialFormat, "3D"));
    }

    // A media stub (.disc file) names the disc type.
    let mut stub: Vec<&str> = vec![input.name];
    for source in input.sources {
        stub.push(source.path.as_deref().unwrap_or(""));
        stub.push(source.name.as_deref().unwrap_or(""));
    }
    let stub = stub
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" | ")
        .to_lowercase();
    if stub.contains(".disc") {
        let has = |words: &[&str]| words.iter().any(|w| stub.contains(w));
        tags.push(Tag::new(
            Category::Source,
            if has(&["bluray", "blu-ray", "bdrip", "bd-rip", "bdremux"]) {
                "BluRay"
            } else if has(&["hddvd", "hd-dvd", "hd dvd"]) {
                "HD DVD"
            } else if has(&["dvd"]) {
                "DVD"
            } else if has(&["vhs"]) {
                "VHS"
            } else if has(&["hdtv"]) {
                "HDTV"
            } else {
                "Physical"
            },
        ));
    }
    tags
}

/// "ATMOS 7.1" is an ATMOS tag; the plugin sorts and colours by the base.
fn base(tag: &str) -> &str {
    for base in ["Dolby Digital+", "ATMOS", "DTS-X", "TRUEHD", "DTS"] {
        if tag == base || tag.strip_prefix(base).is_some_and(|rest| rest.starts_with(' ')) {
            return base;
        }
    }
    tag
}

/// Applies the choices of the user: drops the categories that are off, keeps
/// the best resolution, sorts inside a category and stacks the categories in
/// the order of the user (`insertOverlay` of the plugin).
pub fn arrange(tags: &[Tag], prefs: &Prefs) -> Vec<String> {
    let mut buckets: Vec<(Category, Vec<&str>)> = Vec::new();
    for tag in tags.iter().filter(|tag| prefs.show[tag.category.index()]) {
        match buckets.iter_mut().find(|(c, _)| *c == tag.category) {
            Some((_, list)) => list.push(&tag.label),
            None => buckets.push((tag.category, vec![&tag.label])),
        }
    }
    for (category, list) in &mut buckets {
        list.sort_by_key(|tag| {
            category
                .items()
                .iter()
                .position(|i| *i == base(tag))
                .unwrap_or(999)
        });
        if *category == Category::Resolution {
            list.truncate(1);
        }
    }
    buckets.sort_by_key(|(c, _)| (prefs.order[c.index()], c.index()));
    buckets
        .into_iter()
        .flat_map(|(_, list)| list)
        .map(str::to_string)
        .collect()
}

/// The tags derived for the items seen, by item id, with the shape of the
/// item they were derived from (`shape`). A card asks for its labels at
/// each frame, and deriving them reads every stream title of the item.
static SEEN: Mutex<Option<HashMap<String, ([usize; 4], Vec<Tag>)>>> = Mutex::new(None);
/// The map starts again when it holds this many items.
const SEEN_MAX: usize = 4096;

/// What the tags of an item were derived from, as far as a look that
/// costs nothing can tell: a list has an item without its streams, the
/// detail page has it with them, under one id.
fn shape(item: &Item) -> [usize; 4] {
    [
        item.media_streams.len(),
        item.media_sources.len(),
        item.media_sources.first().map_or(0, |source| source.media_streams.len()),
        item.name.len(),
    ]
}

/// The labels to show for an item, with the choices in force.
pub fn labels(item: &Item) -> Vec<String> {
    if item.media_streams.is_empty() && item.media_sources.is_empty() {
        return Vec::new();
    }
    let shape = shape(item);
    let mut seen = SEEN.lock().unwrap();
    let seen = seen.get_or_insert_with(HashMap::new);
    if let Some((known, tags)) = seen.get(&item.id)
        && *known == shape
    {
        return arrange(tags, &prefs());
    }
    let tags = detect(&Input::of(item));
    let shown = arrange(&tags, &prefs());
    if seen.len() >= SEEN_MAX {
        seen.clear();
    }
    seen.insert(item.id.clone(), (shape, tags));
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(title: &str, codec: &str, range: &str, w: i32, h: i32) -> MediaStream {
        MediaStream {
            kind: "Video".into(),
            display_title: Some(title.into()),
            codec: Some(codec.into()),
            video_range_type: Some(range.into()),
            width: Some(w),
            height: Some(h),
            ..Default::default()
        }
    }

    fn sound(title: &str, codec: &str, profile: &str, channels: i32, layout: &str) -> MediaStream {
        MediaStream {
            kind: "Audio".into(),
            display_title: Some(title.into()),
            codec: Some(codec.into()),
            profile: (!profile.is_empty()).then(|| profile.into()),
            channels: Some(channels),
            channel_layout: Some(layout.into()),
            ..Default::default()
        }
    }

    fn tags_of(streams: &[MediaStream]) -> Vec<Tag> {
        let input = Input {
            streams: streams.iter().collect(),
            sources: &[],
            name: "Some Film",
            original_title: "",
        };
        detect(&input)
    }

    #[test]
    fn stream_table() {
        let all = Prefs::default();
        let cases: Vec<(&str, Vec<MediaStream>, Vec<&str>)> = vec![
            (
                "4K HEVC with Atmos",
                vec![
                    video("4K HEVC HDR10", "hevc", "HDR10", 3840, 2160),
                    sound("English - Dolby TrueHD Atmos - 7.1", "truehd", "", 8, "7.1"),
                ],
                // Order: resolution, range, codec, audio.
                vec!["4K", "HDR10", "HEVC", "ATMOS 7.1"],
            ),
            (
                "stereo AAC gets no audio tag (only 7.1 and 5.1 stand alone)",
                vec![
                    video("1080p H264 SDR", "h264", "SDR", 1920, 1080),
                    sound("English - AAC - Stereo", "aac", "", 2, "stereo"),
                ],
                vec!["1080p", "H264"],
            ),
            (
                "stereo DTS keeps its layout",
                vec![
                    video("1080p H264 SDR", "h264", "SDR", 1920, 1080),
                    sound("English - DTS - Stereo", "dts", "DTS", 2, "stereo"),
                ],
                vec!["1080p", "H264", "DTS 2.0"],
            ),
            (
                "DTS-HD MA 5.1 is DTS",
                vec![
                    video("1080p H264 SDR", "h264", "SDR", 1920, 1080),
                    sound("English - DTS-HD MA - 5.1", "dts", "DTS-HD MA", 6, "5.1"),
                ],
                vec!["1080p", "H264", "DTS 5.1"],
            ),
            (
                "DTS:X by profile",
                vec![
                    video("4K HEVC", "hevc", "SDR", 3840, 2160),
                    sound("English - Surround", "dts", "DTS:X", 8, "7.1"),
                ],
                vec!["4K", "HEVC", "DTS-X 7.1"],
            ),
            (
                "AV1 Dolby Vision with DD+ 5.1",
                vec![
                    video("4K AV1 Dolby Vision", "av1", "DOVIWithHDR10", 3840, 2160),
                    sound("English - Dolby Digital+ - 5.1", "eac3", "", 6, "5.1"),
                ],
                vec!["4K", "Dolby Vision", "AV1", "Dolby Digital+ 5.1"],
            ),
            (
                "eac3 without a word in the title",
                vec![
                    video("1080p H264", "h264", "SDR", 1920, 804),
                    sound("English - EAC3 - 5.1", "eac3", "", 6, "5.1"),
                ],
                vec!["1080p", "H264", "Dolby Digital+ 5.1"],
            ),
            (
                "channel layout alone",
                vec![
                    video("1080p H264", "h264", "SDR", 1920, 1080),
                    sound("English - AC3 - 5.1", "ac3", "", 6, "5.1"),
                ],
                vec!["1080p", "H264", "5.1"],
            ),
            (
                "a false 4K title on a small picture falls to the size",
                vec![video("4K HEVC", "hevc", "SDR", 1280, 720)],
                vec!["720p", "HEVC"],
            ),
            (
                "a 4K by size when the title has no word",
                vec![video("HEVC", "hevc", "HDR10Plus", 3840, 1600)],
                vec!["4K", "HDR10+", "HEVC"],
            ),
            (
                "PAL DVD",
                vec![video("MPEG2", "mpeg2video", "SDR", 720, 576)],
                vec!["576p", "MPEG2"],
            ),
            (
                "low resolution",
                vec![video("MPEG4", "mpeg4", "SDR", 320, 240)],
                vec!["LOW-RES", "MPEG4"],
            ),
            (
                "plain HDR",
                vec![video("1080p HEVC HDR", "hevc", "HDR", 1920, 1080)],
                vec!["1080p", "HDR", "HEVC"],
            ),
        ];
        for (name, streams, want) in cases {
            let got = arrange(&tags_of(&streams), &all);
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn choices_filter_and_order() {
        let streams = vec![
            video("4K HEVC HDR10", "hevc", "HDR10", 3840, 2160),
            sound("English - TrueHD - 7.1", "truehd", "", 8, "7.1"),
        ];
        let tags = tags_of(&streams);
        // As the server of the user is set: resolution and range only.
        let prefs = Prefs { show: [true, false, true, false, false, false], ..Default::default() };
        assert_eq!(arrange(&tags, &prefs), ["4K", "HDR10"]);
        // The user stacks audio first.
        let mut prefs = Prefs::default();
        prefs.order[Category::Audio.index()] = 0;
        assert_eq!(arrange(&tags, &prefs)[0], "TRUEHD 7.1");
    }

    #[test]
    fn imax_and_three_d_and_stub() {
        let streams = [video("1080p H264", "h264", "SDR", 1920, 1080)];
        let source = |path: &str| MediaSource {
            path: Some(path.into()),
            ..Default::default()
        };
        let detect_with = |name: &str, path: &str| {
            let sources = vec![source(path)];
            let input = Input {
                streams: streams.iter().collect(),
                sources: &sources,
                name,
                original_title: "",
            };
            detect(&input).into_iter().map(|tag| tag.label).collect::<Vec<String>>()
        };
        assert!(detect_with("Dune", "/m/Dune.2021.IMAX.1080p.mkv").contains(&"IMAX".to_string()));
        assert!(detect_with("Dune", "/m/Dune.IMAX-Enhanced.mkv").contains(&"IMAX".to_string()));
        assert!(!detect_with("Dune", "/m/Dune.NON-IMAX.mkv").contains(&"IMAX".to_string()));
        assert!(!detect_with("Dune", "/m/Dune.IMAXX.mkv").contains(&"IMAX".to_string()));
        assert!(detect_with("Film", "/m/Film.3D.HSBS.mkv").contains(&"3D".to_string()));
        assert!(!detect_with("Film", "/m/Film.3D.mkv").contains(&"3D".to_string()));
        assert!(detect_with("Film", "/m/Film.BluRay.disc").contains(&"BluRay".to_string()));
        assert!(detect_with("Film", "/m/Film.HDDVD.disc").contains(&"HD DVD".to_string()));
        assert!(detect_with("Film", "/m/Film.dvdrip.disc").contains(&"DVD".to_string()));
        assert!(detect_with("Film", "/m/Film.disc").contains(&"Physical".to_string()));
        // A stub tag needs the .disc file; a WEB-DL name gives no source tag.
        let plain = detect_with("Film", "/m/Film.WEB-DL.REMUX.mkv");
        assert_eq!(plain, ["1080p", "H264"]);
    }

    #[test]
    fn the_imax_check() {
        let cases = [
            ("", false),
            ("imax", true),
            ("IMAX", true),
            ("nonimax", false),
            ("NON IMAX", false),
            ("non-imax", false),
            ("Dune.IMAX", true),
            ("Dune.IMAXX", false),
            ("IMAX Enhanced", true),
            ("IMAX.ENHANCED", true),
            ("IMAXENHANCED", true),
            ("IMAX_ENHANCEDX", false),
            ("imax non_imax", false),
            ("a non", false),
            ("non", false),
            ("ima", false),
            ("Élan IMAX", true),
            // The small letter of this capital is two characters.
            ("İMAX", false),
            ("x imax enhance", true),
            ("NONIMAX IMAX", false),
            // Never inside a word.
            ("aimaxb", false),
            ("Climax", false),
        ];
        for (text, want) in cases {
            assert_eq!(is_imax(text), want, "{text:?}");
            // The same at the end and at the start of a longer text.
            assert_eq!(is_imax(&format!("Film | {text}")), want, "after a name: {text:?}");
            assert_eq!(is_imax(&format!("{text} | English - AAC")), want, "before a title: {text:?}");
        }
    }

    /// The labels of an item are derived once and kept by its id; the same
    /// id with other streams (the detail page has more than a list) is
    /// derived again.
    #[test]
    fn labels_are_kept_by_item_and_shape() {
        let mut item: Item = serde_json::from_value(serde_json::json!({
            "Id": "labels-kept-1", "Name": "Film", "Type": "Movie",
            "MediaStreams": [{"Type": "Video", "Codec": "hevc", "Width": 3840, "Height": 2160, "DisplayTitle": "4K HEVC"}]
        }))
        .unwrap();
        let first = labels(&item);
        assert!(first.contains(&"4K".to_string()), "{first:?}");
        assert_eq!(labels(&item), first);
        assert!(SEEN.lock().unwrap().as_ref().unwrap().contains_key("labels-kept-1"));
        item.media_streams.push(sound("English - Dolby TrueHD Atmos - 7.1", "truehd", "", 8, "7.1"));
        assert!(labels(&item).iter().any(|label| label.starts_with("ATMOS")));
        let bare: Item = serde_json::from_value(serde_json::json!({"Id": "labels-kept-2", "Name": "Film", "Type": "Movie"})).unwrap();
        assert!(labels(&bare).is_empty());
    }

    #[test]
    fn the_master_switch_falls_back_to_the_server_default() {
        let on = serde_json::json!({"QualityTagsEnabled": true});
        let off = serde_json::json!({"QualityTagsEnabled": false});
        let null = serde_json::json!({"QualityTagsEnabled": null});
        let none = serde_json::json!({});
        assert!(master_switch(&null, &on), "a null of the user is no choice");
        assert!(master_switch(&none, &on));
        assert!(!master_switch(&off, &on));
        assert!(master_switch(&on, &off));
        assert!(!master_switch(&none, &none));
    }

    #[test]
    fn word_matching() {
        assert!(has_word("English - DTS - 5.1", &["dts"]));
        assert!(!has_word("English - DTSHD", &["dts"]));
        assert!(has_word("HDR", &["hdr"]));
        assert!(!has_word("HDR10", &["hdr"]));
        assert_eq!(first_word("4K HEVC", &["4k", "1080p"]), Some("4k"));
        assert_eq!(first_word("H.264 1080p", &["4k", "1080p"]), Some("1080p"));
    }

    #[test]
    fn prefs_from_settings() {
        let own = serde_json::json!({
            "ShowSourceTag": false, "ShowAudioInfoTag": false, "AudioInfoTagOrder": 2
        });
        let public = serde_json::json!({ "ShowVideoCodecTag": false, "ShowSourceTag": true });
        let prefs = Prefs::from_settings(&own, &public);
        // The user's value wins; the server default fills the gaps; the rest is on.
        assert_eq!(prefs.show, [true, false, true, true, false, false]);
        assert_eq!(prefs.order[Category::Audio.index()], 2);
        set_prefs(prefs);
        assert_eq!(super::prefs().show, prefs.show);
        set_prefs(Prefs::default());
    }
}
