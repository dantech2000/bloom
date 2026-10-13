// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search over the titles the app holds in memory, so results show on each
//! keystroke without a request. The server search still runs after it and
//! adds what this one does not know (episodes, and titles added since).

use crate::jellyfin::Item;

/// Results of one search.
const LIMIT: usize = 60;

/// Lower case, no accents, and only letters, digits and single spaces.
fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    fold_into(text, &mut out);
    out
}

/// [`fold`] into a text that is there already: a search folds each title of
/// the catalog, and one text serves them all.
fn fold_into(text: &str, out: &mut String) {
    // No space at the start, and one between two words.
    fn push(out: &mut String, c: char) {
        if c != ' ' || !(out.is_empty() || out.ends_with(' ')) {
            out.push(c);
        }
    }
    out.clear();
    for c in text.chars() {
        // Most letters of most titles are ASCII, and need no table.
        if c.is_ascii() {
            push(out, if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { ' ' });
            continue;
        }
        for c in c.to_lowercase() {
            let c = match c {
                'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
                'è' | 'é' | 'ê' | 'ë' => 'e',
                'ì' | 'í' | 'î' | 'ï' => 'i',
                'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
                'ù' | 'ú' | 'û' | 'ü' => 'u',
                'ñ' => 'n',
                'ç' => 'c',
                c if c.is_alphanumeric() => c,
                _ => ' ',
            };
            push(out, c);
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
}

/// True when the words differ by one letter: changed, missing or extra.
fn one_off(a: &str, b: &str) -> bool {
    let (a_len, b_len) = (a.chars().count(), b.chars().count());
    let (short, long) = if a_len <= b_len { (a, b) } else { (b, a) };
    if a_len.abs_diff(b_len) > 1 {
        return false;
    }
    // Past the letters the two words start with.
    let (mut short, mut long) = (short.chars(), long.chars());
    loop {
        let (mut next_short, mut next_long) = (short.clone(), long.clone());
        match (next_short.next(), next_long.next()) {
            (Some(x), Some(y)) if x == y => (short, long) = (next_short, next_long),
            _ => break,
        }
    }
    if a_len == b_len {
        short.skip(1).eq(long.skip(1))
    } else {
        short.eq(long.skip(1))
    }
}

/// The folded text of a search, with what [`rank`] needs of it for each
/// title: made once for a search, not once for a title.
struct Query<'a> {
    text: &'a str,
    /// The text after a space: the start of a word inside a title.
    after_space: String,
    /// The words of the text, each with its count of letters.
    words: Vec<(&'a str, usize)>,
}

impl<'a> Query<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            after_space: format!(" {text}"),
            words: text.split(' ').map(|word| (word, word.chars().count())).collect(),
        }
    }
}

/// How well a title fits the text; lower is better, `None` is no match.
fn rank(title: &str, query: &Query) -> Option<u8> {
    if title == query.text {
        return Some(0);
    }
    if title.starts_with(query.text) {
        return Some(1);
    }
    if title.split(' ').any(|word| word.starts_with(query.text)) || title.contains(&query.after_space) {
        return Some(2);
    }
    if title.contains(query.text) {
        return Some(3);
    }
    // A typing error in one word. Short words give too many false matches.
    let near = |wanted: &str, letters: usize| {
        title.split(' ').any(|word| {
            word.starts_with(wanted)
                || (letters >= 5
                    && (one_off(word, wanted)
                        || word
                            .char_indices()
                            .nth(letters)
                            .is_some_and(|(end, _)| one_off(&word[..end], wanted))))
        })
    };
    query.words.iter().all(|(word, letters)| near(word, *letters)).then_some(4)
}

