//! The title index of the Search tab (the Mac's `TitleSearch.catalog`):
//! every film and series of the server, folded once when it loads, so the
//! titles that fit the typed text show at each key, before the server's
//! search answers. The ranking is core's (`search_index::Index`).

use std::sync::Arc;

use bloom_core::{jellyfin::Client, search_index::Index};

use crate::{BloomError, MediaItem, Server, guarded, medias, run};

/// The titles of one session. Swift loads one after the sign-in and again
/// on the server's news, and searches it on the main thread: a search folds
/// the typed text alone and ranks the folded titles.
#[derive(uniffi::Object)]
pub struct TitleIndex {
    client: Client,
    index: Index,
}

#[uniffi::export]
impl Server {
    /// Loads every film and series (the request of the Mac's catalog,
    /// `pages::catalog_query`) and folds their titles.
    pub async fn title_index(&self) -> Result<Arc<TitleIndex>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let started = std::time::Instant::now();
            let items = client.items(&bloom_core::pages::catalog_query())?.items;
            log::info!("title index: {} titles in {} ms", items.len(), started.elapsed().as_millis());
            Ok(Arc::new(TitleIndex { index: Index::new(items), client }))
        })
        .await
    }
}

#[uniffi::export]
impl TitleIndex {
    /// The titles that fit the text, best first (at most 60); none for a
    /// text with no letter or digit.
    pub fn search(&self, text: String) -> Vec<MediaItem> {
        guarded("title search", Vec::new(), || medias(&self.client, &self.index.search(&text)))
    }

    /// How many titles the index holds.
    pub fn count(&self) -> u32 {
        u32::try_from(self.index.items().len()).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bloom_core::jellyfin::Item;

    use super::*;

    fn item(n: usize, name: &str, kind: &str) -> Item {
        serde_json::from_value(serde_json::json!({
            "Id": format!("{n:032x}"), "Name": name, "Type": kind, "ProductionYear": 2000 + n % 26,
            "ImageTags": { "Primary": "tag" }, "BackdropImageTags": ["tag"],
        }))
        .expect("item")
    }

    fn index(items: Vec<Item>) -> TitleIndex {
        TitleIndex { client: Client::new("http://127.0.0.1:1", "titles-test").with_session("token", "u1"), index: Index::new(items) }
    }

    /// The films and series of dev/mock-jellyfin.
    fn mock_library() -> Vec<Item> {
        [
            "The Long Harbour", "Paper Lanterns", "Nine Minutes North", "A Quiet Machine", "Salt and Copper",
            "The Orchard Line", "Low Tide Radio", "Glass Weather", "Second Lighthouse", "Field Notes",
            "The Slow Train Home", "Winter Almanac", "Small Hours", "Northbound", "The Cartographer",
            "Amber Signal", "Kite Season", "Harbour Lights",
        ]
        .iter()
        .enumerate()
        .map(|(n, name)| item(n, name, "Movie"))
        .chain([item(100, "Station Eleven North", "Series"), item(101, "The Night Desk", "Series")])
        .collect()
    }

    /// Titles made of common words, so a short text matches many of them,
    /// with an accent and some punctuation for the fold.
    fn synthetic(count: usize) -> Vec<Item> {
        const WORDS: [&str; 24] = [
            "the", "night", "harbour", "star", "return", "of", "glass", "winter", "kingdom", "amélie", "north",
            "dark", "city", "lost", "river", "machine", "summer", "last", "train", "signal", "house", "storm",
            "light", "garden",
        ];
        (0..count)
            .map(|n| {
                let words = 2 + n % 4;
                let name: Vec<&str> = (0..words).map(|w| WORDS[(n * 7 + w * 13 + n / 24) % WORDS.len()]).collect();
                let mut name = name.join(" ");
                if n % 9 == 0 {
                    name.push_str(&format!(": Part {}", n % 5 + 1));
                }
                item(n, &name, if n % 3 == 0 { "Series" } else { "Movie" })
            })
            .collect()
    }

    /// The time of the slowest text, the bridge's conversion to `MediaItem`
    /// included: the median of 21 runs of each, since a busy machine or a
    /// paused runner stretches a single run (CLAUDE.md, timing in tests).
    fn slowest(index: &TitleIndex, texts: &[&str]) -> (Duration, usize) {
        let (mut worst, mut found) = (Duration::ZERO, 0);
        for text in texts {
            let mut runs: Vec<Duration> = (0..21)
                .map(|_| {
                    let started = Instant::now();
                    found = found.max(index.search(text.to_string()).len());
                    started.elapsed()
                })
                .collect();
            runs.sort();
            worst = worst.max(runs[runs.len() / 2]);
        }
        (worst, found)
    }

    #[test]
    fn ranks_as_core_does_and_finds_a_typing_error() {
        let index = index(mock_library());
        assert_eq!(index.count(), 20);
        let names = |text: &str| index.search(text.into()).into_iter().map(|m| m.name).collect::<Vec<_>>();
        assert_eq!(names("harb"), ["Harbour Lights", "The Long Harbour"]);
        assert_eq!(names("cartograper"), ["The Cartographer"], "one letter missing");
        assert_eq!(names("north"), ["Northbound", "Nine Minutes North", "Station Eleven North"]);
        assert!(names(" ").is_empty());
        let first = &index.search("night".into())[0];
        assert_eq!(first.kind, crate::MediaKind::Series);
        assert!(first.poster.as_deref().is_some_and(|url| url.contains("/Items/")), "{:?}", first.poster);
    }

    /// The query time, against the budget of a frame (8 ms): the mock's
    /// library and 5,000 titles. The worst texts are a short common one
    /// (60 results to convert) and a long one with a typing error (every
    /// title's words compared letter by letter). Printed for the parity
    /// row; `--nocapture` shows it.
    #[test]
    fn a_search_takes_well_under_a_frame() {
        let texts = ["t", "the", "star", "harbour lig", "kingdon", "amelie garden", "xyzzy qwert"];
        let budget = bloom_core::test_time::scaled(Duration::from_millis(8));
        for (label, items) in [("mock", mock_library()), ("5000", synthetic(5000)), ("20000", synthetic(20_000))] {
            let started = Instant::now();
            let index = index(items);
            let built = started.elapsed();
            let (worst, found) = slowest(&index, &texts);
            println!("title index {label}: built in {built:?}, slowest search {worst:?} ({found} results at most)");
            if label != "20000" {
                assert!(worst < budget, "{label}: {worst:?}");
            }
        }
    }
}
