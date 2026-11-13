// SPDX-License-Identifier: AGPL-3.0-or-later
//! The few AVFoundation calls the AirPlay sender needs, through the
//! Objective-C runtime (see `src/macos.rs`). An `AVPlayer` with
//! `allowsExternalPlayback` hands the stream URL to the AirPlay receiver the
//! user picks; macOS does discovery, pairing and the encrypted session.
//!
//! Struct returns (`CMTime`) go through plain `objc_msgSend`, as the window
//! frame does in `src/pip.rs`: this is the arm64 calling convention.

use std::ffi::c_void;

// A struct comes back from `objc_msgSend` itself on arm64 alone; on x86_64
// it needs `objc_msgSend_stret`, which nothing here calls.
#[cfg(not(target_arch = "aarch64"))]
compile_error!("src/airplay/av.rs and picker.rs read struct returns (CMTime, NSRect) the arm64 way");

use crate::macos::{Id, class, ns_string, send, string_of};
// The pool is shared with the render threads of the players.
pub use crate::macos::Pool;

// The frameworks are loaded with the app; the link lines make the symbols
// of the classes and of the C calls below resolve. Clippy takes the second
// `kind` for a repeat of the first; each line names its own framework.
#[allow(clippy::duplicated_attributes)]
#[link(name = "AVFoundation", kind = "framework")]
#[link(name = "AVKit", kind = "framework")]
#[link(name = "CoreMedia", kind = "framework")]
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    fn CMTimeMakeWithSeconds(seconds: f64, timescale: i32) -> CMTime;
    fn CMTimeGetSeconds(time: CMTime) -> f64;
}

/// `CMTime` of CoreMedia: value / timescale seconds.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

const CMTIME_VALID: u32 = 1;
const CMTIME_INDEFINITE: u32 = 1 << 4;
/// Timescale of the times this module makes; one millisecond.
const TIMESCALE: i32 = 1000;

impl CMTime {
    pub fn seconds(secs: f64) -> Self {
        unsafe { CMTimeMakeWithSeconds(secs, TIMESCALE) }
    }

    /// Seconds, or none when the time is not known (an item that is still
    /// loading has an indefinite duration).
    pub fn as_secs(self) -> Option<f64> {
        if self.flags & CMTIME_VALID == 0 || self.flags & CMTIME_INDEFINITE != 0 {
            return None;
        }
        let secs = unsafe { CMTimeGetSeconds(self) };
        secs.is_finite().then_some(secs)
    }
}

/// `localizedDescription` of an `NSError`, with its underlying error.
fn error_text(error: Id) -> Option<String> {
    let mut text = string_of(send!(Id, error, c"localizedDescription"))?;
    let info = send!(Id, error, c"userInfo");
    if !info.is_null() {
        let under = send!(Id, info, c"objectForKey:", ns_string("NSUnderlyingError") => Id);
        if let Some(more) = string_of(send!(Id, under, c"localizedDescription")) {
            text.push_str(": ");
            text.push_str(&more);
        }
    }
    Some(text)
}

/// Status of the player or of its item: `AVPlayerStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    Unknown,
    ReadyToPlay,
    Failed,
}

/// `AVPlayerTimeControlStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeControl {
    Paused,
    Waiting,
    Playing,
}

/// One `AVPlayer`, owned by the engine thread. Its pointer may be handed to
/// the route picker on the main thread; AVFoundation allows calls to a
/// player from any thread.
pub struct Player(Id);

unsafe impl Send for Player {}

impl Player {
    pub fn new() -> Self {
        let player = send!(Id, class(c"AVPlayer"), c"alloc");
        let player = send!(Id, player, c"init");
        send!((), player, c"setAllowsExternalPlayback:", true => bool);
        Self(player)
    }

    pub fn id(&self) -> Id {
        self.0
    }

