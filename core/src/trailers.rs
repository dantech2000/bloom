// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which trailer of an item plays. The link comes from the server and goes
//! to a player that opens more than web addresses, so the choice is here,
//! with its tests, and not beside the player (`src/trailer.rs`).

use crate::jellyfin::NamedRef;

/// The trailer link the web plugin would pick: the best YouTube entry by its
/// name, or the first link when none is from YouTube.
pub fn best_trailer(trailers: &[NamedRef]) -> Option<String> {
    let score = |name: &str| {
        let name = name.to_lowercase();
        let base = if name.contains("official trailer") {
            5.0
        } else if name.contains("final trailer") || name.contains("main trailer") {
            4.0
        } else if name.contains("trailer") {
            3.0
        } else if name.contains("teaser") {
            2.0
        } else {
            1.0
        };
        let special = ["sign language", "audio descri", "vertical"]
            .iter()
            .any(|word| name.contains(word));
        if special { base - 0.5 } else { base }
    };
    // The link comes from the server and goes to mpv: a web address only.
    // mpv also opens files, devices and lists of its own (`file://`,
    // `av://`, `edl://`), and an item must not name one of those.
    let web = |url: &&str| url.starts_with("https://") || url.starts_with("http://");
    let is_youtube = |url: &str| url.contains("youtube.com") || url.contains("youtu.be");
    let mut best: Option<(f64, &str)> = None;
    for trailer in trailers {
        let Some(url) = trailer.url.as_deref().filter(web).filter(|url| is_youtube(url)) else {
            continue;
        };
        let rank = score(trailer.name.as_deref().unwrap_or_default());
        if best.is_none_or(|(top, _)| rank > top) {
            best = Some((rank, url));
        }
    }
    best.map(|(_, url)| url.to_string())
        .or_else(|| trailers.iter().filter_map(|t| t.url.as_deref()).find(web).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, url: &str) -> NamedRef {
        NamedRef {
            name: Some(name.into()),
            url: Some(url.into()),
        }
    }

    #[test]
    fn picks_the_official_trailer() {
        let trailers = [
            entry("Teaser", "https://youtu.be/a"),
            entry("Official Trailer", "https://www.youtube.com/watch?v=b"),
            entry("Official Trailer (Audio Described)", "https://youtu.be/c"),
            entry("Trailer", "https://example.com/d.mp4"),
        ];
        assert_eq!(
            best_trailer(&trailers).as_deref(),
            Some("https://www.youtube.com/watch?v=b")
        );
    }

    #[test]
    fn falls_back_to_the_first_link() {
        let trailers = [entry("Trailer", "https://example.com/d.mp4")];
        assert_eq!(
            best_trailer(&trailers).as_deref(),
            Some("https://example.com/d.mp4")
        );
        assert_eq!(best_trailer(&[]), None);
    }

    /// A link that is no web address does not reach the player.
    #[test]
    fn takes_web_links_only() {
        for link in ["file:///etc/passwd", "edl://x", "av://lavfi:x", "/Users/x/a.mp4", "memory://youtube.com"] {
            assert_eq!(best_trailer(&[entry("Official Trailer", link)]), None, "{link}");
        }
        let both = [entry("Official Trailer", "file:///youtube.com/x"), entry("Clip", "https://example.com/d.mp4")];
        assert_eq!(best_trailer(&both).as_deref(), Some("https://example.com/d.mp4"));
    }
}
