// SPDX-License-Identifier: AGPL-3.0-or-later
//! News of the server that is not SyncPlay's ("the library changed", "the
//! watched state changed"): what the app reloads for it, and the counters
//! the debug command `events` prints.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// Which page is open, for the question "does it show what changed".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shows {
    Home,
    Library,
    Other,
}

/// What to reload when the news has been quiet for a moment.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// The title index of the search.
    pub catalog: bool,
    /// The page that is open.
    pub page: bool,
}

/// Whether a message type is news for the pages, and if so whether it is
/// about the library (`Some(true)`) or only about the user (`Some(false)`).
pub fn kind_of(kind: &str) -> Option<bool> {
    match kind {
        "LibraryChanged" => Some(true),
        // The watched state or the resume point of an item changed, for
        // example on another device.
        "UserDataChanged" => Some(false),
        _ => None,
    }
}

/// The home page shows the rows of both kinds of news. A library page shows
/// only the library. While the player is open nothing reloads under it.
pub fn outcome(library: bool, shows: Shows, player_open: bool) -> Outcome {
    let shows_it = match shows {
        Shows::Home => true,
        Shows::Library => library,
        Shows::Other => false,
    };
    Outcome { catalog: library, page: shows_it && !player_open }
}

/// The news must be quiet this long before the app reloads.
pub const QUIET: Duration = Duration::from_secs(3);
/// The reload happens at the latest this long after the first message that
/// no reload has handled, even when the messages do not stop.
pub const MAX_WAIT: Duration = Duration::from_secs(15);

/// How long to wait for the next reload, `since_first` after the first
/// unhandled message. A message that arrives restarts the quiet time, but
/// the total wait never goes over `MAX_WAIT`.
pub fn delay(since_first: Duration) -> Duration {
    QUIET.min(MAX_WAIT.saturating_sub(since_first))
}

/// What the app did with the news, for `events`.
#[derive(Default)]
pub struct NewsStats {
    messages: BTreeMap<String, u32>,
    last_message: Option<Instant>,
    page_reloads: u32,
    catalog_refreshes: u32,
    last_reload: Option<Instant>,
}

impl NewsStats {
    pub fn message(&mut self, kind: &str) {
        *self.messages.entry(kind.to_string()).or_default() += 1;
        self.last_message = Some(Instant::now());
    }

    pub fn reloaded(&mut self, outcome: &Outcome) {
        self.page_reloads += u32::from(outcome.page);
        self.catalog_refreshes += u32::from(outcome.catalog);
        if outcome.page || outcome.catalog {
            self.last_reload = Some(Instant::now());
        }
    }

    pub fn describe(&self, pending: bool) -> String {
        let ago = |at: Option<Instant>| {
            at.map_or("-".to_string(), |at| format!("{:.1}s ago", at.elapsed().as_secs_f64()))
        };
        let kinds = if self.messages.is_empty() {
            "-".to_string()
        } else {
            self.messages.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(",")
        };
        format!(
            "messages: {kinds} | page_reloads={} catalog_refreshes={} | last_message={} last_reload={} pending={pending}",
            self.page_reloads,
            self.catalog_refreshes,
            ago(self.last_message),
            ago(self.last_reload),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_two_kinds_are_news() {
        assert_eq!(kind_of("LibraryChanged"), Some(true));
        assert_eq!(kind_of("UserDataChanged"), Some(false));
        assert_eq!(kind_of("Play"), None);
        assert_eq!(kind_of("SyncPlayGroupUpdate"), None);
    }

    #[test]
    fn a_library_change_refreshes_the_catalog_and_the_home_or_library_page() {
        let both = Outcome { catalog: true, page: true };
        assert_eq!(outcome(true, Shows::Home, false), both);
        assert_eq!(outcome(true, Shows::Library, false), both);
        assert_eq!(outcome(true, Shows::Other, false), Outcome { catalog: true, page: false });
    }

    #[test]
    fn a_change_of_user_data_reloads_the_home_rows_only() {
        assert_eq!(outcome(false, Shows::Home, false), Outcome { catalog: false, page: true });
        assert_eq!(outcome(false, Shows::Library, false), Outcome::default());
        assert_eq!(outcome(false, Shows::Other, false), Outcome::default());
    }

    #[test]
    fn nothing_reloads_under_the_player() {
        assert_eq!(outcome(true, Shows::Home, true), Outcome { catalog: true, page: false });
        assert_eq!(outcome(false, Shows::Home, true), Outcome::default());
    }

    #[test]
    fn a_quiet_moment_is_three_seconds_at_first() {
        assert_eq!(delay(Duration::ZERO), QUIET);
        assert_eq!(delay(Duration::from_secs(5)), QUIET);
    }

    #[test]
    fn a_steady_stream_cannot_postpone_the_reload_for_ever() {
        // A message every second: the deadline stays at 15 s after the first.
        let mut now = Duration::ZERO;
        let mut fired = None;
        for _ in 0..40 {
            let wait = delay(now);
            if wait < Duration::from_secs(1) {
                fired = Some(now + wait);
                break;
            }
            now += Duration::from_secs(1);
        }
        assert_eq!(fired, Some(MAX_WAIT));
        assert_eq!(delay(MAX_WAIT), Duration::ZERO);
        assert_eq!(delay(Duration::from_secs(20)), Duration::ZERO);
        assert_eq!(delay(Duration::from_secs(13)), Duration::from_secs(2));
    }

    #[test]
    fn the_counters_add_up() {
        let mut stats = NewsStats::default();
        stats.message("UserDataChanged");
        stats.message("UserDataChanged");
        stats.message("LibraryChanged");
        stats.reloaded(&Outcome { catalog: true, page: true });
        let text = stats.describe(false);
        assert!(text.contains("LibraryChanged=1,UserDataChanged=2"), "{text}");
        assert!(text.contains("page_reloads=1 catalog_refreshes=1"), "{text}");
    }
}
