// SPDX-License-Identifier: AGPL-3.0-or-later
//! Keeps the display awake while a video plays. mpv draws through the
//! render API, so the system sees no video and the app must say so itself.
//!
//! The assertion is `IOPMAssertionCreateWithName` with
//! `PreventUserIdleDisplaySleep`, not `NSProcessInfo beginActivityWithOptions:`:
//! it is one C call with a name that `pmset -g assertions` lists under the
//! pid of the app, it has no activity object to keep, and it does not touch
//! App Nap or the "user initiated" state of the process.
//!
//! One owner ([`Awake`]) holds the one assertion. The decision is the pure
//! function [`wanted`]; the app feeds it the facts of the player at every
//! change of the status (see `start_player_poll` in `src/app.rs`).

use std::time::{Duration, Instant};

use core_foundation::{base::TCFType, string::CFString};
use gpui_kit::{Context, Task};

use crate::{app::Bloom, cast::target::Kind, player::PlayState};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        kind: core_foundation::string::CFStringRef,
        level: u32,
        name: core_foundation::string::CFStringRef,
        id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

/// Tells macOS that the app does work the user waits for, with timers that
/// must be on time. The system may still sleep when idle; the display is
/// the business of the assertion. Returns the token to end it with.
fn begin_activity() -> Option<usize> {
    use crate::macos::{Id, class, send};
    // NSActivityUserInitiatedAllowingIdleSystemSleep | NSActivityLatencyCritical
    const OPTIONS: u64 = 0x00EF_FFFF | 0xFF_0000_0000;
    let info = send!(Id, class(c"NSProcessInfo"), c"processInfo");
    let reason = crate::airplay::av::nsstring("A video plays");
    let token = send!(
        Id, info, c"beginActivityWithOptions:reason:",
        OPTIONS => u64, reason => Id
    );
    if token.is_null() {
        return None;
    }
    Some(send!(Id, token, c"retain") as usize)
}

fn end_activity(token: usize) {
    use crate::macos::{Id, class, send};
    let token = token as Id;
    let info = send!(Id, class(c"NSProcessInfo"), c"processInfo");
    send!((), info, c"endActivity:", token => Id);
    send!((), token, c"release");
}

/// `kIOPMAssertionLevelOn`.
const LEVEL_ON: u32 = 255;

/// How long a pause keeps the display awake. A pause for a drink or a
/// message must not let the display sleep and come back; a longer pause
/// is a stop for the display.
pub const GRACE: Duration = Duration::from_secs(30);

/// What the decision looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub state: PlayState,
    pub paused: bool,
    /// The player reported an error.
    pub error: bool,
    /// Playback is on this Mac. With a cast target (AirPlay, Chromecast, a
    /// Jellyfin session) the item plays elsewhere, and this display may
    /// sleep.
    pub local: bool,
    /// A SyncPlay group has the player.
    pub group: bool,
}

impl Default for Facts {
    /// Before the first decision: nothing plays, on this Mac.
    fn default() -> Self {
        Self {
            state: PlayState::Idle,
            paused: false,
            error: false,
            local: true,
            group: false,
        }
    }
}

/// What the owner must do with the assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// Hold it, for this reason.
    Hold(&'static str),
    /// Keep it for [`GRACE`] if it is held, then release it. Nothing is
    /// taken when it is not held.
    Grace,
    /// Release it now.
    Release,
}

/// The decision. Playing and not paused holds the assertion. A pause and
/// the way from one item of the queue to the next (`Ended`, `Starting`)
/// keep it for the grace, so a short break does not flap. A group that
/// paused the player keeps it: the group starts again without a touch of
/// this Mac, and a dark display would stay dark. Idle, an error, and a
/// cast target release it.
pub fn wanted(facts: &Facts) -> Want {
    if !facts.local || facts.error {
        return Want::Release;
    }
    match facts.state {
        PlayState::Idle => Want::Release,
        PlayState::Ended => Want::Grace,
        PlayState::Starting if facts.group => Want::Hold("a SyncPlay group loads an item"),
        PlayState::Starting => Want::Grace,
        PlayState::Playing if !facts.paused && facts.group => Want::Hold("a SyncPlay group plays"),
        PlayState::Playing if !facts.paused => Want::Hold("a video plays"),
        PlayState::Playing if facts.group => Want::Hold("a SyncPlay group paused the player"),
        PlayState::Playing => Want::Grace,
    }
}

/// The owner of the assertion.
#[derive(Default)]
pub struct Awake {
    /// The id IOKit gave; none while nothing is held.
    id: Option<u32>,
    /// The activity that keeps macOS from slowing the timers of the app
    /// ("App Nap") while a video plays: the token of `NSProcessInfo`, as a
    /// number. With the window behind others, a napping app drew frames
    /// and sent reports late.
    activity: Option<usize>,
    reason: &'static str,
    /// The timer that releases the assertion at the end of the grace, and
    /// when it ends. Dropped when playback comes back.
    grace: Option<(Instant, Task<()>)>,
    /// The facts of the last decision, for the debug command.
    facts: Facts,
    /// The result of the last `IOPMAssertionCreateWithName`, for the debug
    /// command when it fails.
    last_error: Option<i32>,
}

impl Awake {
    pub fn held(&self) -> bool {
        self.id.is_some()
    }