    /// Loads the URL as the current item and starts the request for it.
    /// An error for a text that is no URL: Foundation gives nil for it, and
    /// an item made of nil must not reach the player (an exception of
    /// Objective-C through this code ends the app).
    pub fn load(&self, url: &str) -> Result<(), String> {
        let address = send!(Id, class(c"NSURL"), c"URLWithString:", ns_string(url) => Id);
        if address.is_null() {
            return Err("the address of the stream is not a URL".into());
        }
        let item = send!(Id, class(c"AVPlayerItem"), c"playerItemWithURL:", address => Id);
        if item.is_null() {
            return Err("no player item for the stream".into());
        }
        send!((), self.0, c"replaceCurrentItemWithPlayerItem:", item => Id);
        Ok(())
    }

    /// Takes the item away; the stream stops and the receiver goes idle.
    pub fn unload(&self) {
        send!((), self.0, c"replaceCurrentItemWithPlayerItem:", std::ptr::null_mut::<c_void>() => Id);
    }

    pub fn play(&self) {
        send!((), self.0, c"play");
    }

    pub fn pause(&self) {
        send!((), self.0, c"pause");
    }

    /// Seeks with a small tolerance, so the player can land on a segment
    /// boundary near the target instead of decoding up to the exact frame.
    pub fn seek(&self, secs: f64) {
        let tolerance = CMTime::seconds(0.5);
        send!(
            (), self.0, c"seekToTime:toleranceBefore:toleranceAfter:",
            CMTime::seconds(secs) => CMTime, tolerance => CMTime, tolerance => CMTime
        );
    }

    pub fn set_volume(&self, volume: f32) {
        send!((), self.0, c"setVolume:", volume => f32);
    }

    pub fn set_muted(&self, muted: bool) {
        send!((), self.0, c"setMuted:", muted => bool);
    }

    pub fn time_control(&self) -> TimeControl {
        match send!(isize, self.0, c"timeControlStatus") {
            0 => TimeControl::Paused,
            1 => TimeControl::Waiting,
            _ => TimeControl::Playing,
        }
    }

    pub fn external_playback_active(&self) -> bool {
        send!(bool, self.0, c"isExternalPlaybackActive")
    }

    pub fn position(&self) -> Option<f64> {
        send!(CMTime, self.0, c"currentTime").as_secs()
    }

    fn item(&self) -> Id {
        send!(Id, self.0, c"currentItem")
    }

    /// Status of the current item, and of the player when it failed.
    pub fn status(&self) -> ItemStatus {
        let item = self.item();
        let code = if item.is_null() {
            send!(isize, self.0, c"status")
        } else {
            send!(isize, item, c"status")
        };
        match code {
            1 => ItemStatus::ReadyToPlay,
            2 => ItemStatus::Failed,
            _ => ItemStatus::Unknown,
        }
    }

    pub fn duration(&self) -> Option<f64> {
        let item = self.item();
        if item.is_null() {
            return None;
        }
        send!(CMTime, item, c"duration").as_secs()
    }

    /// The item played to its end: the player stopped at the duration.
    pub fn reached_end(&self) -> bool {
        let (Some(position), Some(duration)) = (self.position(), self.duration()) else {
            return false;
        };
        duration > 0. && position >= duration - 0.5 && self.time_control() == TimeControl::Paused
    }

    /// The error of the item or of the player, when one failed.
    pub fn error(&self) -> Option<String> {
        let item = self.item();
        let error = if item.is_null() {
            send!(Id, self.0, c"error")
        } else {
            send!(Id, item, c"error")
        };
        if error.is_null() {
            return None;
        }
        error_text(error)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.unload();
        send!((), self.0, c"release");
    }
}

/// `AVRouteDetector`: tells whether the system sees any AirPlay route at
/// all. Detection costs power, so it is on only while it is in use.
pub struct RouteDetector(Id);

unsafe impl Send for RouteDetector {}

impl RouteDetector {
    pub fn new() -> Self {
        let detector = send!(Id, class(c"AVRouteDetector"), c"alloc");
        let detector = send!(Id, detector, c"init");
        send!((), detector, c"setRouteDetectionEnabled:", true => bool);
        Self(detector)
    }

    pub fn multiple_routes(&self) -> bool {
        send!(bool, self.0, c"multipleRoutesDetected")
    }
}

impl Drop for RouteDetector {
    fn drop(&mut self) {
        send!((), self.0, c"setRouteDetectionEnabled:", false => bool);
        send!((), self.0, c"release");
    }
}
