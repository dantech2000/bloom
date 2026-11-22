// SPDX-License-Identifier: AGPL-3.0-or-later
//! The versions of a film or an episode: one item with more than one
//! media source (a 4K and a 1080p file, a director's cut). Jellyfin had
//! them for films and has them for episodes since 12.0. The id of each
//! source is the id of the item of that file; the source whose id is the
//! item's own is the one a play without a choice gets, as before.

use crate::jellyfin::{Item, MediaSource, StreamKind};

/// The source to play: the one with the wanted id, else the item's own
/// (the source with its id), else the first. None for no sources.
pub fn pick<'a>(sources: &'a [MediaSource], item_id: &str, wanted: Option<&str>) -> Option<&'a MediaSource> {
    wanted
        .and_then(|id| sources.iter().find(|s| s.id == id))
        .or_else(|| sources.iter().find(|s| s.id == item_id))
        .or_else(|| sources.first())
}

/// The place of the source a play without a choice gets.
pub fn default_place(item: &Item) -> usize {
    item.media_sources.iter().position(|s| s.id == item.id).unwrap_or(0)
}

/// "4K · HEVC · 58 GB": what tells a version from the others by its file.
/// Empty for a source the server said nothing about.
pub fn technical(source: &MediaSource) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(video) = source.media_streams.iter().find(|s| s.kind == StreamKind::Video) {
        if let Some(resolution) = crate::quality_tags::resolution(video) {
            parts.push(resolution.to_string());
        }
        if let Some(codec) = crate::quality_tags::video_codec(video) {
            parts.push(codec.to_string());
        }
    }
    if let Some(size) = source.size.filter(|s| *s > 0) {
        parts.push(size_label(size));
    }
    parts.join(" · ")
}

/// The name of a version in a menu: the server's name of the source,
/// else its file in short (`technical`), else its place ("Version 2").
pub fn name(source: &MediaSource, place: usize) -> String {
    if let Some(name) = source.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    let technical = technical(source);
    if technical.is_empty() { format!("Version {}", place + 1) } else { technical }
}

/// "58 GB", "4.2 GB", "700 MB", in the units of Finder (powers of 1000).
fn size_label(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e10 {
        format!("{:.0} GB", b / 1e9)
    } else if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else {
        format!("{:.0} MB", (b / 1e6).max(1.))
    }
}

impl Item {
    /// True when the item has more than one file to choose from.
    pub fn has_versions(&self) -> bool {
        self.media_sources.len() > 1
    }

    /// Puts the source with this id first, so that what reads the first
    /// source (the streams, the file facts) reads that version. Returns
    /// false for an id the item does not have.
    pub fn put_source_first(&mut self, source_id: &str) -> bool {
        let Some(place) = self.media_sources.iter().position(|s| s.id == source_id) else {
            return false;
        };
        let source = self.media_sources.remove(place);
        self.media_sources.insert(0, source);
        true
    }

    /// Puts the source a play without a choice gets first: the server can
    /// list another version before it, and the page would describe a file
    /// that does not play.
    pub fn put_default_source_first(&mut self) {
        let id = self.id.clone();
        self.put_source_first(&id);
    }

    /// The versions in the order a menu lists them, which does not change
    /// when one is picked: the item's own file first, then the larger
    /// files first.
    pub fn versions(&self) -> Vec<&MediaSource> {
        let mut sources: Vec<&MediaSource> = self.media_sources.iter().collect();
        sources.sort_by(|a, b| {
            (a.id != self.id)
                .cmp(&(b.id != self.id))
                .then(b.size.unwrap_or(0).cmp(&a.size.unwrap_or(0)))
                .then(a.id.cmp(&b.id))
        });
        sources
    }

    /// The name of a version of the item in a menu (`name`, numbered by
    /// its place in `versions`).
    pub fn version_name(&self, source: &MediaSource) -> String {
        let place = self.versions().iter().position(|s| s.id == source.id).unwrap_or(0);
        name(source, place)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::MediaStream;

    fn source(id: &str, name: Option<&str>, height: i32, codec: &str, size: u64) -> MediaSource {
        MediaSource {
            id: id.into(),
            name: name.map(str::to_string),
            size: Some(size),
            media_streams: vec![MediaStream {
                kind: StreamKind::Video,
                codec: Some(codec.into()),
                height: Some(height),
                width: Some(height * 16 / 9),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn film() -> Item {
        Item {
            id: "film".into(),
            media_sources: vec![
                source("other", None, 2160, "hevc", 58_000_000_000),
                source("film", Some("1080p"), 1080, "h264", 4_200_000_000),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn the_wanted_source_wins_else_the_items_own() {
        let item = film();
        assert_eq!(pick(&item.media_sources, &item.id, Some("other")).unwrap().id, "other");
        // Without a choice: the item's own file, also when it is not first.
        assert_eq!(pick(&item.media_sources, &item.id, None).unwrap().id, "film");
        // An id the item does not have (a version removed since) is no choice.
        assert_eq!(pick(&item.media_sources, &item.id, Some("gone")).unwrap().id, "film");
        // A source of another id only: the first.
        assert_eq!(pick(&item.media_sources[..1], "film", None).unwrap().id, "other");
        assert!(pick(&[], "film", None).is_none());
        assert_eq!(default_place(&item), 1);
    }

    #[test]
    fn a_version_has_a_name() {
        let item = film();
        assert_eq!(name(&item.media_sources[1], 1), "1080p");
        assert_eq!(name(&item.media_sources[0], 0), "4K · HEVC · 58 GB");
        assert_eq!(technical(&item.media_sources[1]), "1080p · H264 · 4.2 GB");
        let blank = MediaSource { name: Some("  ".into()), ..Default::default() };
        assert_eq!(name(&blank, 2), "Version 3");
    }

    #[test]
    fn the_chosen_version_goes_first() {
        let mut item = film();
        assert!(item.has_versions());
        item.put_default_source_first();
        assert_eq!(item.media_sources[0].id, "film", "the file a play without a choice gets");
        assert!(item.put_source_first("other"));
        assert_eq!(item.media_sources[0].id, "other");
        assert!(!item.put_source_first("gone"));
        assert_eq!(item.media_sources.len(), 2);
    }

    /// A menu keeps its order when a version is picked: the item's own
    /// file first, then the larger files.
    #[test]
    fn the_menu_order_does_not_follow_the_pick() {
        let mut item = film();
        item.media_sources.push(source("small", None, 720, "h264", 900_000_000));
        let order = |item: &Item| item.versions().iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert_eq!(order(&item), ["film", "other", "small"]);
        item.put_source_first("small");
        assert_eq!(order(&item), ["film", "other", "small"]);
        let blank = MediaSource { id: "blank".into(), ..Default::default() };
        item.media_sources.push(blank.clone());
        assert_eq!(item.version_name(&blank), "Version 4");
        assert_eq!(item.version_name(&item.media_sources[0].clone()), "720p · H264 · 900 MB");
    }
}
