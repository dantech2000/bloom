// SPDX-License-Identifier: AGPL-3.0-or-later
//! The names of audio and subtitle tracks, in one style for the detail
//! page and the player: "English", "English (Forced)", "English (SDH)",
//! "English · image", "English · 5.1", "Commentary". The format of the
//! track is a quiet detail beside the name, not part of it. The server
//! (`MediaStream`) and mpv (its track list) describe a track in their own
//! words; both come here as [`Facts`].

use crate::jellyfin::{MediaStream, StreamKind};

/// What a name is made from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts<'a> {
    /// Audio when true, else a subtitle.
    pub audio: bool,
    /// The language code ("eng", "ger").
    pub language: Option<&'a str>,
    /// The title the file gives the track ("SDH", "Director's Commentary").
    pub title: Option<&'a str>,
    /// The codec ("subrip", "hdmv_pgs_subtitle", "ac3").
    pub codec: Option<&'a str>,
    /// Audio channels.
    pub channels: Option<i64>,
    pub forced: bool,
    pub hearing_impaired: bool,
    /// In a file of its own beside the media file.
    pub external: bool,
    /// The server's whole name of the track ("English - Default -
    /// SUBRIP"), for a track that says nothing else about itself.
    pub display: Option<&'a str>,
}

/// The name of a track and its quiet detail (the format; for a subtitle
/// in a file of its own, also that).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub name: String,
    pub detail: String,
}

impl<'a> Facts<'a> {
    /// The facts of a stream of the server.
    pub fn of_stream(stream: &'a MediaStream) -> Self {
        Facts {
            audio: stream.kind == StreamKind::Audio,
            language: stream.language.as_deref(),
            title: stream.title.as_deref(),
            codec: stream.codec.as_deref(),
            channels: stream.channels.map(i64::from),
            forced: stream.is_forced,
            hearing_impaired: stream.is_hearing_impaired,
            external: stream.is_external,
            display: stream.display_title.as_deref(),
        }
    }
}

/// The name of a stream of the server.
pub fn stream_name(stream: &MediaStream) -> Name {
    name(&Facts::of_stream(stream))
}

/// The name of a track.
pub fn name(facts: &Facts) -> Name {
    let title = facts.title.map(str::trim).filter(|t| !t.is_empty());
    let lower = title.map(str::to_lowercase).unwrap_or_default();
    let has = |word: &str| lower.split(|c: char| !c.is_alphanumeric()).any(|w| w == word);
    let language = facts
        .language
        .filter(|code| !code.is_empty() && !code.eq_ignore_ascii_case("und"))
        .map(|code| crate::languages::name(code).map_or_else(|| code.to_uppercase(), str::to_string));
    let format = format_name(facts.codec, facts.audio);
    // With no language, the title of the track names it ("Signs"), else
    // the server's name of it.
    let unnamed = || {
        title.or(facts.display.map(str::trim).filter(|t| !t.is_empty()))
            .map(str::to_string)
    };

    if facts.audio {
        if lower.contains("commentary") {
            return Name { name: "Commentary".to_string(), detail: join(&[language.as_deref(), format]) };
        }
        let layout = facts.channels.and_then(channels_name);
        // A track with no language is named by its layout ("Stereo").
        let name = match (language, layout) {
            (Some(language), Some(layout)) => format!("{language} · {layout}"),
            (Some(language), None) => language,
            (None, Some(layout)) => layout.to_string(),
            (None, None) => unnamed()
                .or(format.map(str::to_string))
                .unwrap_or_else(|| "Unknown language".to_string()),
        };
        return Name { name, detail: format.unwrap_or_default().to_string() };
    }

    let mut name = language.or_else(unnamed).unwrap_or_else(|| "Unknown language".to_string());
    if facts.forced || has("forced") {
        name.push_str(" (Forced)");
    } else if facts.hearing_impaired || has("sdh") || has("cc") || lower.contains("hearing impaired") {
        name.push_str(" (SDH)");
    } else if lower.contains("commentary") {
        name.push_str(" (Commentary)");
    }
    if is_image(facts.codec) {
        name.push_str(" · image");
    }
    let external = facts.external.then_some("external file");
    Name { name, detail: join(&[format, external]) }
}

/// "Stereo", "5.1": the layout of a number of channels.
pub fn channels_name(channels: i64) -> Option<&'static str> {
    match channels {
        1 => Some("Mono"),
        2 => Some("Stereo"),
        3 => Some("2.1"),
        6 => Some("5.1"),
        7 => Some("6.1"),
        8 => Some("7.1"),
        _ => None,
    }
}

/// True for a subtitle made of pictures (PGS, VobSub, DVB).
pub fn is_image(codec: Option<&str>) -> bool {
    matches!(
        codec.map(str::to_ascii_lowercase).as_deref(),
        Some("pgs" | "pgssub" | "hdmv_pgs_subtitle" | "dvdsub" | "dvd_subtitle" | "vobsub" | "dvbsub" | "dvb_subtitle")
    )
}

