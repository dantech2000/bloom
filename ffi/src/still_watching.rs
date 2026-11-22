//! "Are you still watching?" for the phone: core's rule and session
//! (`bloom_core::still_watching`, the Mac's src/still_watching.rs), so
//! both apps ask at the same moment. The choice lives in the display
//! preferences under the web's key; Swift reads it and passes it in.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use bloom_core::still_watching::{Choice, PREF_KEY, Session};

use crate::lock;

/// A choice of the setting (the web's values).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum StillWatchingChoice {
    Off,
    Short,
    Default,
    Long,
    VeryLong,
}

impl From<Choice> for StillWatchingChoice {
    fn from(choice: Choice) -> Self {
        match choice {
            Choice::Off => Self::Off,
            Choice::Short => Self::Short,
            Choice::Default => Self::Default,
            Choice::Long => Self::Long,
            Choice::VeryLong => Self::VeryLong,
        }
    }
}

impl From<StillWatchingChoice> for Choice {
    fn from(choice: StillWatchingChoice) -> Self {
        match choice {
            StillWatchingChoice::Off => Self::Off,
            StillWatchingChoice::Short => Self::Short,
            StillWatchingChoice::Default => Self::Default,
            StillWatchingChoice::Long => Self::Long,
            StillWatchingChoice::VeryLong => Self::VeryLong,
        }
    }
}

/// The key of the setting in `CustomPrefs`.
#[uniffi::export]
pub fn still_watching_key() -> String {
    PREF_KEY.to_string()
}

/// The stored value as a choice (none is the default, on).
#[uniffi::export]
pub fn still_watching_choice(value: Option<String>) -> StillWatchingChoice {
    Choice::parse(value.as_deref()).into()
}

/// Every choice, in the order of the menu.
#[uniffi::export]
pub fn still_watching_choices() -> Vec<StillWatchingChoice> {
    Choice::ALL.into_iter().map(Into::into).collect()
}

/// The value to store for a choice.
#[uniffi::export]
pub fn still_watching_value(choice: StillWatchingChoice) -> String {
    Choice::from(choice).key().to_string()
}

/// A choice in words ("3 episodes or 90 minutes").
#[uniffi::export]
pub fn still_watching_label(choice: StillWatchingChoice) -> String {
    Choice::from(choice).label()
}

/// One player's session of the rule.
#[derive(uniffi::Object, Default)]
pub struct StillWatching {
    session: Mutex<Session>,
}

#[uniffi::export]
impl StillWatching {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The first item of a play the user started.
    pub fn begin(&self, item_id: String, episode: bool) {
        lock(&self.session).begin(&item_id, episode, Instant::now());
    }

    /// A touch on the player.
    pub fn input(&self) {
        lock(&self.session).input(Instant::now());
    }

    /// An item that starts after another, chosen by the user (Next, the
    /// episode list): it counts, and asks nothing.
    pub fn note(&self, item_id: String, episode: bool) {
        lock(&self.session).note(&item_id, episode);
    }

    /// An item about to start by itself after another: true when the
    /// player asks first.
    pub fn ask(&self, item_id: String, episode: bool, choice: StillWatchingChoice) -> bool {
        lock(&self.session).ask(&item_id, episode, Choice::from(choice).rule(), Instant::now())
    }

    /// Continue: a new session with this item.
    pub fn keep_watching(&self, item_id: String) {
        lock(&self.session).keep_watching(&item_id, Instant::now());
    }
}
