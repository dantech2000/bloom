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

/// The language of a code, by either of its codes; case and outer spaces
/// do not count.
pub fn find(code: &str) -> Option<&'static Language> {
    let code = code.trim();
    LANGUAGES.iter().find(|language| {
        language.code.eq_ignore_ascii_case(code)
            || language.other.is_some_and(|other| other.eq_ignore_ascii_case(code))
    })
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
