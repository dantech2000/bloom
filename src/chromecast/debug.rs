// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `chromecast` command of the debug channel: drives the engine with
//! no panel. The verbs marked "acts on the device" start or change what
//! the TV shows; the others only look.
//!
//! `connect` and `status` open the connection the panel uses
//! (`Bloom::chromecast_open`) and do not make the device the target: a
//! look at a TV must not send the next play of the app to it. `cast
//! chromecast <device>` (`src/cast/debug.rs`) chooses it as the target.

use std::time::{Duration, Instant};

use gpui_kit::{Context, Window};
use serde_json::Value;

use super::{jellyfin, discovery::Device, messages, session::Session};
use crate::{app::Bloom, cast::target::Kind};

const HELP: &str = "chromecast scan | status <device> | latency | state | media-status | connect <device> | disconnect | \
    launch [app id|default] | load <item id> [start secs] | play | pause | seek <s> | volume <0-100> | mute <on|off> | \
    audio <index> | subtitle <index|off> | stop | stop-app | join | raw <namespace> <json>\n\
    look only: scan, status, latency, state, media-status, connect, disconnect\n\
    act on the device: launch, load, play, pause, seek, volume, mute, audio, subtitle, stop, stop-app, join, raw";

/// How long a debug verb waits for the answer of the device. This is the
/// one place that waits on the UI thread; it is test tooling.
const ANSWER: Duration = Duration::from_millis(2500);

