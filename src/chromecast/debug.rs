// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `chromecast` command of the debug channel: drives the engine with
//! no panel. The verbs marked "acts on the device" start or change what
//! the TV shows; the others only look.

use std::time::{Duration, Instant};

use gpui_kit::{Context, Window};
use serde_json::Value;

use super::{
    jellyfin::{self, Identity, ItemStub},
    mdns::{self, Device},
    messages::{self, LoadMedia},
    session::{Event, JellyfinLoad, LoadRequest, Session},
};
use crate::app::Bloom;

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
                let discovery = self.chromecast.discovery.get_or_insert_with(mdns::discover);
                discovery.rescan();
                let devices = discovery.devices();
                if devices.is_empty() {
                    return "scanning; no device yet, ask again".into();
                }
                devices
                    .iter()
                    .map(|d| format!("{} | {} | {} | {}:{}", d.name, d.model, d.id, d.address, d.port))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "connect" => match self.cast_connect(arg, cx) {
                Ok(()) => self.cast_state(),
                Err(text) => text,
            },
            "disconnect" => {
                self.chromecast.session = None;
                self.chromecast.task = None;
                "disconnected".into()
            }
            "status" => {
                if !arg.is_empty()
                    && let Err(text) = self.cast_connect(arg, cx)
                {
                    return text;
                }
                let Some(session) = &self.chromecast.session else {
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
                let Some(session) = &self.chromecast.session else {
                    return "error: connect first".into();
                };
                let started = Instant::now();
                let answer = session.get_status();
                match answer.recv_timeout(ANSWER) {
                    Ok(_) => format!("{:.1} ms", started.elapsed().as_secs_f64() * 1000.),
                    Err(_) => "error: no answer".into(),
                }
            }
            "state" => self.cast_state(),
            "launch" => {
                let Some(session) = &self.chromecast.session else {
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
                if self.chromecast.session.is_none() {
                    return "error: connect first".into();
                }
                let start_secs = start.trim().parse::<f64>().unwrap_or(0.);
                let id = id.to_string();
                self.fetch(
                    cx,
                    move |client| client.item(&id),
                    move |this, result, cx| match result {
                        Ok(item) => this.cast_load(&item, start_secs, cx),
                        Err(err) => this.toast("Could not load the item", format!("{err:#}"), cx),
                    },
                );
                "loading".into()
            }
            "play" => self.cast_do(|s| s.play()),
            "pause" => self.cast_do(|s| s.pause()),
            "seek" => match arg.parse::<f64>() {
                Ok(secs) => self.cast_do(|s| s.seek(secs)),
                Err(_) => "error: seek <seconds>".into(),
            },
            "volume" => match arg.parse::<f64>() {
                Ok(level) => self.cast_do(|s| s.set_volume(level.clamp(0., 100.) / 100.)),
                Err(_) => "error: volume <0-100>".into(),
            },
            "mute" => self.cast_do(|s| s.set_muted(arg == "on")),
            "audio" => match arg.parse::<i64>() {
                Ok(index) => self.cast_do(|s| s.set_audio(index)),
                Err(_) => "error: audio <stream index>".into(),
            },
            "subtitle" => {
                let track = arg.parse::<i64>().ok();
                self.cast_do(|s| s.set_subtitle(track))
            }
            "stop" => self.cast_do(|s| s.stop_media()),
            "stop-app" => self.cast_do(|s| s.stop_app()),
            "join" => self.cast_do(|s| s.join()),
            // Asks the app for its media status; `state` shows the answer.
            "media-status" => self.cast_do(|s| s.media_status()),
            // Any request, to the app when one is connected, else to the
            // device: `raw urn:x-cast:com.google.cast.receiver {"type":"GET_STATUS"}`.
            "raw" => {
                let (namespace, json) = arg.split_once(' ').unwrap_or((arg, "{}"));
                let Ok(payload) = serde_json::from_str::<Value>(json.trim()) else {
                    return "error: raw <namespace> <json>".into();
                };
                let Some(session) = &self.chromecast.session else {
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
    fn cast_do(&mut self, act: impl FnOnce(&Session)) -> String {
        match &self.chromecast.session {
            Some(session) => {
                act(session);
                "sent".into()
            }
            None => "error: connect first".into(),
        }
    }

    /// The device named by part of its name or model, its id or its address.
    fn cast_device(&mut self, name: &str) -> Result<Device, String> {
        let discovery = self.chromecast.discovery.get_or_insert_with(mdns::discover);
        let devices = discovery.devices();
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
    fn cast_connect(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let device = self.cast_device(name)?;
        if self.chromecast.session.as_ref().is_some_and(|s| s.device().id == device.id) {
            return Ok(());
        }
        let (session, events) = Session::connect(device);
        self.chromecast.session = Some(session);
        self.chromecast.events.clear();
        self.chromecast.task = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let fed = this.update(cx, |this, cx| {
                    this.chromecast.note(&event);
                    if let Event::Error(text) = &event {
                        log::warn!("cast: {text}");
                    }
                    cx.notify();
                });
                if fed.is_err() {
                    break;
                }
            }
        }));
        Ok(())
    }

    /// Plays an item on the device, both ways in one request.
    fn cast_load(&mut self, item: &crate::jellyfin::Item, start_secs: f64, cx: &mut Context<Self>) {
        let (Some(session), Some(account)) = (&self.chromecast.session, &self.session) else {
            return;
        };
        let client = &account.client;
        let jellyfin = Identity::from_client(client, &account.server_id, "").map(|identity| JellyfinLoad {
            identity,
            items: vec![ItemStub::from_item(item)],
            start_secs,
            audio_index: None,
            subtitle_index: None,
        });
        let play_session_id = uuid::Uuid::new_v4().simple().to_string();
        let media = jellyfin::stream_url(client, &item.id, start_secs, &play_session_id).map(|url| LoadMedia {
            url,
            content_type: jellyfin::STREAM_CONTENT_TYPE.into(),
            title: item.display_title(),
            subtitle: item.series_name.clone().unwrap_or_default(),
            image_url: Some(client.image_url(&item.id, "Primary", None, 480)),
            start_secs: 0.,
            duration: item.run_time_ticks.map(|t| t as f64 / crate::jellyfin::TICKS_PER_SECOND as f64),
            tracks: Vec::new(),
            active_tracks: Vec::new(),
            live: false,
        });
        session.load(LoadRequest { jellyfin, media });
        cx.notify();
    }

    fn cast_state(&self) -> String {
        let Some(session) = &self.chromecast.session else {
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
            if status.player_state.is_empty() { "-" } else { &status.player_state },
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
