// SPDX-License-Identifier: AGPL-3.0-or-later
//! "Are you still watching?": the rule of Jellyfin Web 12.0
//! (`src/plugins/stillWatching/plugin.ts` and `constants.ts`). Before an
//! episode that follows another starts, the player asks when the session
//! has run for the choice's time and either nobody touched the app for
//! that long or the session played the choice's number of episodes. The
//! first item of a play starts a session; Continue starts another.
//!
//! The web keeps the choice in the browser under `stillWatchingPrompt`;
//! Bloom keeps it in the display preferences (`CustomPrefs`) under the
//! same key and values, so the Mac and the phone share it. The web's
//! default is off since 12.2 (jellyfin-web#8592), and so is Bloom's.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

/// The key in `CustomPrefs`, the web's name.
pub const PREF_KEY: &str = "stillWatchingPrompt";

/// The choices of the setting, with the web's values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Off,
    Short,
    Default,
    Long,
    VeryLong,
}

impl Choice {
    pub const ALL: [Choice; 5] = [Choice::Off, Choice::Short, Choice::Default, Choice::Long, Choice::VeryLong];

    /// The value the web stores.
    pub fn key(self) -> &'static str {
        match self {
            Choice::Off => "disabled",
            Choice::Short => "short",
            Choice::Default => "default",
            Choice::Long => "long",
            Choice::VeryLong => "veryLong",
        }
    }

    /// The stored value; none (or one this build does not know) is off,
    /// as on the web since 12.2.
    pub fn parse(value: Option<&str>) -> Choice {
        Choice::ALL.into_iter().find(|c| Some(c.key()) == value).unwrap_or(Choice::Off)
    }

    /// The episodes and the time of the choice; none when it is off.
    pub fn rule(self) -> Option<Rule> {
        // A test makes the rule short (`BLOOM_STILL_WATCHING=count:secs`),
        // whatever the choice (it is off until a user picks one).
        if let Some(rule) = crate::options::options().still_watching {
            return Some(rule);
        }
        let (count, minutes) = match self {
            Choice::Off => return None,
            Choice::Short => (2, 60),
            Choice::Default => (3, 90),
            Choice::Long => (5, 150),
            Choice::VeryLong => (8, 240),
        };
        Some(Rule { count, duration: Duration::from_secs(minutes * 60) })
    }

    /// The choice in words, as the web puts it ("{0} episodes or {1}
    /// minutes").
    pub fn label(self) -> String {
        match self {
            Choice::Off => "Off".to_string(),
            Choice::Short => "2 episodes or 60 minutes".to_string(),
            Choice::Default => "3 episodes or 90 minutes".to_string(),
            Choice::Long => "5 episodes or 150 minutes".to_string(),
            Choice::VeryLong => "8 episodes or 240 minutes".to_string(),
        }
    }
}

/// When the prompt comes: the session has lasted `duration`, and either
/// nobody gave input for `duration` or `count` episodes played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub count: usize,
    pub duration: Duration,
}

/// One play session of the player, as the web's plugin keeps it.
#[derive(Debug, Default)]
pub struct Session {
    start: Option<Instant>,
    last_input: Option<Instant>,
    played: HashSet<String>,
}

impl Session {
    /// The first item of a play the user started: a new session.
    pub fn begin(&mut self, item_id: &str, episode: bool, now: Instant) {
        self.start = Some(now);
        self.played.clear();
        if episode {
            self.played.insert(item_id.to_string());
        }
    }

    /// A key, a click, a touch: the user is there.
    pub fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// An item that follows another (the end of an episode, the next of
    /// the queue). True: the player asks before it plays. False: it plays,
    /// and an episode counts toward the rule.
    pub fn ask(&mut self, item_id: &str, episode: bool, rule: Option<Rule>, now: Instant) -> bool {
        let Some(rule) = rule else { return false };
        if !episode {
            return false;
        }
        let start = *self.start.get_or_insert(now);
        let session = now.saturating_duration_since(start);
        let idle = now.saturating_duration_since(self.last_input.map_or(start, |input| input.max(start)));
        if session >= rule.duration && (idle >= rule.duration || self.played.len() >= rule.count) {
            return true;
        }
        self.played.insert(item_id.to_string());
        false
    }

    /// An item that started without the question (the user chose Next):
    /// an episode counts toward the rule.
    pub fn note(&mut self, item_id: &str, episode: bool) {
        if episode {
            self.played.insert(item_id.to_string());
        }
    }

    /// Continue: the item plays, and a new session starts with it.
    pub fn keep_watching(&mut self, item_id: &str, now: Instant) {
        self.begin(item_id, true, now);
        self.last_input = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: Rule = Rule { count: 3, duration: Duration::from_secs(90 * 60) };

    fn minutes(n: u64) -> Duration {
        Duration::from_secs(n * 60)
    }

    #[test]
    fn the_prompt_waits_for_the_time_of_the_session_however_many_episodes_play() {
        let t0 = Instant::now();
        let mut session = Session::default();
        session.begin("e1", true, t0);
        // Short episodes: five in 50 minutes, and no prompt.
        for (n, id) in ["e2", "e3", "e4", "e5"].iter().enumerate() {
            assert!(!session.ask(id, true, Some(RULE), t0 + minutes(10 * (n as u64 + 1))));
        }
        // Past 90 minutes with three played: it asks.
        assert!(session.ask("e6", true, Some(RULE), t0 + minutes(91)));
    }

    #[test]
    fn idle_time_alone_brings_the_prompt_after_the_time_of_the_rule() {
        let t0 = Instant::now();
        let mut session = Session::default();
        session.begin("e1", true, t0);
        // One long episode later, nobody touched the app: it asks.
        assert!(session.ask("e2", true, Some(RULE), t0 + minutes(95)));
        // With input a minute before, and one episode played, it plays.
        let mut touched = Session::default();
        touched.begin("e1", true, t0);
        touched.input(t0 + minutes(94));
        assert!(!touched.ask("e2", true, Some(RULE), t0 + minutes(95)));
    }

    #[test]
    fn continue_starts_a_new_session() {
        let t0 = Instant::now();
        let mut session = Session::default();
        session.begin("e1", true, t0);
        assert!(session.ask("e2", true, Some(RULE), t0 + minutes(100)));
        session.keep_watching("e2", t0 + minutes(101));
        assert!(!session.ask("e3", true, Some(RULE), t0 + minutes(140)));
        assert!(session.ask("e4", true, Some(RULE), t0 + minutes(240)));
    }

    #[test]
    fn films_and_a_choice_of_off_never_ask() {
        let t0 = Instant::now();
        let mut session = Session::default();
        session.begin("m1", false, t0);
        assert!(!session.ask("m2", false, Some(RULE), t0 + minutes(500)));
        assert!(!session.ask("e1", true, None, t0 + minutes(500)));
        assert!(Choice::Off.rule().is_none());
    }

    #[test]
    fn the_choice_reads_the_web_values_and_defaults_to_off() {
        assert_eq!(Choice::parse(Some("veryLong")), Choice::VeryLong);
        assert_eq!(Choice::parse(Some("disabled")), Choice::Off);
        assert_eq!(Choice::parse(None), Choice::Off);
        assert_eq!(Choice::parse(Some("something new")), Choice::Off);
        for choice in Choice::ALL {
            assert_eq!(Choice::parse(Some(choice.key())), choice);
        }
    }
}
