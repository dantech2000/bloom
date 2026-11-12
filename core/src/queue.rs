// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rules of the play queue that need no player (`src/queue.rs` has the
//! queue itself).

use crate::jellyfin::Item;

/// The episode that stands where `lost` stood: the same season and episode
/// numbers, under another id.
pub fn replacement<'a>(lost: &Item, episodes: &'a [Item]) -> Option<&'a Item> {
    episodes.iter().find(|e| {
        e.id != lost.id
            && e.index_number == lost.index_number
            && e.parent_index_number == lost.parent_index_number
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode(id: &str, season: Option<i32>, number: i32) -> Item {
        let mut value = serde_json::json!({"Id": id, "Name": id, "Type": "Episode", "IndexNumber": number});
        if let Some(season) = season {
            value["ParentIndexNumber"] = season.into();
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn the_replacement_has_the_numbers_of_the_lost_episode_under_another_id() {
        let lost = episode("old2", Some(2), 2);
        let episodes = [episode("new1", Some(2), 1), episode("new2", Some(2), 2), episode("new3", Some(2), 3)];
        assert_eq!(replacement(&lost, &episodes).map(|e| e.id.as_str()), Some("new2"));
        // An episode of another season with the same number is not it.
        let other_season = [episode("s1e2", Some(1), 2)];
        assert!(replacement(&lost, &other_season).is_none());
        // The lost episode itself, listed still, is no replacement.
        let itself = [episode("old2", Some(2), 2)];
        assert!(replacement(&lost, &itself).is_none());
        // A series with no seasons matches on the episode number alone.
        let flat_lost = episode("old7", None, 7);
        let flat = [episode("new7", None, 7)];
        assert_eq!(replacement(&flat_lost, &flat).map(|e| e.id.as_str()), Some("new7"));
    }
}
