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
    // At the start of a later word. (The first word is the case above.)
    if title.contains(&query.after_space) {
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

/// The best rank of an item whose folded title and original title are given.
fn rank_of(name: &str, original: Option<&str>, query: &Query) -> Option<u8> {
    let name = rank(name, query);
    let original = original.and_then(|title| rank(title, query));
    name.into_iter().chain(original).min()
}

/// The items of the ranks, best first.
fn best(mut found: Vec<(u8, &Item)>) -> Vec<Item> {
    // A title with a typing error only counts when no title fits the text
    // as it is; next to real matches it reads as a wrong result.
    if found.iter().any(|(rank, _)| *rank < 4) {
        found.retain(|(rank, _)| *rank < 4);
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().take(LIMIT).map(|(_, item)| item.clone()).collect()
}

/// The items whose title fits the text, best first. Folds every title for
/// this one search; a caller that searches the same titles at each key
/// keeps an [`Index`].
pub fn search(catalog: &[Item], query: &str) -> Vec<Item> {
    let query = fold(query);
    if query.is_empty() {
        return Vec::new();
    }
    let query = Query::new(&query);
    let (mut name, mut original) = (String::new(), String::new());
    best(catalog
        .iter()
        .filter_map(|item| {
            fold_into(&item.name, &mut name);
            let other = item.original_title.as_deref().map(|title| {
                fold_into(title, &mut original);
                original.as_str()
            });
            rank_of(&name, other, &query).map(|rank| (rank, item))
        })
        .collect())
}

/// The titles of a library with their folded text, made once when the
/// titles load: a search at each key then folds the typed text alone.
#[derive(Clone, Debug, Default)]
pub struct Index {
    items: Vec<Item>,
    /// The folded title and original title of each item, in step with `items`.
    folded: Vec<(String, Option<String>)>,
}

impl Index {
    pub fn new(items: Vec<Item>) -> Self {
        let folded = items
            .iter()
            .map(|item| (fold(&item.name), item.original_title.as_deref().map(fold)))
            .collect();
        Self { items, folded }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The items whose title fits the text, best first; what [`search`] gives.
    pub fn search(&self, query: &str) -> Vec<Item> {
        let query = fold(query);
        if query.is_empty() {
            return Vec::new();
        }
        let query = Query::new(&query);
        best(self
            .items
            .iter()
            .zip(&self.folded)
            .filter_map(|(item, (name, original))| {
                rank_of(name, original.as_deref(), &query).map(|rank| (rank, item))
            })
            .collect())
    }
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

    #[test]
    fn folds_to_lower_case_letters_digits_and_single_spaces() {
        for (text, folded) in [
            ("", ""),
            (" ", ""),
            ("  A  b ", "a b"),
            ("Amélie", "amelie"),
            ("WALL·E", "wall e"),
            ("Spider-Man: Homecoming", "spider man homecoming"),
            ("a\u{a0}b", "a b"),
            ("Señor Çà", "senor ca"),
            ("!!", ""),
        ] {
            assert_eq!(fold(text), folded, "{text:?}");
        }
    }

    #[test]
    fn one_letter_changed_missing_or_extra_is_one_off() {
        for (a, b, near) in [
            ("abc", "abd", true),
            ("abc", "ab", true),
            ("abc", "xbc", true),
            ("abc", "bc", true),
            ("abcd", "abxd", true),
            ("abc", "abc", true),
            ("a", "", true),
            ("abcd", "ab", false),
            ("abc", "acb", false),
            ("étoile", "etoile", true),
        ] {
            assert_eq!(one_off(a, b), near, "{a:?} {b:?}");
            assert_eq!(one_off(b, a), near, "{b:?} {a:?}");
        }
    }

    /// A word of the title that starts with the text is a match of the
    /// second rank wherever the word is, with one word typed or more.
    #[test]
    fn the_start_of_a_later_word_ranks_before_a_substring() {
        let items = catalog(&["The Dark Knight", "Darkness", "Undark", "A Knight Of The Dark"]);
        assert_eq!(names(&search(&items, "dark")), ["Darkness", "A Knight Of The Dark", "The Dark Knight", "Undark"]);
        assert_eq!(names(&search(&items, "dark kn")), ["The Dark Knight"]);
    }

    /// The index gives what the search over the same titles gives.
    #[test]
    fn the_index_gives_the_results_of_the_search() {
        let mut items = catalog(&["Amélie", "Star Wars", "A Star Is Born", "Superstar", "Interstellar", "Étoile"]);
        items[5].original_title = Some("The Star".into());
        let index = Index::new(items.clone());
        assert_eq!(index.items().len(), 6);
        for query in ["star", "AMELIE", "intersteller", "the s", "etoile", "xyz", "", " "] {
            assert_eq!(names(&index.search(query)), names(&search(&items, query)), "{query:?}");
        }
        assert_eq!(names(&index.search("the star")), ["Étoile"]);
        assert!(Index::default().search("star").is_empty());
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