    /// Takes the assertion, or keeps the one held and notes the reason.
    fn hold(&mut self, reason: &'static str) {
        self.grace = None;
        self.reason = reason;
        if self.id.is_some() {
            return;
        }
        let kind = CFString::from_static_string("PreventUserIdleDisplaySleep");
        let name = CFString::new(&format!("{} plays a video", crate::brand::NAME));
        let mut id = 0;
        let result = unsafe {
            IOPMAssertionCreateWithName(
                kind.as_concrete_TypeRef(),
                LEVEL_ON,
                name.as_concrete_TypeRef(),
                &mut id,
            )
        };
        if result == 0 {
            self.id = Some(id);
            self.last_error = None;
            self.activity = begin_activity();
            log::info!("awake: display sleep prevented ({reason})");
        } else {
            self.last_error = Some(result);
            log::warn!("awake: IOPMAssertionCreateWithName failed: {result:#x}");
        }
    }

    /// Gives the assertion back. Nothing happens when none is held.
    pub fn release(&mut self) {
        self.grace = None;
        if let Some(activity) = self.activity.take() {
            end_activity(activity);
        }
        if let Some(id) = self.id.take() {
            unsafe { IOPMAssertionRelease(id) };
            log::info!("awake: display may sleep again");
        }
    }

    /// One line for the debug command `awake`.
    pub fn describe(&self) -> String {
        let grace = match &self.grace {
            Some((ends, _)) => format!("{:.0}s", ends.saturating_duration_since(Instant::now()).as_secs_f64()),
            None => "off".into(),
        };
        format!(
            "held={} reason={:?} assertion={} grace={grace} want={:?} facts={{state={:?} paused={} error={} local={} group={}}}{}",
            self.id.is_some(),
            if self.id.is_some() { self.reason } else { "" },
            self.id.map(|id| format!("{id:#x}")).unwrap_or_else(|| "none".into()),
            wanted(&self.facts),
            self.facts.state,
            self.facts.paused,
            self.facts.error,
            self.facts.local,
            self.facts.group,
            self.last_error.map(|e| format!(" last_error={e:#x}")).unwrap_or_default(),
        )
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.release();
    }
}

impl Bloom {
    /// The facts of the player right now.
    fn awake_facts(&self) -> Facts {
        Facts {
            state: self.player_status.state,
            paused: self.player_status.paused,
            error: self.player_status.error.is_some(),
            local: self.cast.kind() == Kind::Local,
            group: self.sync.holds_player(),
        }
    }

    /// Takes or gives back the assertion after a change of the player
    /// status. Cheap: a few comparisons, and a call to IOKit only on a
    /// change.
    pub fn awake_update(&mut self, cx: &mut Context<Self>) {
        let facts = self.awake_facts();
        self.awake.facts = facts;
        match wanted(&facts) {
            Want::Hold(reason) => self.awake.hold(reason),
            Want::Grace => {
                if self.awake.id.is_some() && self.awake.grace.is_none() {
                    let task = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(GRACE).await;
                        // A hold meanwhile dropped this task; so the
                        // assertion is still not wanted.
                        this.update(cx, |this, _| this.awake.release()).ok();
                    });
                    self.awake.grace = Some((Instant::now() + GRACE, task));
                }
            }
            Want::Release => self.awake.release(),
        }
    }

    /// The debug command `awake`: whether the assertion is held and why.
    pub fn debug_awake(&mut self, rest: &str, cx: &mut Context<Self>) -> String {
        match rest {
            "" | "state" => {}
            // Runs the decision now, without a change of the player.
            "update" => self.awake_update(cx),
            _ => return "error: awake [state|update]".into(),
        }
        self.awake.describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(state: PlayState, paused: bool) -> Facts {
        Facts {
            state,
            paused,
            error: false,
            local: true,
            group: false,
        }
    }

    #[test]
    fn playing_holds_and_pause_waits() {
        assert_eq!(wanted(&facts(PlayState::Playing, false)), Want::Hold("a video plays"));
        assert_eq!(wanted(&facts(PlayState::Playing, true)), Want::Grace);
    }

    #[test]
    fn idle_and_error_release() {
        assert_eq!(wanted(&facts(PlayState::Idle, false)), Want::Release);
        let error = Facts {
            error: true,
            ..facts(PlayState::Playing, false)
        };
        assert_eq!(wanted(&error), Want::Release);
    }

    #[test]
    fn the_step_between_items_keeps_it() {
        assert_eq!(wanted(&facts(PlayState::Ended, false)), Want::Grace);
        assert_eq!(wanted(&facts(PlayState::Starting, false)), Want::Grace);
    }

    #[test]
    fn a_cast_target_releases() {
        let remote = Facts {
            local: false,
            ..facts(PlayState::Playing, false)
        };
        assert_eq!(wanted(&remote), Want::Release);
    }

    #[test]
    fn a_group_holds_through_its_pause() {
        let group = |state, paused| Facts {
            group: true,
            ..facts(state, paused)
        };
        assert_eq!(wanted(&group(PlayState::Playing, false)), Want::Hold("a SyncPlay group plays"));
        assert_eq!(
            wanted(&group(PlayState::Playing, true)),
            Want::Hold("a SyncPlay group paused the player")
        );
        assert_eq!(wanted(&group(PlayState::Idle, false)), Want::Release);
    }

    #[test]
    fn release_without_a_hold_is_harmless() {
        let mut awake = Awake::default();
        awake.release();
        awake.release();
        assert!(!awake.held());
        assert!(awake.describe().starts_with("held=false"));
    }
}
