// SPDX-License-Identifier: AGPL-3.0-or-later
//! Subtitles, the parts with no window: the labels and the steps of the
//! timing offset, the candidates of a search on the server's providers,
//! the languages the search offers, and the paths of its requests. The
//! panel and the requests themselves are in `src/subtitles.rs`.

use serde::Deserialize;
use serde_json::Value;

use crate::{jellyfin::MediaStream, languages};

/// The delay stays within this many seconds, as a typing slip would not
/// push a subtitle out of the film.
pub const MAX_DELAY_SECS: f64 = 600.;

// ----- the timing offset -----------------------------------------------------

/// "0 ms", "+250 ms", "-1.5 s" for a delay in seconds.
pub fn delay_label(secs: f64) -> String {
    let ms = (secs * 1000.).round() as i64;
    if ms == 0 {
        "0 ms".to_string()
    } else if ms.abs() >= 10_000 {
        format!("{:+.1} s", ms as f64 / 1000.)
    } else {
        format!("{ms:+} ms")
    }
}

/// The delay in seconds after a step of `delta_ms` from `current`.
pub fn stepped(current: f64, delta_ms: i64) -> f64 {
    let ms = (current * 1000.).round() as i64 + delta_ms;
    (ms as f64 / 1000.).clamp(-MAX_DELAY_SECS, MAX_DELAY_SECS)
}

/// The text of a step in the menu: "+100 ms".
pub fn step_label(delta_ms: i64) -> String {
    format!("{delta_ms:+} ms")
}

/// Which delay: of the subtitle or of the audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delay {
    Subtitle,
    Audio,
}

impl Delay {
    pub fn property(self) -> &'static str {
        match self {
            Delay::Subtitle => "sub-delay",
            Delay::Audio => "audio-delay",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Delay::Subtitle => "Subtitles",
            Delay::Audio => "Audio",
        }
    }
}

/// Why a delay cannot change now; none when it can.
///
/// A subtitle burned into the video is part of the picture, so mpv cannot
/// move it. The audio delay is allowed in a SyncPlay group: mpv's
/// `audio-pts`, which the group measures, moves with the delay, so the
/// worker adds the delay back to the sample (`player.rs`).
pub fn blocked(delay: Delay, burned: bool) -> Option<&'static str> {
    match delay {
        Delay::Subtitle if burned => Some("The subtitle is burned into the video; it cannot move."),
        _ => None,
    }
}

// ----- candidates ----------------------------------------------------------------

/// One subtitle the server found at a provider.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Candidate {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub download_count: Option<i64>,
    #[serde(default)]
    pub community_rating: Option<f32>,
    #[serde(default)]
    pub is_hash_match: Option<bool>,
    #[serde(default)]
    pub ai_translated: Option<bool>,
    #[serde(default)]
    pub machine_translated: Option<bool>,
    #[serde(default)]
    pub forced: Option<bool>,
    #[serde(default)]
    pub hearing_impaired: Option<bool>,
}

impl Candidate {
    /// The quiet line under the name: provider, format, downloads, rating.
    pub fn detail(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.provider_name.is_empty() {
            parts.push(self.provider_name.clone());
        }
        if !self.format.is_empty() {
            parts.push(self.format.to_uppercase());
        }
        if let Some(count) = self.download_count.filter(|c| *c > 0) {
            parts.push(format!("{} downloads", thousands(count)));
        }
        if let Some(rating) = self.community_rating.filter(|r| *r > 0.) {
            parts.push(format!("rating {rating:.1}"));
        }
        if !self.author.is_empty() {
            parts.push(format!("by {}", self.author));
        }
        parts.join(" · ")
    }

    /// The small labels on a row, in a fixed order.
    pub fn badges(&self) -> Vec<&'static str> {
        let mut badges = Vec::new();
        if self.is_hash_match == Some(true) {
            badges.push("Hash match");
        }
        if self.hearing_impaired == Some(true) {
            badges.push("Hearing impaired");
        }
        if self.forced == Some(true) {
            badges.push("Forced");
        }
        if self.machine_translated == Some(true) || self.ai_translated == Some(true) {
            badges.push("Machine translated");
        }
        badges
    }
}

