// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rows of the home page as a list, and the ten slots of the server
//! (`homesection0` to `homesection9` of the display preferences) that
//! hold it. The web client reads the same slots.
//!
//! The list has the rows this app shows, in order, each once. A kind that
//! only Jellyfin Web shows (Continue Listening, Live TV, …) keeps the
//! slot it has, so a change here does not move it on the web; the rows of
//! the app take the free slots around it.

/// Number of slots the server keeps.
pub const SLOTS: usize = crate::jellyfin::HOME_SECTION_SLOTS;

/// The kinds this app shows, with their names.
pub const APP_KINDS: [(&str, &str); 5] = [
    ("resume", "Continue Watching"),
    ("nextup", "Next Up"),
    ("latestmedia", "Recently Added"),
    ("smalllibrarytiles", "My Media"),
    ("librarybuttons", "My Media (small)"),
];

/// The kinds only Jellyfin Web shows.
pub const WEB_KINDS: [(&str, &str); 4] = [
    ("resumeaudio", "Continue Listening"),
    ("resumebook", "Continue Reading"),
    ("livetv", "Live TV"),
    ("activerecordings", "Active Recordings"),
];

/// The name of a kind; an unknown kind shows as it is.
pub fn name(kind: &str) -> String {
    APP_KINDS
        .iter()
        .chain(WEB_KINDS.iter())
        .find(|(known, _)| *known == kind)
        .map_or_else(|| kind.to_string(), |(_, name)| name.to_string())
}

fn is_app_kind(kind: &str) -> bool {
    APP_KINDS.iter().any(|(known, _)| *known == kind)
}