impl Bloom {
    pub fn debug_chromecast(&mut self, rest: &str, _window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        let arg = arg.trim();
        match verb {
            "scan" => {
                self.debug_cast_search(cx);
                let devices = self.chromecast_devices();
                if devices.is_empty() {
                    return "scanning; no device yet, ask again".into();
                }
                devices
                    .iter()
                    .map(|d| format!("{} | {} | {} | {}:{}", d.name, d.model, d.id, d.address, d.port))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "connect" => match self.debug_cast_connect(arg, cx) {
                Ok(()) => self.chromecast_state(),
                Err(text) => text,
            },
            // A device that is the target is left as the panel leaves it.
            "disconnect" => {
                if self.cast.kind() == Kind::Chromecast {
                    self.cast_off(cx);
                } else {
                    self.chromecast.probe = None;
                }
                "disconnected".into()
            }
            "status" => {
                if !arg.is_empty()
                    && let Err(text) = self.debug_cast_connect(arg, cx)
                {
                    return text;
                }
                let Some(session) = self.chromecast_link().map(|open| &open.session) else {
                    return "error: status <device>".into();
                };
                let apps = vec![
                    jellyfin::APP_STABLE.to_string(),
                    jellyfin::APP_UNSTABLE.to_string(),
                    messages::APP_DEFAULT_MEDIA.to_string(),
                ];
                let status = session.get_status();
                let availability = session.app_availability(apps);
                let Ok(status) = status.recv_timeout(ANSWER) else {
                    return format!("error: no status answer from {}", session.device().name);
                };
                let availability = availability.recv_timeout(ANSWER).unwrap_or(Value::Null);
                let apps = status["status"]["applications"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|app| {
                        format!(
                            "{} {:?} idle_screen={} status={:?}",
                            app["appId"].as_str().unwrap_or(""),
                            app["displayName"].as_str().unwrap_or(""),
                            app["isIdleScreen"].as_bool().unwrap_or(false),
                            app["statusText"].as_str().unwrap_or(""),
                        )
                    })
                    .collect::<Vec<_>>();
                format!(
                    "device={:?} apps=[{}] volume={} muted={} availability={}",
                    session.device().name,
                    apps.join("; "),
                    status["status"]["volume"]["level"],
                    status["status"]["volume"]["muted"],
                    availability["availability"],
                )
            }
            "latency" => {
                let Some(session) = self.chromecast_link().map(|open| &open.session) else {
                    return "error: connect first".into();
                };
                let started = Instant::now();
                let answer = session.get_status();
                match answer.recv_timeout(ANSWER) {
                    Ok(_) => format!("{:.1} ms", started.elapsed().as_secs_f64() * 1000.),
                    Err(_) => "error: no answer".into(),
                }
            }
            "state" => self.chromecast_state(),
            "launch" => {
                let Some(session) = self.chromecast_link().map(|open| &open.session) else {
                    return "error: connect first".into();
                };
                let ids = match arg {
                    "" => jellyfin::app_ids(),
                    "default" => vec![messages::APP_DEFAULT_MEDIA.to_string()],
                    id => vec![id.to_string()],
                };
                session.launch(ids);
                "launching".into()
            }
            "load" => {
                let (id, start) = arg.split_once(' ').unwrap_or((arg, "0"));
                if id.is_empty() {
                    return "error: load <item id> [start secs]".into();
                }
                if self.chromecast_link().is_none() {
                    return "error: connect first".into();
                }
                let start_secs = start.trim().parse::<f64>().unwrap_or(0.);
                let id = id.to_string();
                self.fetch(
                    cx,
                    move |client| client.item(&id),
                    move |this, result, cx| match result {
                        Ok(item) => this.chromecast_load(std::slice::from_ref(&item), start_secs, cx),
                        Err(err) => this.toast("Could not load the item", format!("{err:#}"), cx),
                    },
                );
                "loading".into()
            }
            "play" => self.chromecast_send(|s| s.play()),
            "pause" => self.chromecast_send(|s| s.pause()),
            "seek" => match arg.parse::<f64>() {
                Ok(secs) => self.chromecast_send(|s| s.seek(secs)),
                Err(_) => "error: seek <seconds>".into(),
            },
            "volume" => match arg.parse::<f64>() {
                Ok(level) => self.chromecast_send(|s| s.set_volume(level.clamp(0., 100.) / 100.)),
                Err(_) => "error: volume <0-100>".into(),
            },
            "mute" => self.chromecast_send(|s| s.set_muted(arg == "on")),
            "audio" => match arg.parse::<i64>() {
                Ok(index) => self.chromecast_send(|s| s.set_audio(index)),
                Err(_) => "error: audio <stream index>".into(),
            },
            "subtitle" => {
                let track = arg.parse::<i64>().ok();
                self.chromecast_send(|s| s.set_subtitle(track))
            }
            "stop" => self.chromecast_send(|s| s.stop_media()),
            "stop-app" => self.chromecast_send(|s| s.stop_app()),
            "join" => self.chromecast_send(|s| s.join()),
            // Asks the app for its media status; `state` shows the answer.
            "media-status" => self.chromecast_send(|s| s.media_status()),
            // Any request, to the app when one is connected, else to the
            // device: `raw urn:x-cast:com.google.cast.receiver {"type":"GET_STATUS"}`.
            "raw" => {
                let (namespace, json) = arg.split_once(' ').unwrap_or((arg, "{}"));
                let Ok(payload) = serde_json::from_str::<Value>(json.trim()) else {
                    return "error: raw <namespace> <json>".into();
                };
                let Some(session) = self.chromecast_link().map(|open| &open.session) else {
                    return "error: connect first".into();
                };
                match session.request(namespace, payload).recv_timeout(ANSWER) {
                    Ok(answer) => answer.to_string(),
                    Err(_) => "no answer".into(),
                }
            }
            _ => format!("error: {HELP}"),
        }
    }

    /// Runs a verb that acts on the device.
    fn chromecast_send(&mut self, act: impl FnOnce(&Session)) -> String {
        match self.chromecast_link() {
            Some(open) => {
                act(&open.session);
                "sent".into()
            }
            None => "error: connect first".into(),
        }
    }

    /// Starts the search for a test: it stays with the panel closed, so
    /// the next ask finds what answered meanwhile.
    fn debug_cast_search(&mut self, cx: &mut Context<Self>) {
        self.chromecast.keep_search = true;
        self.start_cast_search(cx);
    }

    /// The device named by part of its name or model, its id or its
    /// address, among the ones the search found (and the fake one of a
    /// test).
    pub(crate) fn chromecast_find(&mut self, name: &str, cx: &mut Context<Self>) -> Result<Device, String> {
        self.debug_cast_search(cx);
        let devices = self.chromecast_devices();
        let wanted = name.to_lowercase();
        devices
            .iter()
            .find(|d| {
                d.name.to_lowercase().contains(&wanted)
                    || d.model.to_lowercase().contains(&wanted)
                    || d.id == name
                    || d.address.to_string() == name
            })
            .cloned()
            .ok_or_else(|| {
                if devices.is_empty() {
                    "error: no device found yet; run scan and ask again".into()
                } else {
                    format!("error: no device named {name:?}")
                }
            })
    }

    /// Opens the connection to a device, unless it is open already.
    fn debug_cast_connect(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let device = self.chromecast_find(name, cx)?;
        self.chromecast_open(device, cx);
        Ok(())
    }

    fn chromecast_state(&self) -> String {
        let Some(session) = self.chromecast_link().map(|open| &open.session) else {
            return "no session".into();
        };
        let status = session.status();
        let device = session.device();
        let app = status
            .app
            .as_ref()
            .map_or("none".to_string(), |app| format!("{}/{:?}/{}", app.app_id, app.display_name, app.transport_id));
        format!(
            "device={:?} address={}:{} connected={} app={app} volume={:.2} muted={} player={} pos={:.1} duration={} idle={} \
             item={} content={} tracks={:?} rtt={} error={:?} | events: {}",
            device.name,
            device.address,
            device.port,
            status.connected,
            status.volume,
            status.muted,
            status.player_state.map_or("-", |state| state.name()),
            status.position_now(),
            status.duration.map_or("-".to_string(), |d| format!("{d:.1}")),
            status.idle_reason.as_deref().unwrap_or("-"),
            status.item_id.as_deref().unwrap_or("-"),
            status.content_id.as_deref().map_or("-".to_string(), |c| c.split('?').next().unwrap_or("").to_string()),
            status.active_tracks,
            status.rtt_ms.map_or("-".to_string(), |ms| format!("{ms:.1}ms")),
            status.error,
            self.chromecast.events.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join("; "),
        )
    }
}
