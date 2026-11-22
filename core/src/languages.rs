// SPDX-License-Identifier: AGPL-3.0-or-later
//! The languages the app names: three-letter code and English name. One
//! table for the settings, the detail page and the subtitle search.
//!
//! ISO 639-2 has two codes for some languages: the one of the library
//! world ("ger") and the one from the language's own name ("deu"). Jellyfin
//! keeps the first in the settings of a user, and a file can have either,
//! so a code is looked up by both.

/// One language: the code as the server stores it, its other code when it
/// has one, and its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Language {
    pub code: &'static str,
    pub other: Option<&'static str>,
    pub name: &'static str,
}

const fn language(code: &'static str, other: Option<&'static str>, name: &'static str) -> Language {
    Language { code, other, name }
}

/// The languages in common use for audio and subtitles, by name. The
/// server knows some 500; a code outside the list has no name here.
pub const LANGUAGES: [Language; 40] = [
    language("ara", None, "Arabic"),
    language("bul", None, "Bulgarian"),
    language("cat", None, "Catalan"),
    language("chi", Some("zho"), "Chinese"),
    language("hrv", None, "Croatian"),
    language("cze", Some("ces"), "Czech"),
    language("dan", None, "Danish"),
    language("dut", Some("nld"), "Dutch"),
    language("eng", None, "English"),
    language("est", None, "Estonian"),
    language("fil", None, "Filipino"),
    language("fin", None, "Finnish"),
    language("fre", Some("fra"), "French"),
    language("ger", Some("deu"), "German"),
    language("gre", Some("ell"), "Greek"),
    language("heb", None, "Hebrew"),
    language("hin", None, "Hindi"),
    language("hun", None, "Hungarian"),
    language("ice", Some("isl"), "Icelandic"),
    language("ind", None, "Indonesian"),
    language("ita", None, "Italian"),
    language("jpn", None, "Japanese"),
    language("kor", None, "Korean"),
    language("lav", None, "Latvian"),
    language("lit", None, "Lithuanian"),
    language("may", Some("msa"), "Malay"),
    language("nor", None, "Norwegian"),
    language("per", Some("fas"), "Persian"),
    language("pol", None, "Polish"),
    language("por", None, "Portuguese"),
    language("rum", Some("ron"), "Romanian"),
    language("rus", None, "Russian"),
    language("srp", None, "Serbian"),
    language("slo", Some("slk"), "Slovak"),
    language("spa", None, "Spanish"),
    language("swe", None, "Swedish"),
    language("tha", None, "Thai"),
    language("tur", None, "Turkish"),
    language("ukr", None, "Ukrainian"),
    language("vie", None, "Vietnamese"),
];

/// The two-letter code (ISO 639-1) of each language of the list that has
/// one. Jellyfin 12 keeps an item's original language this way ("en", from
/// the metadata providers), while the streams of a file carry three letters.
const TWO_LETTER: [(&str, &str); 39] = [
    ("ara", "ar"), ("bul", "bg"), ("cat", "ca"), ("chi", "zh"), ("hrv", "hr"), ("cze", "cs"),
    ("dan", "da"), ("dut", "nl"), ("eng", "en"), ("est", "et"), ("fin", "fi"), ("fre", "fr"),
    ("ger", "de"), ("gre", "el"), ("heb", "he"), ("hin", "hi"), ("hun", "hu"), ("ice", "is"),
    ("ind", "id"), ("ita", "it"), ("jpn", "ja"), ("kor", "ko"), ("lav", "lv"), ("lit", "lt"),
    ("may", "ms"), ("nor", "no"), ("per", "fa"), ("pol", "pl"), ("por", "pt"), ("rum", "ro"),
    ("rus", "ru"), ("srp", "sr"), ("slo", "sk"), ("spa", "es"), ("swe", "sv"), ("tha", "th"),
    ("tur", "tr"), ("ukr", "uk"), ("vie", "vi"),
];

/// The language of a code, by either of its three-letter codes or its
/// two-letter one, with or without a region ("pt-BR"); case and outer
/// spaces do not count.
pub fn find(code: &str) -> Option<&'static Language> {
    let code = code.trim();
    LANGUAGES
        .iter()
        .find(|language| {
            language.code.eq_ignore_ascii_case(code)
                || language.other.is_some_and(|other| other.eq_ignore_ascii_case(code))
        })
        .or_else(|| {
            let base = code.split(['-', '_']).next().unwrap_or_default();
            let (three, _) = TWO_LETTER.iter().find(|(_, two)| two.eq_ignore_ascii_case(base))?;
            LANGUAGES.iter().find(|language| language.code == *three)
        })
}

/// The value of the user's audio language setting that asks for the
/// original language of each title (Jellyfin 12.0, `AudioLanguagePreference`;
/// the literal of `MediaSourceManager` on the server and of jellyfin-web).
pub const ORIGINAL_LANGUAGE: &str = "OriginalLanguage";

/// True for the setting [`ORIGINAL_LANGUAGE`].
pub fn is_original(preference: &str) -> bool {
    preference.trim().eq_ignore_ascii_case(ORIGINAL_LANGUAGE)
}

/// The language a track should be in for a title: the setting as it is,
/// or, for [`ORIGINAL_LANGUAGE`], the title's original language (the first
/// of a list the server joins with commas); none when the title has none,
/// so the track the server names plays.
pub fn wanted_audio(preference: &str, original: Option<&str>) -> Option<String> {
    if !is_original(preference) {
        return Some(preference.to_string()).filter(|language| !language.trim().is_empty());
    }
    original
        .and_then(|list| list.split(',').map(str::trim).find(|code| !code.is_empty()))
        .map(str::to_string)
}

