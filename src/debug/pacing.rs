// SPDX-License-Identifier: AGPL-3.0-or-later
//! The debug command `pacing` (`dev/jctl pacing ...`): the numbers of the
//! frame timing, and what a measurement of it needs of the window. The
//! timing itself is in `src/pacing.rs`.

use std::{
    ptr,
    time::{Duration, Instant},
};

use gpui_kit::{Context, Window};

use crate::{
    app::Bloom,
    macos::{CGRect, send},
    pacing,
};

impl Bloom {
    /// `pacing`: the numbers since the last call; the first call turns the
    /// trace on (see `pacing::trace`), so the second call is the first window.
    /// `pacing clock`: the display link. `pacing file <path>`: plays a file
    /// on this Mac, muted, for the other frame rates (make one with
    /// ffmpeg). `pacing speed <x>` sets the playback speed. `pacing hide`
    /// takes the window off the screen and `pacing show` brings it back: a
    /// window that draws nothing.
    pub fn debug_pacing(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            "" => {
                let started = crate::player::clock_ns();
                let (_, period) = self.player.clock_time();
                let report = format!(
                    "{}\n  {}\n  {}\n  {}",
                    crate::perf::pacing_report(period),
                    pacing::trace::report(period, 60),
                    pacing::trace::other_draws_report(period),
                    pacing::phase::describe()
                );
                pacing::trace::enable();
                // The report runs on the main thread: how long it held it.
                let ended = crate::player::clock_ns();
                pacing::trace::main_job("pacing report", started, ended);
                format!("{report}\n  report took {:.2} ms", (ended - started) as f64 / 1e6)
            }
            // A stall of the main thread of <ms>, to see what one late
            // draw does to the frames after it.
            "stall" => match arg.parse::<u64>() {
                Ok(ms) => {
                    let until = Instant::now() + Duration::from_millis(ms);
                    while Instant::now() < until {
                        std::hint::spin_loop();
                    }
                    format!("stalled {ms} ms")
                }
                Err(_) => "error: pacing stall <ms>".into(),
            },
            "clock" => self.player.pacing_state(),
            "speed" => match arg.parse::<f32>() {
                Ok(speed) => {
                    self.set_speed(speed, cx);
                    format!("speed {speed}")
                }
                Err(_) => "error: pacing speed <x>".into(),
            },
            // In front of every other window, without the focus: a test
            // instance that other windows cover draws no frame (macOS
            // shows none of a covered window), and a measurement needs
            // the window in view.
            "front" => {
                let Some(ns_window) = crate::pip::ns_window(window) else {
                    return "error: no window".into();
                };
                send!((), ns_window, c"orderFrontRegardless");
                "in front".into()
            }
            "hide" | "show" => {
                let Some(ns_window) = crate::pip::ns_window(window) else {
                    return "error: no window".into();
                };
                // Off the screen and back behind the other windows, so no
                // focus moves.
                if verb == "hide" {
                    send!((), ns_window, c"orderOut:", ptr::null_mut() => crate::macos::Id);
                } else {
                    send!((), ns_window, c"orderBack:", ptr::null_mut() => crate::macos::Id);
                }
                let visible = send!(bool, ns_window, c"isVisible");
                format!("{verb}: visible={visible}")
            }
            "file" if !arg.is_empty() => {
                let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
                    return "error: not signed in".into();
                };
                self.playback.muted = true;
                self.player.set_muted(true);
                self.player.play(crate::player::PlayRequest {
                    client,
                    item_id: "local-pacing-test".into(),
                    url: arg.to_string(),
                    title: arg.rsplit('/').next().unwrap_or(arg).to_string(),
                    start_secs: 0.,
                    paused: false,
                    token: 0,
                    play_session_id: None,
                    play_method: "DirectPlay".into(),
                    transcoding: false,
                    media_source_id: String::new(),
                    subtitles: Vec::new(),
                });
                self.player.set_muted(true);
                self.playback.status = self.player.status();
                self.playback.open = true;
                self.start_player_poll(cx);
                // The menus of the player, as a play through the app builds
                // them; without this the settings menu had no entries.
                self.rebuild_track_menus(cx);
                cx.notify();
                format!("playing {arg}")
            }
            // One draw outside the frame rhythm, to see what the
            // compositor does with it.
            "poke" => {
                cx.notify();
                "poked".into()
            }
            // `pacing screen`: the displays and their frames; `pacing
            // screen <display id>` moves the window onto that display
            // (a test with a virtual display of another refresh rate).
            "screen" => screen_verb(arg, window),
            _ => "error: pacing [clock | file <path> | front | hide | show | poke | speed <x> | stall <ms> | screen [<display>]]".into(),
        }
    }
}

/// The `NSScreen`s with their display ids and frames, or the window moved
/// onto the one with the id given, 20 points in from its corner and no
/// larger than it.
fn screen_verb(arg: &str, window: &Window) -> String {
    let screens = send!(crate::macos::Id, crate::macos::class(c"NSScreen"), c"screens");
    let count = send!(usize, screens, c"count");
    let wanted: Option<u32> = arg.parse().ok();
    let mut out = Vec::new();
    for i in 0..count {
        let screen = send!(crate::macos::Id, screens, c"objectAtIndex:", i => usize);
        let description = send!(crate::macos::Id, screen, c"deviceDescription");
        let id = pacing::screen_number(description).unwrap_or(0);
        let frame = send!(CGRect, screen, c"frame");
        out.push(format!("display {id}: {}x{} at ({}, {})", frame.width, frame.height, frame.x, frame.y));
        if wanted == Some(id) {
            let Some(ns_window) = crate::pip::ns_window(window) else {
                return "error: no window".into();
            };
            let current = send!(CGRect, ns_window, c"frame");
            let target = CGRect {
                x: frame.x + 20.,
                y: frame.y + 20.,
                width: current.width.min(frame.width - 40.),
                height: current.height.min(frame.height - 40.),
            };
            send!((), ns_window, c"setFrame:display:animate:", target => CGRect, true => bool, false => bool);
            return format!("moved to display {id}: {}x{} at ({}, {})", target.width, target.height, target.x, target.y);
        }
    }
    if wanted.is_some() {
        out.push("error: no such display".into());
    }
    out.join("\n")
}
