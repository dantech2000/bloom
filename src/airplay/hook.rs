// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the app meets the sender: an item goes to the receiver instead of
//! the local player, and the debug channel drives every part of it.

use gpui_kit::{Context, Window};

use super::{AirPlayState, SendRequest, StreamOptions, picker};
use crate::{app::Bloom, jellyfin::Item};

impl Bloom {
    /// Sends an item to the receiver. The local player stops: one item
    /// plays at a time, and it is the one on the receiver.
    pub fn airplay_send(&mut self, item: &Item, start_secs: f64, cx: &mut Context<Self>) -> Option<u64> {
        let client = self.session.as_ref()?.client.clone();
        if self.player_open {
            self.player.stop();
            self.close_player_view(cx);
        }
        self.playing = Some(item.clone());
        Some(self.airplay.send(SendRequest {
            client,
            item_id: item.id.clone(),
            title: item.display_title(),
            start_secs,
            options: StreamOptions::new(),
        }))
    }

    /// Debug channel: `airplay <verb> [args]`.
    pub fn debug_airplay(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        match verb {
            // Names from the Bonjour browse; listens only.
            "routes" => {
                let names = self.airplay.routes();
                self.airplay.detect_routes();
                let status = self.airplay.status();
                let system = match status.multiple_routes {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                };
                if let Some(err) = self.airplay.routes_error() {
                    return format!("error: {err}");
                }
                format!("routes={} | system sees routes: {system}", names.join(", "))
            }
            // Shows the system route picker at a rectangle of the window:
            // "picker [x y w h]" or "picker off". A click on it lists the
            // routes; a route picked there starts the receiver.
            "picker" => {
                if arg == "off" {
                    picker::hide();
                    return "picker off".into();
                }
                let numbers: Vec<f32> = arg.split_whitespace().filter_map(|n| n.parse().ok()).collect();
                let place = match numbers[..] {
                    [x, y, w, h] => [x, y, w, h],
                    _ => [24., 60., 44., 44.],
                };
                let player = self.airplay.player_id();
                if player.is_null() {
                    return "error: no player".into();
                }
                if picker::show(window, player, place) {
                    format!("picker at {place:?}")
                } else {
                    "error: picker not shown".into()
                }
            }
            // "send <item id> [seconds]": fetches the item, then sends it.
            "send" => {
                let (id, secs) = arg.split_once(' ').unwrap_or((arg, ""));
                if id.is_empty() {
                    return "error: send <item id> [seconds]".into();
                }
                let start: f64 = secs.trim().parse().unwrap_or(0.);
                let id = id.to_string();
                self.fetch(
                    cx,
                    move |client| client.item(&id),
                    move |this, result, cx| match result {
                        Ok(item) => {
                            this.airplay_send(&item, start, cx);
                        }
                        Err(err) => this.toast("AirPlay", format!("{err:#}"), cx),
                    },
                );
                "sending".into()
            }
            "play" => {
                self.airplay.play();
                "play".into()
            }
            "pause" => {
                self.airplay.pause();
                "pause".into()
            }
            "seek" => match arg.trim().parse::<f64>() {
                Ok(secs) => {
                    self.airplay.seek(secs);
                    format!("seek {secs}")
                }
                Err(_) => "error: seek <seconds>".into(),
            },
            "stop" => {
                self.airplay.stop();
                "stop".into()
            }
            "disconnect" => {
                self.airplay.disconnect();
                "disconnect".into()
            }
            "volume" => match arg.trim().parse::<f32>() {
                Ok(volume) => {
                    self.airplay.set_volume(volume);
                    format!("volume {volume}")
                }
                Err(_) => "error: volume <0..1>".into(),
            },
            "mute" => {
                let muted = arg.trim() != "off";
                self.airplay.set_muted(muted);
                format!("muted={muted}")
            }
            "" | "state" => {
                let s = self.airplay.status();
                let age = s.position_at.map(|at| at.elapsed().as_millis()).unwrap_or(0);
                format!(
                    "state={} item={} position={:.2} ({age} ms ago) duration={:.2} external={} route={} \
                     picker={} error={} session={} volume={:.2} muted={}",
                    s.state.name(),
                    s.item_id.as_deref().unwrap_or("-"),
                    s.position,
                    s.duration,
                    s.external,
                    s.route.as_deref().unwrap_or("-"),
                    picker::shown(),
                    s.error.as_deref().unwrap_or("-"),
                    s.play_session_id.as_deref().map(|id| &id[..id.len().min(8)]).unwrap_or("-"),
                    s.volume,
                    s.muted,
                )
            }
            "latency" => {
                let s = self.airplay.status();
                let load = s
                    .load_latency
                    .map(|d| format!("{} ms", d.as_millis()))
                    .unwrap_or_else(|| if s.state == AirPlayState::Loading { "loading".into() } else { "-".into() });
                let control = s
                    .control_latency
                    .map(|(name, d)| format!("{name} {} ms", d.as_millis()))
                    .unwrap_or_else(|| "-".into());
                format!("load={load} control={control} poll=250 ms")
            }
            _ => "error: airplay routes|picker [x y w h|off]|send <item id> [seconds]|play|pause|seek <s>|stop|\
                  disconnect|volume <0..1>|mute [off]|state|latency. \
                  Act on the receiver once a route is picked: send, play, pause, seek, volume, mute (and the \
                  picker's menu picks the route). Safe anywhere: routes, state, latency, stop, disconnect."
                .into(),
        }
    }
}
