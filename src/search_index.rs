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

/// True when the words differ by one letter: changed, missing or extra.
fn one_off(a: &str, b: &str) -> bool {
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

/// How well a title fits the text; lower is better, `None` is no match.
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
    // A typing error in one word. Short words give too many false matches.
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

/// The items whose title fits the text, best first.
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
