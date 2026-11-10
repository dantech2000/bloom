// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `cast` command of the debug channel: the target of "Play on", as
//! the panel has it, driven and read with no window. `chromecast`
//! (`src/chromecast/debug.rs`) drives a cast device below the target.

use gpui_kit::{Context, Window};

use super::{
    protocol::{PlayCommand, SessionInfo},
    target::{Kind, Target},
};
use crate::app::{Bloom, Panel};

impl Bloom {
    /// One line about the target, for the debug channel.
    pub fn cast_describe(&self) -> String {
        let view = self.target_view();
        let picker = match self.cast.picker_place {
            Some([x, y, w, h]) => format!("{x:.0},{y:.0} {w:.0}x{h:.0}"),
            None => "off".into(),
        };
        match &self.cast.target {
            Target::Local => format!(
                "kind=local target=none sessions={} chromecast_devices={} airplay_routes={} searching={} subscribed={} panel={} picker={} error={:?}",
                self.cast.sessions.len(),
                self.chromecast_devices().len(),
                self.airplay.routes().len(),
                self.cast.searching(),
                self.cast.subscribed,
                self.panel_is(Panel::Cast),
                picker,
                self.cast.error
            ),
            target => format!(
                "kind={} target={} name={:?} detail={:?} connected={} loaded={} title={:?} item={:?} position={:.1} duration={:.1} paused={} volume={:?} muted={} audio={:?} subtitle={:?} seek={} skip={} tracks={} subscribed={} panel={} picker={} error={:?}",
                target.kind().name(),
                match target {
                    Target::JellyfinSession(s) => s.id.clone(),
                    Target::Chromecast(d) => d.id.clone(),
                    _ => "airplay".into(),
                },
                view.name,
                view.detail,
                view.connected,
                view.loaded,
                view.title,
                view.item_id,
                view.position,
                view.duration,
                view.paused,
                view.volume,
                view.muted,
                view.audio_index,
                view.subtitle_index,
                view.can_seek,
                view.can_skip,
                view.can_tracks,
                self.cast.subscribed,
                self.panel_is(Panel::Cast),
                picker,
                view.error.or(self.cast.error.clone()),
            ),
        }
    }