/// The `Accept-Language` value for the system's preferred languages, most
/// wanted first ("en-US", "de"): each tag, then its language alone when the
/// list lacks it, with falling weights ("en-US,en;q=0.9,de;q=0.8"). A tag
/// with other characters than letters, digits and dashes is left out (an
/// underscore, as Apple writes some, becomes a dash). None for no tag.
pub fn accept_language(tags: &[String]) -> Option<String> {
    let mut wanted: Vec<String> = Vec::new();
    for tag in tags {
        let tag = tag.trim().replace('_', "-");
        if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            continue;
        }
        let base = tag.split('-').next().unwrap_or_default().to_string();
        for one in [tag, base] {
            if !one.is_empty() && !wanted.iter().any(|seen| seen.eq_ignore_ascii_case(&one)) {
                wanted.push(one);
            }
        }
    }
    // Weights go down by a tenth and stay at 0.1 or more (HTTP has three
    // decimals at most); ten tags are plenty.
    wanted.truncate(10);
    let parts: Vec<String> = wanted
        .iter()
        .enumerate()
        .map(|(place, tag)| match place {
            0 => tag.clone(),
            _ => format!("{tag};q=0.{}", 10 - place),
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(","))
}

/// The name of a language code; none for a code outside the list.
pub fn name(code: &str) -> Option<&'static str> {
    find(code).map(|language| language.name)
}

/// True when two codes name one language: the same code, or the two
/// codes of one language ("ger" and "deu").
pub fn same_language(a: &str, b: &str) -> bool {
    match (find(a), find(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a.trim().eq_ignore_ascii_case(b.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_language_is_found_by_either_code() {
        assert_eq!(name("ger"), Some("German"));
        assert_eq!(name("deu"), Some("German"));
        assert_eq!(name(" FRA "), Some("French"));
        assert_eq!(name("eng"), Some("English"));
        assert_eq!(name("xxx"), None);
        assert_eq!(name(""), None);
    }

    #[test]
    fn two_codes_of_one_language_are_the_same() {
        assert!(same_language("fre", "fra") && same_language("CHI", "zho") && same_language("eng", "eng"));
        assert!(!same_language("ger", "fra"));
        // Codes outside the list compare as they are.
        assert!(same_language("tlh", "TLH"));
        assert!(!same_language("tlh", "eng"));
        assert!(!same_language("", "eng"));
    }

    /// An item's original language has two letters ("en"), a track three.
    #[test]
    fn a_two_letter_code_is_the_same_language() {
        assert_eq!(name("en"), Some("English"));
        assert_eq!(name("pt-BR"), Some("Portuguese"));
        assert_eq!(name("zh_TW"), Some("Chinese"));
        assert!(same_language("ja", "jpn") && same_language("de", "deu") && same_language("FR", "fre"));
        assert!(!same_language("en", "fre"));
        assert_eq!(name("xx"), None);
        // Each two-letter code belongs to a language of the list.
        for (three, _) in TWO_LETTER {
            assert!(LANGUAGES.iter().any(|language| language.code == three), "{three}");
        }
    }

    #[test]
    fn the_original_language_is_the_title_s() {
        assert!(is_original("OriginalLanguage") && is_original("originallanguage") && !is_original("eng"));
        assert_eq!(wanted_audio("eng", Some("ja")).as_deref(), Some("eng"));
        assert_eq!(wanted_audio(ORIGINAL_LANGUAGE, Some("ja")).as_deref(), Some("ja"));
        assert_eq!(wanted_audio(ORIGINAL_LANGUAGE, Some(" , ko,en")).as_deref(), Some("ko"));
        assert_eq!(wanted_audio(ORIGINAL_LANGUAGE, Some("")), None);
        assert_eq!(wanted_audio(ORIGINAL_LANGUAGE, None), None);
        assert_eq!(wanted_audio("", None), None);
    }

    #[test]
    fn the_accept_language_of_the_system_languages() {
        let tags = |list: &[&str]| list.iter().map(|tag| tag.to_string()).collect::<Vec<_>>();
        assert_eq!(accept_language(&tags(&["en-US"])).as_deref(), Some("en-US,en;q=0.9"));
        assert_eq!(
            accept_language(&tags(&["en-US", "de-DE", "en-GB", "fr"])).as_deref(),
            Some("en-US,en;q=0.9,de-DE;q=0.8,de;q=0.7,en-GB;q=0.6,fr;q=0.5")
        );
        assert_eq!(accept_language(&tags(&["zh-Hans-CN", "pt_BR"])).as_deref(), Some("zh-Hans-CN,zh;q=0.9,pt-BR;q=0.8,pt;q=0.7"));
        // A tag a header cannot carry is left out.
        assert_eq!(accept_language(&tags(&["en\r\nX: y", " ", "es"])).as_deref(), Some("es"));
        assert_eq!(accept_language(&[]), None);
        // The weights stay at 0.1 or more.
        let many: Vec<String> = (0..20).map(|n| format!("x{n:02}")).collect();
        let value = accept_language(&many).unwrap();
        assert_eq!(value.split(',').count(), 10);
        assert!(value.ends_with(";q=0.1"), "{value}");
    }

    /// No code names two languages, so a lookup has one answer.
    #[test]
    fn the_codes_are_unique() {
        let mut codes: Vec<&str> = LANGUAGES.iter().flat_map(|l| std::iter::once(l.code).chain(l.other)).collect();
        let all = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), all);
    }
}