/// Kinds that show the same row in this app: the two "My Media" kinds
/// both show the row of libraries.
fn same_row(a: &str, b: &str) -> bool {
    let media = |kind: &str| matches!(kind, "smalllibrarytiles" | "librarybuttons");
    a == b || (media(a) && media(b))
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HomeRows {
    /// The rows of this app, from the top.
    pub rows: Vec<String>,
    /// The kinds this app does not show, each with its slot.
    pub web: Vec<(usize, String)>,
}

impl HomeRows {
    /// The list from the kinds of the slots. A row of the app that is
    /// already on the list (in an earlier slot) is dropped; "none" and an
    /// empty slot are skipped.
    pub fn from_slots(slots: &[String]) -> Self {
        let mut list = Self::default();
        for (slot, kind) in slots.iter().take(SLOTS).enumerate() {
            let kind = kind.trim();
            if kind.is_empty() || kind == "none" {
                continue;
            }
            if is_app_kind(kind) {
                if !list.rows.iter().any(|row| same_row(row, kind)) {
                    list.rows.push(kind.to_string());
                }
            } else {
                // A kind of the web, or one a newer server has: it stays.
                list.web.push((slot, kind.to_string()));
            }
        }
        list
    }

    /// The kinds of the ten slots: each web kind in its slot, the rows of
    /// the app in the free slots from the top, "none" in the rest.
    pub fn to_slots(&self) -> [String; SLOTS] {
        let mut slots: [String; SLOTS] = std::array::from_fn(|_| String::new());
        for (slot, kind) in &self.web {
            if *slot < SLOTS {
                slots[*slot] = kind.clone();
            }
        }
        let mut rows = self.rows.iter();
        for slot in slots.iter_mut().filter(|slot| slot.is_empty()) {
            *slot = rows.next().cloned().unwrap_or_else(|| "none".to_string());
        }
        slots
    }

    /// The kinds that can be added: those of the app not on the list.
    pub fn addable(&self) -> Vec<&'static str> {
        APP_KINDS
            .iter()
            .map(|(kind, _)| *kind)
            .filter(|kind| !self.rows.iter().any(|row| same_row(row, kind)))
            .collect()
    }

    /// True when a row can be added: a kind is left and a slot is free.
    pub fn has_room(&self) -> bool {
        self.rows.len() + self.web.len() < SLOTS
    }

    /// Adds a row at the end, when it is not on the list and fits.
    pub fn add(&mut self, kind: &str) {
        if is_app_kind(kind) && self.has_room() && !self.rows.iter().any(|row| same_row(row, kind)) {
            self.rows.push(kind.to_string());
        }
    }

    pub fn remove(&mut self, at: usize) {
        if at < self.rows.len() {
            self.rows.remove(at);
        }
    }

    /// Moves a row one place up (`up`) or down.
    pub fn shift(&mut self, at: usize, up: bool) {
        let other = if up { at.checked_sub(1) } else { Some(at + 1) };
        if let Some(other) = other.filter(|other| *other < self.rows.len() && at < self.rows.len()) {
            self.rows.swap(at, other);
        }
    }

    /// Drops the web kind of a slot.
    pub fn remove_web(&mut self, slot: usize) {
        self.web.retain(|(at, _)| *at != slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(kinds: &[&str]) -> Vec<String> {
        let mut all: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        all.resize(SLOTS, "none".to_string());
        all
    }

    #[test]
    fn the_web_defaults_keep_their_places() {
        let given = slots(&[
            "smalllibrarytiles",
            "resume",
            "resumeaudio",
            "resumebook",
            "livetv",
            "nextup",
            "latestmedia",
        ]);
        let list = HomeRows::from_slots(&given);
        assert_eq!(list.rows, ["smalllibrarytiles", "resume", "nextup", "latestmedia"]);
        assert_eq!(
            list.web,
            [(2, "resumeaudio".to_string()), (3, "resumebook".to_string()), (4, "livetv".to_string())]
        );
        // Unchanged, the list gives back the same slots.
        assert_eq!(list.to_slots().to_vec(), given);
    }

    #[test]
    fn a_move_goes_around_the_web_rows() {
        let mut list = HomeRows::from_slots(&slots(&["resume", "livetv", "nextup"]));
        list.shift(1, true);
        assert_eq!(list.to_slots().to_vec(), slots(&["nextup", "livetv", "resume"]));
        // The ends do not move.
        list.shift(0, true);
        list.shift(1, false);
        assert_eq!(list.rows, ["nextup", "resume"]);
    }

    #[test]
    fn a_row_shows_once() {
        let list = HomeRows::from_slots(&slots(&["resume", "nextup", "resume", "librarybuttons", "smalllibrarytiles"]));
        assert_eq!(list.rows, ["resume", "nextup", "librarybuttons"]);
        assert_eq!(list.addable(), ["latestmedia"]);
        let mut list = list;
        list.add("resume");
        list.add("smalllibrarytiles");
        assert_eq!(list.rows.len(), 3);
        list.add("latestmedia");
        assert_eq!(list.rows, ["resume", "nextup", "librarybuttons", "latestmedia"]);
        assert!(list.addable().is_empty());
    }

    #[test]
    fn a_removed_row_frees_its_slot() {
        let mut list = HomeRows::from_slots(&slots(&["resume", "nextup", "livetv", "latestmedia"]));
        list.remove(0);
        assert_eq!(list.to_slots().to_vec(), slots(&["nextup", "latestmedia", "livetv"]));
        list.remove_web(2);
        assert_eq!(list.to_slots().to_vec(), slots(&["nextup", "latestmedia"]));
    }

    #[test]
    fn an_unknown_kind_stays_in_its_slot() {
        let given = slots(&["none", "resume", "somethingnew"]);
        let list = HomeRows::from_slots(&given);
        assert_eq!(list.web, [(2, "somethingnew".to_string())]);
        assert_eq!(list.to_slots().to_vec(), slots(&["resume", "none", "somethingnew"]));
        assert_eq!(name("somethingnew"), "somethingnew");
    }

    #[test]
    fn a_full_list_takes_no_more() {
        let mut given = vec!["resumeaudio".to_string(); 7];
        given.extend(["resume", "nextup", "latestmedia"].map(String::from));
        let mut list = HomeRows::from_slots(&given);
        assert!(!list.has_room());
        list.add("smalllibrarytiles");
        assert_eq!(list.rows.len(), 3);
    }
}