/// The format of a codec in the words of a player: "SubRip", "PGS",
/// "Dolby Digital". None for a codec with no plain name.
fn format_name(codec: Option<&str>, audio: bool) -> Option<&'static str> {
    let codec = codec?.to_ascii_lowercase();
    let name = match codec.as_str() {
        "subrip" | "srt" => "SubRip",
        "ass" | "ssa" => "ASS",
        "webvtt" | "vtt" => "WebVTT",
        "mov_text" | "tx3g" => "Text",
        "pgs" | "pgssub" | "hdmv_pgs_subtitle" => "PGS",
        "dvdsub" | "dvd_subtitle" | "vobsub" => "VobSub",
        "dvbsub" | "dvb_subtitle" => "DVB",
        "aac" => "AAC",
        "ac3" => "Dolby Digital",
        "eac3" => "Dolby Digital Plus",
        "truehd" => "Dolby TrueHD",
        "dts" => "DTS",
        "flac" => "FLAC",
        "opus" => "Opus",
        "mp3" => "MP3",
        "vorbis" => "Vorbis",
        _ if audio && codec.starts_with("pcm") => "PCM",
        _ => return None,
    };
    Some(name)
}

fn join(parts: &[Option<&str>]) -> String {
    parts.iter().flatten().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub<'a>(language: &'a str, codec: &'a str) -> Facts<'a> {
        Facts { language: Some(language), codec: Some(codec), ..Default::default() }
    }

    fn audio<'a>(language: &'a str, codec: &'a str, channels: i64) -> Facts<'a> {
        Facts { audio: true, language: Some(language), codec: Some(codec), channels: Some(channels), ..Default::default() }
    }

    #[test]
    fn subtitles_read_as_a_language_with_its_kind() {
        assert_eq!(name(&sub("eng", "subrip")), Name { name: "English".into(), detail: "SubRip".into() });
        let forced = Facts { forced: true, ..sub("eng", "subrip") };
        assert_eq!(name(&forced).name, "English (Forced)");
        let sdh = Facts { hearing_impaired: true, ..sub("eng", "subrip") };
        assert_eq!(name(&sdh).name, "English (SDH)");
        let sdh_title = Facts { title: Some("English SDH"), ..sub("eng", "subrip") };
        assert_eq!(name(&sdh_title).name, "English (SDH)");
        assert_eq!(name(&sub("eng", "hdmv_pgs_subtitle")), Name { name: "English · image".into(), detail: "PGS".into() });
        let vobsub = Facts { external: true, ..sub("eng", "dvdsub") };
        assert_eq!(name(&vobsub), Name { name: "English · image".into(), detail: "VobSub · external file".into() });
        assert_eq!(name(&sub("ger", "ass")).name, "German");
    }

    #[test]
    fn a_word_inside_another_is_no_flag() {
        // "cc" in "Accents" is not closed captions.
        let title = Facts { title: Some("Accents"), ..sub("eng", "subrip") };
        assert_eq!(name(&title).name, "English");
    }

    #[test]
    fn audio_reads_as_a_language_and_a_layout() {
        assert_eq!(name(&audio("eng", "ac3", 6)), Name { name: "English · 5.1".into(), detail: "Dolby Digital".into() });
        assert_eq!(name(&audio("eng", "aac", 2)).name, "English · Stereo");
        assert_eq!(name(&audio("jpn", "flac", 4)).name, "Japanese");
        let commentary = Facts { title: Some("Director's Commentary"), ..audio("eng", "aac", 2) };
        assert_eq!(name(&commentary), Name { name: "Commentary".into(), detail: "English · AAC".into() });
    }

    #[test]
    fn a_track_with_no_language_says_so() {
        assert_eq!(name(&Facts { codec: Some("subrip"), ..Default::default() }).name, "Unknown language");
        assert_eq!(name(&sub("und", "subrip")).name, "Unknown language");
        let signs = Facts { title: Some("Signs"), ..Default::default() };
        assert_eq!(name(&signs).name, "Signs");
        let audio_named = Facts { audio: true, display: Some("Track of A"), ..Default::default() };
        assert_eq!(name(&audio_named).name, "Track of A");
        assert_eq!(name(&sub("xx", "subrip")).name, "XX");
        let mono = Facts { audio: true, codec: Some("aac"), channels: Some(1), ..Default::default() };
        assert_eq!(name(&mono), Name { name: "Mono".into(), detail: "AAC".into() });
    }

    #[test]
    fn a_stream_of_the_server_takes_its_flags() {
        let stream = MediaStream {
            kind: StreamKind::Subtitle,
            language: Some("eng".into()),
            codec: Some("PGSSUB".into()),
            is_forced: true,
            display_title: Some("English - Forced - PGSSUB".into()),
            ..Default::default()
        };
        assert_eq!(stream_name(&stream).name, "English (Forced) · image");
    }
}