/// 12345 as "12,345". The dashboard counts with it as well.
pub fn thousands(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut out = String::new();
    for (n, c) in digits.chars().enumerate() {
        if n > 0 && (digits.len() - n) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0 { format!("-{out}") } else { out }
}

/// The candidates of a search answer. A row that is not an object is left
/// out; the best match is first: a hash match, then the downloads.
pub fn parse_candidates(body: &Value) -> Vec<Candidate> {
    let mut list: Vec<Candidate> = body
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| serde_json::from_value(row.clone()).ok())
        .filter(|c: &Candidate| !c.id.is_empty())
        .collect();
    list.sort_by_key(|c| {
        (
            std::cmp::Reverse(c.is_hash_match == Some(true)),
            std::cmp::Reverse(c.download_count.unwrap_or(0)),
        )
    });
    list
}

/// What an empty search means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Nothing {
    /// The server lists no subtitle provider.
    NoProvider,
    /// There are providers, or the app could not ask; nothing matched.
    NotFound { providers_unknown: bool },
}

/// The rule for an empty list. `providers` is the number of subtitle
/// fetchers the server lists (`/Libraries/AvailableOptions`), or none when
/// the app could not read it (the call needs more rights than the search).
pub fn nothing_found(providers: Option<usize>) -> Nothing {
    match providers {
        Some(0) => Nothing::NoProvider,
        Some(_) => Nothing::NotFound { providers_unknown: false },
        None => Nothing::NotFound { providers_unknown: true },
    }
}

impl Nothing {
    pub fn message(&self) -> &'static str {
        match self {
            Nothing::NoProvider => {
                "The server has no subtitle provider. Install one (for example Open Subtitles) in the dashboard."
            }
            Nothing::NotFound { .. } => "No subtitles found",
        }
    }

    /// A quiet second line, when the cause is not certain.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Nothing::NotFound { providers_unknown: true } => Some(
                "The search needs a subtitle provider on the server, such as Open Subtitles.",
            ),
            _ => None,
        }
    }
}


// ----- languages -----------------------------------------------------------------

/// The languages of the chips, by the code a search asks with.
const COMMON: [&str; 14] = [
    "eng", "spa", "fra", "deu", "ita", "por", "nld", "rus", "jpn", "kor", "zho", "ara", "pol", "tur",
];

/// The name of a language code; the code itself when it is not in the
/// list of `languages`.
pub fn language_name(code: &str) -> String {
    languages::name(code).map_or_else(|| code.to_uppercase(), str::to_string)
}

/// The language the search starts with: the preference of the user when it
/// is a three-letter code, else English.
pub fn default_language(preference: Option<&str>) -> String {
    preference
        .map(str::trim)
        .filter(|p| p.len() == 3 && p.chars().all(|c| c.is_ascii_alphabetic()))
        .map_or_else(|| "eng".to_string(), str::to_ascii_lowercase)
}

/// The chips: the language of the user first when it is not a common one,
/// then the common ones. A common language keeps the code the user has
/// for it ("ger"), so it is one chip and the search asks with that code.
pub fn language_choices(preference: Option<&str>) -> Vec<String> {
    let mut list: Vec<String> = COMMON.iter().map(|code| code.to_string()).collect();
    let own = default_language(preference);
    match list.iter_mut().find(|code| languages::same_language(code, &own)) {
        Some(code) => *code = own,
        None => list.insert(0, own),
    }
    list
}

// ----- the requests ----------------------------------------------------------------

/// A value in the path of a request.
pub fn segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The path of a search (`last` is a language, with GET) and of a
/// download (`last` is the id of a candidate, with POST).
pub fn remote_search_path(item_id: &str, last: &str) -> String {
    format!("/Items/{}/RemoteSearch/Subtitles/{}", segment(item_id), segment(last))
}

pub fn remove_path(item_id: &str, index: i64) -> String {
    format!("/Videos/{}/Subtitles/{index}", segment(item_id))
}