    /// `cast <verb> [arg]`. Reads of the server come back later: `list`
    /// and `caps` print what the last read gave, and start a new one.
    pub fn debug_cast(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        let arg = arg.trim();
        match verb {
            "" | "state" => return self.cast_describe(),
            // This player, as a target sees it.
            "local" => {
                let tracks: Vec<String> = self
                    .player_status
                    .tracks
                    .iter()
                    .filter(|t| t.kind != "video")
                    .map(|t| format!("{}{}:{}{}", t.kind, t.id, t.label(), if t.selected { "*" } else { "" }))
                    .collect();
                return format!(
                    "player={:?} playing={:?} position={:.1} paused={} volume={} muted={} tracks=[{}] fullscreen={} allowed={}",
                    self.player_status.state,
                    self.playing.as_ref().map(|i| i.display_title()),
                    self.player_status.position,
                    self.player_status.paused,
                    self.volume,
                    self.muted,
                    tracks.join(" "),
                    window.is_fullscreen(),
                    self.remote_control_allowed(),
                );
            }
            "panel" => self.toggle_cast_panel(window, cx),
            "list" => {
                self.load_cast_sessions(cx);
                let lines: Vec<String> = self
                    .cast
                    .sessions
                    .iter()
                    .map(|s| {
                        format!(
                            "{} user={:?} device={:?} client={:?} playing={:?}",
                            s.id, s.user_name, s.device_name, s.client, s.now_playing_title()
                        )
                    })
                    .collect();
                return if lines.is_empty() {
                    format!("no sessions yet (loading={})", self.cast.loading)
                } else {
                    lines.join("\n")
                };
            }
            // Every section of the panel, as it would list them now.
            "devices" => {
                self.chromecast.keep_search = true;
                self.start_cast_search(cx);
                let mut lines = vec![format!("this device: {}", if self.cast.target.is_local() { "chosen" } else { "-" })];
                for s in &self.cast.sessions {
                    lines.push(format!("jellyfin: {} device={:?} client={:?}", s.id, s.device_name, s.client));
                }
                for d in self.chromecast_devices() {
                    lines.push(format!("chromecast: {} name={:?} model={:?} at {}:{}", d.id, d.name, d.model, d.address, d.port));
                }
                if self.cast.searching() {
                    lines.push("chromecast: searching".into());
                }
                let routes = self.airplay.routes();
                lines.push(format!(
                    "airplay: row={} routes={:?} picker={}",
                    !routes.is_empty(),
                    routes,
                    self.cast.picker_place.map_or("off".to_string(), |p| format!("{p:?}"))
                ));
                return lines.join("\n");
            }
            // Only a session of a test account may be driven by a test.
            "to" => {
                let Some(session) = self.cast.sessions.iter().find(|s| s.id == arg) else {
                    return format!("error: no controllable session {arg:?}; run `cast list` first");
                };
                if !session.user_name.starts_with("jellyui-test") {
                    return "error: only sessions of a jellyui-test user".into();
                }
                if let Err(err) = self.cast_to(arg, cx) {
                    return format!("error: {err}");
                }
            }
            // A cast device by part of its name, its id or its address:
            // connects and reads its state; nothing plays until `play`.
            "chromecast" => {
                let device = match self.chromecast_find(arg, cx) {
                    Ok(device) => device,
                    Err(text) => return text,
                };
                self.select_target(Target::Chromecast(device), cx);
            }
            // Opens the panel with the AirPlay row; the user picks the
            // route in the system picker there.
            "airplay" => {
                if !self.panel_is(Panel::Cast) {
                    self.toggle_cast_panel(window, cx);
                }
                let routes = self.airplay.routes();
                return format!(
                    "panel={} airplay_row={} routes={:?} picker={}",
                    self.panel_is(Panel::Cast),
                    !routes.is_empty(),
                    routes,
                    self.cast.picker_place.map_or("pending".to_string(), |p| format!("{p:?}"))
                );
            }
            "off" => self.cast_off(cx),
            "here" => self.cast_play_here(window, cx),
            // `play <item id>`: on a Jellyfin session by its id; a cast
            // device or AirPlay needs the item first.
            "play" => {
                if !self.cast.active() {
                    return "error: no target".into();
                }
                if self.cast.kind() == Kind::Jellyfin {
                    self.cast_send_play(vec![arg.to_string()], PlayCommand::PlayNow, None, cx);
                } else {
                    let id = arg.to_string();
                    self.fetch(
                        cx,
                        move |client| client.item(&id),
                        move |this, result, cx| match result {
                            Ok(item) => {
                                this.cast_play(std::slice::from_ref(&item), false, cx);
                            }
                            Err(err) => this.toast("Play On", format!("{err:#}"), cx),
                        },
                    );
                    return "loading the item".into();
                }
            }
            // The queue of the target: `queue <item id>`, `queue-next <item id>`.
            "queue" | "queue-next" => {
                if self.cast.kind() != Kind::Jellyfin {
                    return "error: only a Jellyfin session has a queue".into();
                }
                self.cast_enqueue(arg.to_string(), verb == "queue-next", cx);
            }
            "pause" => self.target_set_paused(true, cx),
            "unpause" => self.target_set_paused(false, cx),
            "toggle" => self.target_toggle_pause(cx),
            "stop" => self.target_stop(cx),
            "next" => self.target_skip(true, cx),
            "previous" => self.target_skip(false, cx),
            "seek" => match arg.parse::<f64>() {
                Ok(secs) => self.target_seek(secs, cx),
                Err(_) => return "error: usage: cast seek <seconds>".into(),
            },
            "volume" => match arg.parse::<f32>() {
                Ok(volume) => self.target_set_volume(volume, cx),
                Err(_) => return "error: usage: cast volume <0-100>".into(),
            },
            "mute" => self.target_toggle_mute(cx),
            // A text the target shows as a toast.
            "message" => self.cast_general(
                "DisplayMessage",
                &[("Header", crate::brand::NAME.to_string()), ("Text", arg.to_string())],
                cx,
            ),
            "audio" | "subtitle" => match arg.parse::<i64>() {
                Ok(index) => self.target_set_track(if verb == "audio" { "Audio" } else { "Subtitle" }, index, cx),
                Err(_) => return format!("error: usage: cast {verb} <stream index>"),
            },
            "caps" => {
                let device_id = self.config.device_id.clone();
                self.fetch(
                    cx,
                    move |client| {
                        let sessions: Vec<SessionInfo> =
                            client.get("/Sessions", &[("deviceId", device_id)])?;
                        Ok(sessions
                            .into_iter()
                            .map(|s| {
                                format!(
                                    "server: session={} user={:?} supports_remote_control={} supports_media_control={} commands={:?}",
                                    s.id,
                                    s.user_name,
                                    s.supports_remote_control,
                                    s.supports_media_control,
                                    s.capabilities.map(|c| c.supported_commands).unwrap_or_default()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" | "))
                    },
                    |this, result, cx| {
                        this.cast.own = Some(match result {
                            Ok(text) if text.is_empty() => "server: no session of this device".into(),
                            Ok(text) => text,
                            Err(err) => format!("server: error {err:#}"),
                        });
                        cx.notify();
                    },
                );
                return format!(
                    "allowed={} posted={} socket={} | {}",
                    self.remote_control_allowed(),
                    self.cast.capabilities_posted,
                    self.sync.session.as_ref().is_some_and(|s| s.connected),
                    self.cast.own.as_deref().unwrap_or("server: not read yet; ask again")
                );
            }
            "allow" => {
                if (arg == "on") != self.remote_control_allowed() {
                    self.toggle_remote_control(cx);
                }
            }
            _ => {
                return "error: cast state|local|panel|list|devices|to <session id>|chromecast <device>|airplay|off|here|\
                        play <item id>|queue <item id>|queue-next <item id>|pause|unpause|toggle|seek <s>|stop|next|previous|\
                        volume <0-100>|mute|audio <index>|subtitle <index>|message <text>|caps|allow <on|off>"
                    .into();
            }
        }
        cx.notify();
        self.cast_describe()
    }
}