/// The items whose title fits the text, best first.
pub fn search(catalog: &[Item], query: &str) -> Vec<Item> {
    let query = fold(query);
    if query.is_empty() {
        return Vec::new();
    }
    let query = Query::new(&query);
    let mut folded = String::new();
    let mut rank_of = |title: &str| {
        fold_into(title, &mut folded);
        rank(&folded, &query)
    };
    let mut found: Vec<(u8, &Item)> = catalog
        .iter()
        .filter_map(|item| {
            let name = rank_of(&item.name);
            let original = item.original_title.as_deref().and_then(&mut rank_of);
            name.into_iter().chain(original).min().map(|rank| (rank, item))
        })
        .collect();
    // A title with a typing error only counts when no title fits the text
    // as it is; next to real matches it reads as a wrong result.
    if found.iter().any(|(rank, _)| *rank < 4) {
        found.retain(|(rank, _)| *rank < 4);
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().take(LIMIT).map(|(_, item)| item.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(found: &[Item]) -> Vec<&str> {
        found.iter().map(|i| i.name.as_str()).collect()
    }

    fn catalog(titles: &[&str]) -> Vec<Item> {
        titles
            .iter()
            .map(|title| {
                serde_json::from_value(serde_json::json!({
                    "Id": title, "Name": title, "Type": "Movie"
                }))
                .expect("item")
            })
            .collect()
    }

    /// The search as it was, for the comparison below.
    mod before {
        use super::super::{Item, LIMIT};

        pub fn fold(text: &str) -> String {
            let mut out = String::with_capacity(text.len());
            for c in text.chars().flat_map(char::to_lowercase) {
                let c = match c {
                    'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
                    'è' | 'é' | 'ê' | 'ë' => 'e',
                    'ì' | 'í' | 'î' | 'ï' => 'i',
                    'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
                    'ù' | 'ú' | 'û' | 'ü' => 'u',
                    'ñ' => 'n',
                    'ç' => 'c',
                    c if c.is_alphanumeric() => c,
                    _ => ' ',
                };
                if c != ' ' || !out.ends_with(' ') {
                    out.push(c);
                }
            }
            out.trim().to_string()
        }

        pub fn one_off(a: &str, b: &str) -> bool {
            let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
            let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
            if long.len() - short.len() > 1 {
                return false;
            }
            let same = short.iter().zip(long.iter()).take_while(|(x, y)| x == y).count();
            if long.len() == short.len() {
                short[same..].iter().skip(1).eq(long[same..].iter().skip(1))
            } else {
                short[same..].iter().eq(long[same..].iter().skip(1))
            }
        }

        fn rank(title: &str, query: &str) -> Option<u8> {
            if title == query {
                return Some(0);
            }
            if title.starts_with(query) {
                return Some(1);
            }
            if title.split(' ').any(|word| word.starts_with(query)) || title.contains(&format!(" {query}")) {
                return Some(2);
            }
            if title.contains(query) {
                return Some(3);
            }
            let words: Vec<&str> = query.split(' ').collect();
            let near = |wanted: &str| {
                title.split(' ').any(|word| {
                    word.starts_with(wanted)
                        || (wanted.chars().count() >= 5
                            && (one_off(word, wanted)
                                || word
                                    .char_indices()
                                    .nth(wanted.chars().count())
                                    .is_some_and(|(end, _)| one_off(&word[..end], wanted))))
                })
            };
            words.iter().all(|word| near(word)).then_some(4)
        }

        pub fn search(catalog: &[Item], query: &str) -> Vec<Item> {
            let query = fold(query);
            if query.is_empty() {
                return Vec::new();
            }
            let mut found: Vec<(u8, &Item)> = catalog
                .iter()
                .filter_map(|item| {
                    let name = rank(&fold(&item.name), &query);
                    let original = item
                        .original_title
                        .as_deref()
                        .and_then(|title| rank(&fold(title), &query));
                    name.into_iter().chain(original).min().map(|rank| (rank, item))
                })
                .collect();
            if found.iter().any(|(rank, _)| *rank < 4) {
                found.retain(|(rank, _)| *rank < 4);
            }
            found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
            found.into_iter().take(LIMIT).map(|(_, item)| item.clone()).collect()
        }
    }

    /// A catalog of made-up titles, some with accents, signs and an
    /// original title.
    fn large_catalog(titles: usize) -> Vec<Item> {
        const WORDS: [&str; 24] = [
            "The", "Night", "Étoile", "Star", "Wars", "of", "Return", "Kingdom", "Señor", "Amélie",
            "Spider-Man:", "Home", "Interstellar", "Inception", "Dark", "Knight", "Lost", "Über",
            "Garden", "(2019)", "II", "Blade", "Runner", "Çà",
        ];
        // The same titles in each run.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % below as u64) as usize
        };
        (0..titles)
            .map(|n| {
                let mut title = |words: usize| {
                    (0..words).map(|_| WORDS[next(WORDS.len())]).collect::<Vec<_>>().join(" ")
                };
                let name = title(1 + n % 5);
                let original = (n % 4 == 0).then(|| title(2 + n % 3));
                serde_json::from_value(serde_json::json!({
                    "Id": n.to_string(), "Name": name, "Type": "Movie", "OriginalTitle": original
                }))
                .expect("item")
            })
            .collect()
    }

    /// The search gives the results it gave, in the order it gave them, and
    /// takes less time for each key typed.
    #[test]
    fn the_search_is_the_same_and_cheaper() {
        const TITLES: usize = 5000;
        let items = large_catalog(TITLES);
        let queries = [
            "s", "the", "star", "star w", "star wars", "night of the", "amelie", "AMÉLIE", "etoile",
            "spider man", "spider-man: home", "intersteller", "incepton", "kingdon", "kinght",
            "the kinght", "retrun", "returnn", "retur", "garde", "gardenn", "uber", "ca", "2019",
            "xyz", "blade runer", " ", "::", "ii", "senor", "señor los",
        ];
        let ids = |found: &[Item]| found.iter().map(|i| i.id.clone()).collect::<Vec<_>>();
        for query in queries {
            assert_eq!(ids(&search(&items, query)), ids(&before::search(&items, query)), "{query:?}");
        }
        for text in ["", " ", "  A  b ", "Amélie", "İstanbul", "ǅ", "ß", "Σίσυφος", "a\u{a0}b", "WALL·E", "9½ Weeks", "!!"] {
            assert_eq!(fold(text), before::fold(text), "{text:?}");
        }
        for (a, b) in [("", ""), ("a", ""), ("abc", "abd"), ("abc", "ab"), ("ab", "abc"), ("abc", "xbc"),
            ("abc", "bc"), ("abcd", "ab"), ("abc", "acb"), ("étoile", "etoile"), ("abc", "abc"), ("abcd", "abxd")]
        {
            assert_eq!(one_off(a, b), before::one_off(a, b), "{a:?} {b:?}");
            assert_eq!(one_off(b, a), before::one_off(b, a), "{b:?} {a:?}");
        }

        let time = |run: &dyn Fn(&[Item], &str) -> Vec<Item>, query: &str| {
            // The best of a few rounds: other tests run at the same time.
            (0..7)
                .map(|_| {
                    let started = std::time::Instant::now();
                    std::hint::black_box(run(std::hint::black_box(&items), query));
                    started.elapsed()
                })
                .min()
                .unwrap()
        };
        let (mut was, mut is) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for query in ["the", "intersteller", "blade runer", "xyz"] {
            let (before, now) = (time(&before::search, query), time(&search, query));
            println!("search of {TITLES} titles for {query:?}: {before:?} before, {now:?} now");
            was += before;
            is += now;
        }
        // The times are printed, not asserted: a loaded machine can turn them.
    }

    #[test]
    fn ranks_prefix_before_word_before_substring() {
        let items = catalog(&["Superstar", "Star Wars", "A Star Is Born", "Mayday"]);
        assert_eq!(
            names(&search(&items, "star")),
            ["Star Wars", "A Star Is Born", "Superstar"]
        );
    }

    #[test]
    fn ignores_case_accents_and_punctuation() {
        let items = catalog(&["Amélie", "Spider-Man: Homecoming"]);
        assert_eq!(names(&search(&items, "AMELIE")), ["Amélie"]);
        assert_eq!(names(&search(&items, "spider man home")), ["Spider-Man: Homecoming"]);
    }

    #[test]
    fn allows_one_typing_error_in_a_long_word() {
        let items = catalog(&["Interstellar", "Inception"]);
        assert_eq!(names(&search(&items, "intersteller")), ["Interstellar"]);
        assert_eq!(names(&search(&items, "incepton")), ["Inception"]);
        assert!(search(&items, "xyz").is_empty());
    }

    #[test]
    fn leaves_near_matches_out_when_a_title_fits() {
        let items = catalog(&["Starman", "Heads of State", "Scary Movie"]);
        assert_eq!(names(&search(&items, "star")), ["Starman"]);
        assert_eq!(names(&search(&items, "starm")), ["Starman"]);
    }
}