/// The stream that `after` has and `before` has not: an external one with a
/// path that was not there. The indexes of the external streams can move
/// when one is added, so the path names a stream.
pub fn new_stream<'a>(before: &[MediaStream], after: &'a [MediaStream]) -> Option<&'a MediaStream> {
    let external = |list: &[MediaStream]| list.iter().filter(|s| s.is_external).count();
    after
        .iter()
        .filter(|s| s.is_external)
        .find(|s| s.path.is_none() || !before.iter().any(|b| b.is_external && b.path == s.path))
        .filter(|_| external(after) > external(before))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn delay_labels() {
        assert_eq!(delay_label(0.), "0 ms");
        assert_eq!(delay_label(0.25), "+250 ms");
        assert_eq!(delay_label(-0.1), "-100 ms");
        assert_eq!(delay_label(0.0004), "0 ms");
        assert_eq!(delay_label(12.5), "+12.5 s");
    }

    #[test]
    fn steps_round_to_milliseconds_and_stay_in_range() {
        assert_eq!(stepped(0., 100), 0.1);
        assert_eq!(stepped(0.1, -500), -0.4);
        assert_eq!(stepped(0.30000000000000004, 100), 0.4);
        assert_eq!(stepped(599.9, 500), 600.);
    }

    #[test]
    fn a_burned_subtitle_blocks_only_its_delay() {
        assert!(blocked(Delay::Subtitle, true).is_some());
        assert!(blocked(Delay::Subtitle, false).is_none());
        assert!(blocked(Delay::Audio, true).is_none());
    }

    fn sample() -> Value {
        json!([
            {"Id": "a", "Name": "Film.2024.en", "ProviderName": "Open Subtitles", "Format": "srt",
             "Author": "x", "DownloadCount": 1200, "CommunityRating": 8.1, "IsHashMatch": false,
             "ThreeLetterISOLanguageName": "eng", "HearingImpaired": true},
            {"Id": "b", "Name": "Film.2024.hash", "ProviderName": "Open Subtitles", "Format": "ass",
             "DownloadCount": 5, "IsHashMatch": true, "ThreeLetterISOLanguageName": "eng",
             "MachineTranslated": true, "Forced": true},
            {"Name": "no id"},
            "junk"
        ])
    }

    #[test]
    fn candidates_parse_and_sort_hash_matches_first() {
        let list = parse_candidates(&sample());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "b");
        assert_eq!(list[0].badges(), vec!["Hash match", "Forced", "Machine translated"]);
        assert_eq!(list[1].badges(), vec!["Hearing impaired"]);
        assert_eq!(list[1].detail(), "Open Subtitles · SRT · 1,200 downloads · rating 8.1 · by x");
        assert!(parse_candidates(&json!({"x": 1})).is_empty());
    }

    #[test]
    fn language_choice() {
        assert_eq!(default_language(Some("spa")), "spa");
        assert_eq!(default_language(Some("")), "eng");
        assert_eq!(default_language(Some("english")), "eng");
        assert_eq!(default_language(None), "eng");
        assert_eq!(language_choices(Some("swe"))[0], "swe");
        assert_eq!(language_choices(Some("spa"))[0], "eng");
        assert_eq!(language_name("fra"), "French");
        assert_eq!(language_name("swe"), "Swedish");
        assert_eq!(language_name("tlh"), "TLH");
        // Jellyfin keeps "ger" for German: one chip, named, with the code
        // of the user, and not a chip "GER" beside "German".
        assert_eq!(language_name("ger"), "German");
        let german = language_choices(Some("ger"));
        assert_eq!(german.len(), COMMON.len());
        assert_eq!(german[3], "ger");
        assert!(!german.contains(&"deu".to_string()));
            }

    #[test]
    fn the_message_for_an_empty_search() {
        assert_eq!(nothing_found(Some(0)), Nothing::NoProvider);
        assert_eq!(
            nothing_found(Some(0)).message(),
            "The server has no subtitle provider. Install one (for example Open Subtitles) in the dashboard."
        );
        assert_eq!(nothing_found(Some(2)).message(), "No subtitles found");
        assert!(nothing_found(Some(2)).hint().is_none());
        assert_eq!(nothing_found(None).message(), "No subtitles found");
        assert!(nothing_found(None).hint().is_some());
    }

    #[test]
    fn request_paths() {
        assert_eq!(remote_search_path("abc", "eng"), "/Items/abc/RemoteSearch/Subtitles/eng");
        assert_eq!(
            remote_search_path("abc", "os/1 2"),
            "/Items/abc/RemoteSearch/Subtitles/os%2F1%202"
        );
        assert_eq!(remove_path("abc", 7), "/Videos/abc/Subtitles/7");
    }

    #[test]
    fn a_new_external_stream_is_found_by_its_path() {
        let stream = |index, is_external, path: &str| MediaStream {
            kind: "Subtitle".into(),
            index,
            is_external,
            path: (!path.is_empty()).then(|| path.to_string()),
            language: Some("eng".into()),
            ..Default::default()
        };
        let before = vec![stream(2, false, ""), stream(3, true, "/m/a.srt")];
        assert!(new_stream(&before, &before).is_none());
        // The new file sorts first: the old one moves to index 4.
        let after = vec![stream(2, false, ""), stream(3, true, "/m/0.srt"), stream(4, true, "/m/a.srt")];
        assert_eq!(new_stream(&before, &after).map(|s| s.index), Some(3));
    }
}
