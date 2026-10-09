// SPDX-License-Identifier: AGPL-3.0-or-later
//! The lights (experimental). Bloom sends nothing to a light itself: a
//! helper program, `bloom-lights` (the crate in `lights/`), reads the
//! events of Bloom and tells the lights. This is the part of Bloom for it:
//! the helper is started and ended here, the lights it finds are listed
//! and chosen here, and its values are set here and written to the
//! settings file it reads (`lights.json`). A development instance has a
//! test film as well, to see the lights with no film.

use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
};
use std::time::Duration;

use gpui_kit::{AppContext as _, Context, Entity, Task};
use serde_json::{Value, json};

use super::{ItemInfo, Snapshot, colours::hue_colour, hub, socket_path};
use crate::{
    app::Bloom,
    ui::slider::{SliderEvent, SliderState},
};

/// A number a slider can have: "NaN" and "inf" are numbers to the parser,
/// and a slider with one of them cannot be drawn.
fn finite(text: &str) -> Option<f32> {
    text.parse::<f32>().ok().filter(|value| value.is_finite())
}

/// The helper, next to the program of Bloom (in the app, and in the folder
/// of a build). None for a build without it.
fn helper_path() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join("bloom-lights");
    // The file itself, not a link to a file somewhere else.
    std::fs::symlink_metadata(&path).is_ok_and(|file| file.is_file()).then_some(path)
}

/// The helper while it runs. It ends when its pipe from Bloom closes: when
/// this is dropped, and when Bloom quits or is killed. It gives the lights
/// back what they had before it ends.
pub(super) struct Helper(Option<Child>);

impl Helper {
    fn start() -> std::io::Result<Self> {
        let path = helper_path().ok_or_else(|| std::io::Error::other("this build has no lights helper"))?;
        let child = Command::new(path)
            .args(["--with-parent", "--socket", &socket_path(), "--settings", &lights_path()])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;
        Ok(Self(Some(child)))
    }
}

impl Helper {
    /// Whether the helper ended by itself.
    fn ended(&mut self) -> bool {
        self.0.as_mut().is_none_or(|child| !matches!(child.try_wait(), Ok(None)))
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else { return };
        // The end of the pipe tells it to put the lights back and to leave.
        drop(child.stdin.take());
        // It is waited for away from the UI thread (the lights glide back
        // for a second or two), and ended by force when it does not leave.
        let _ = std::thread::Builder::new().name("lights-helper".into()).spawn(move || {
            for _ in 0..100 {
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

/// The lights a run of `bloom-lights --scan` printed, one line of JSON
/// each. The names in it come from the lights themselves: a line that is no
/// light is left out, a text is cut to a length a page can show, and the
/// list ends at 32 lights.
fn found_lines(text: &str) -> Vec<Value> {
    let lamp = |line: &str| {
        let value: Value = serde_json::from_str(line).ok()?;
        let part = |name: &str| value[name].as_str().map(|text| text.chars().take(64).collect::<String>());
        let (driver, address) = (part("driver").filter(|text| !text.is_empty())?, part("address").filter(|text| !text.is_empty())?);
        // The id is how a light is known again: it is kept whole, and a
        // light with an id of no sane length is left out.
        let id = value["id"].as_str().unwrap_or_default();
        (id.len() <= 256).then(|| json!({ "driver": driver, "id": id, "model": part("model").unwrap_or_default(), "address": address }))
    };
    text.lines().filter_map(lamp).take(32).collect()
}

/// Whether two entries are one light: by the name it has for good, or by
/// its address when it has no such name.
fn same_lamp(a: &Value, b: &Value) -> bool {
    let id = |lamp: &Value| lamp["id"].as_str().filter(|id| !id.is_empty()).map(str::to_string);
    a["driver"] == b["driver"]
        && match (id(a), id(b)) {
            (Some(a), Some(b)) => a == b,
            _ => a["address"] == b["address"],
        }
}

/// The settings file of the helper, next to the socket: `lights.json`, or
/// `lights-<name>.json` for a test instance. The settings of Bloom write it.
pub fn lights_path() -> String {
    let socket = socket_path();
    let path = std::path::Path::new(&socket);
    let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("events").replacen("events", "lights", 1);
    path.with_file_name(format!("{name}.json")).display().to_string()
}

/// What a film does to the light: the choice of the lights lab, as the
/// lights example reads it from its settings file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilmColour {
    Keep,
    Artwork,
    Fixed,
}

/// The lights of the settings: the helper, the lights it uses, and its
/// values.
pub struct Lights {
    pub(super) film: Entity<SliderState>,
    pub(super) pause: Entity<SliderState>,
    pub(super) credits: Entity<SliderState>,
    pub(super) gap: Entity<SliderState>,
    pub(super) kelvin: Entity<SliderState>,
    pub(super) hue: Entity<SliderState>,
    /// The time the light takes to come to a new colour of the picture.
    pub(super) glide: Entity<SliderState>,
    /// The light follows the colours of the picture.
    pub(super) aura: bool,
    /// A pause shows white of `kelvin`; off, it keeps the colour.
    pub(super) white: bool,
    pub(super) colour: FilmColour,
    /// The write of the settings file that waits for a slider to rest.
    pub(super) write: Option<Task<()>>,
    /// The helper, while the lights are on.
    helper: Option<Helper>,
    /// The lights Bloom may use, as the helper reads them.
    lamps: Vec<Value>,
    /// The lights the last look on the network found.
    found: Vec<Value>,
    /// A look on the network runs.
    pub(super) scanning: bool,
}

/// The state of a slider of the lights. The upper bound goes in first: a
/// fresh state ends at 100, and a lower bound above its end is refused.
pub(super) fn lab_slider(min: f32, max: f32, step: f32, value: f32) -> SliderState {
    SliderState::new().max(max.max(100.)).min(min).max(max).step(step).default_value(value.clamp(min, max))
}

impl Bloom {
    /// Sends the events of a short film to the readers, 3 seconds apart:
    /// start, the colour of the artwork, pause, resume, credits, end. For
    /// a program that reads the events, to try what it does with no film.
    /// Every event of it has `"test": true` and the session 0.
    pub fn events_test_run(&mut self, cx: &mut Context<Self>) -> &'static str {
        if !hub().on() {
            return "the stream is off";
        }
        if self.events.last.active {
            return "a playback runs";
        }
        let mut film = Snapshot {
            active: true,
            item: Some(ItemInfo { id: "test".into(), kind: "Movie".into(), name: "Test film".into(), ..ItemInfo::default() }),
            duration: 5400.,
            rate: 1.,
            fullscreen: true,
            ..Snapshot::default()
        };
        let titles = self.config.events_titles == Some(true);
        let mut playback = |position: f64, paused: bool| {
            (film.position, film.paused) = (position, paused);
            film.playback_json(0, titles)
        };
        let colour = self.events.colour.unwrap_or([0, 120, 255]);
        let mut steps = vec![
            (3000, "playback.started", playback(0., false)),
            (300, "artwork.colour", json!({ "session": 0, "colour": colour })),
            (3000, "playback.paused", playback(3., true)),
            (3000, "playback.resumed", playback(3., false)),
            (3000, "segment.entered", json!({ "session": 0, "kind": "Outro", "position": 5100. })),
            (3000, "playback.stopped", playback(5103., false)),
        ];
        for (_, event, data) in &mut steps {
            data["test"] = json!(true);
            if *event == "playback.stopped" {
                data["reason"] = json!("user");
                data["played_to_end"] = json!(false);
            }
        }
        self.events.test_run = Some(cx.spawn(async move |this, cx| {
            for (wait, event, data) in steps {
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                hub().emit(event, data);
            }
            let _ = this.update(cx, |this, cx| {
                this.events.test_run = None;
                cx.notify();
            });
        }));
        cx.notify();
        "the test film runs"
    }

    /// Makes the lights of the settings, with the values of the settings
    /// file when there is one, and starts the helper when the user had the
    /// lights on. A test instance does not start it by itself: it would be
    /// a second helper on the lights of the app the user runs.
    pub fn start_lights(&mut self, cx: &mut Context<Self>) {
        let file: Value = std::fs::read(lights_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        let number = |name: &str, default: f32| file[name].as_f64().map_or(default, |v| v as f32);
        let mut slider = |min: f32, max: f32, step: f32, value: f32| {
            let state = cx.new(|_| lab_slider(min, max, step, value));
            cx.subscribe(&state, |this, _, _: &SliderEvent, cx| this.lights_write_soon(cx)).detach();
            state
        };
        self.events.lights = Some(Lights {
            film: slider(1., 100., 1., number("film", 5.)),
            pause: slider(1., 100., 1., number("pause", 25.)),
            credits: slider(1., 100., 1., number("credits", 40.)),
            gap: slider(0., 1.5, 0.05, number("gap", 0.4)),
            kelvin: slider(2700., 6500., 100., number("pause_kelvin", 2700.)),
            hue: slider(0., 359., 1., number("hue", 270.)),
            glide: slider(0.02, 0.5, 0.01, number("aura_glide", 0.07)),
            aura: file["aura"].as_bool().unwrap_or(false),
            white: file["pause_white"].as_bool().unwrap_or(true),
            colour: match &file["colour"] {
                Value::String(text) if text == "artwork" => FilmColour::Artwork,
                Value::Array(_) => FilmColour::Fixed,
                _ => FilmColour::Keep,
            },
            write: None,
            helper: None,
            lamps: file["lamps"].as_array().map(|lamps| found_lines(&lamps.iter().map(|lamp| format!("{lamp}\n")).collect::<String>())).unwrap_or_default(),
            found: Vec::new(),
            scanning: false,
        });
        if self.config.lights == Some(true) && hub().on() && std::env::var_os("BLOOM_TEST_NAME").is_none() {
            self.lights_helper(true);
        }
    }

    /// Whether the helper runs.
    pub fn lights_on(&self) -> bool {
        self.events.lights.as_ref().is_some_and(|lights| lights.helper.is_some())
    }

    /// Sees a helper that ended by itself (it does not, but for a fault):
    /// the lights are off then, and the page says so.
    pub(super) fn lights_check(&mut self, cx: &mut Context<Self>) {
        let Some(lights) = &mut self.events.lights else { return };
        if lights.helper.as_mut().is_some_and(Helper::ended) {
            lights.helper = None;
            log::warn!("lights: the helper ended by itself");
            self.toast("Lights", "The lights helper ended. Turn \"Control lights\" on again.", cx);
            cx.notify();
        }
    }

    /// Starts or ends the helper. Its error, when it does not start.
    pub(super) fn lights_helper(&mut self, on: bool) -> Option<String> {
        let lights = self.events.lights.as_mut()?;
        // One that ended by itself is none.
        if lights.helper.as_mut().is_some_and(Helper::ended) {
            lights.helper = None;
        }
        if on == lights.helper.is_some() {
            return None;
        }
        lights.helper = None;
        if on {
            match Helper::start() {
                Ok(helper) => lights.helper = Some(helper),
                Err(err) => return Some(err.to_string()),
            }
        }
        None
    }

    /// The setting "Control lights": the helper runs or not, at once, and
    /// the same at the next start. The lights need the events, and the
    /// colours of the picture for a light that follows it.
    pub fn set_lights(&mut self, on: bool, cx: &mut Context<Self>) {
        if on && !hub().on() {
            self.set_events(true, cx);
        }
        if on && !hub().on() {
            // The socket did not open, and a toast said so.
            return;
        }
        if on && self.events.lights.as_ref().is_some_and(|lights| lights.aura) && self.config.events_colours != Some(true) {
            self.set_events_colours(true, cx);
        }
        // The helper reads the file at its start.
        self.lights_write(cx);
        if let Some(err) = self.lights_helper(on) {
            log::warn!("lights: the helper did not start: {err}");
            self.toast("Lights", format!("The lights helper did not start: {err}"), cx);
        }
        self.config.lights = Some(self.lights_on());
        self.save_config(cx);
        cx.notify();
    }

    /// Looks on the network for lights, with the helper, away from the UI
    /// thread: the list is there about two seconds later.
    pub fn lights_scan(&mut self, cx: &mut Context<Self>) {
        let Some(path) = helper_path() else {
            return self.toast("Lights", "This build has no lights helper.", cx);
        };
        let Some(lights) = self.events.lights.as_mut().filter(|lights| !lights.scanning) else { return };
        lights.scanning = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let output = cx
                .background_executor()
                .spawn(async move { Command::new(path).args(["--scan", "2"]).stdin(Stdio::null()).output() })
                .await;
            let _ = this.update(cx, |this, cx| {
                let found = match &output {
                    Ok(output) => found_lines(&String::from_utf8_lossy(&output.stdout)),
                    Err(err) => {
                        log::warn!("lights: no look for lights: {err}");
                        Vec::new()
                    }
                };
                if found.is_empty() {
                    this.toast("Lights", "No light answered.", cx);
                }
                if let Some(lights) = &mut this.events.lights {
                    (lights.found, lights.scanning) = (found, false);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The lights of the page: the chosen ones first, then the ones the
    /// last look found, each with whether it is chosen.
    pub fn lights_rows(&self) -> Vec<(Value, bool)> {
        let Some(lights) = &self.events.lights else { return Vec::new() };
        let more = lights.found.iter().filter(|found| !lights.lamps.iter().any(|lamp| same_lamp(lamp, found)));
        lights.lamps.iter().map(|lamp| (lamp.clone(), true)).chain(more.map(|lamp| (lamp.clone(), false))).collect()
    }

    /// Takes a light into the ones Bloom may use, or out of them. The
    /// helper reads the file at its next event.
    pub fn lights_choose(&mut self, lamp: &Value, cx: &mut Context<Self>) {
        let Some(lights) = &mut self.events.lights else { return };
        match lights.lamps.iter().position(|chosen| same_lamp(chosen, lamp)) {
            Some(at) => {
                // It stays in the list of the page, to be chosen again.
                let left = lights.lamps.remove(at);
                if !lights.found.iter().any(|found| same_lamp(found, &left)) {
                    lights.found.push(left);
                }
            }
            None => lights.lamps.push(lamp.clone()),
        }
        self.lights_write(cx);
    }

    /// Makes a light blink, to see which one it is.
    pub fn lights_identify(&mut self, lamp: &Value, cx: &mut Context<Self>) {
        // The blink sets the brightness of the light, and puts back what
        // the light has now: next to a film that would fight the helper.
        if self.events.last.active {
            return self.toast("Lights", "A light can blink when no film plays.", cx);
        }
        let (Some(path), Some(driver), Some(address)) = (helper_path(), lamp["driver"].as_str(), lamp["address"].as_str()) else {
            return self.toast("Lights", "This build has no lights helper.", cx);
        };
        match Command::new(path).args(["--identify", driver, address]).stdin(Stdio::null()).stdout(Stdio::null()).spawn() {
            // Waited for, so no ended process stays behind.
            Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
            Err(err) => self.toast("Lights", format!("The light could not be told to blink: {err}"), cx),
        }
    }

    /// The settings as the helper reads them.
    pub fn lights_json(&self, cx: &gpui_kit::App) -> Value {
        let Some(lights) = &self.events.lights else { return Value::Null };
        let value = |slider: &Entity<SliderState>| slider.read(cx).value().end();
        let hue = value(&lights.hue);
        json!({
            "film": value(&lights.film).round(),
            "pause": value(&lights.pause).round(),
            "credits": value(&lights.credits).round(),
            "gap": (f64::from(value(&lights.gap)) * 100.).round() / 100.,
            "pause_white": lights.white,
            "pause_kelvin": value(&lights.kelvin).round(),
            "hue": hue.round(),
            "aura": lights.aura,
            "lamps": lights.lamps,
            "aura_glide": (f64::from(value(&lights.glide)) * 100.).round() / 100.,
            "colour": match lights.colour {
                FilmColour::Keep => json!("keep"),
                FilmColour::Artwork => json!("artwork"),
                FilmColour::Fixed => json!(hue_colour(hue)),
            },
        })
    }

    /// Writes the settings file once the slider rests: a drag sends many
    /// changes, and each write is work for the disk on the UI thread.
    fn lights_write_soon(&mut self, cx: &mut Context<Self>) {
        let write = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let _ = this.update(cx, |this, cx| this.lights_write(cx));
        });
        if let Some(lights) = &mut self.events.lights {
            lights.write = Some(write);
        }
        cx.notify();
    }

    /// Writes the settings file; the helper reads it at its next
    /// event. Whole or not at all: the example may read at any moment.
    fn lights_write(&mut self, cx: &mut Context<Self>) {
        let (path, text) = (lights_path(), self.lights_json(cx).to_string());
        let part = format!("{path}.part");
        if let Err(err) = std::fs::write(&part, text).and_then(|()| std::fs::rename(&part, &path)) {
            log::warn!("lights: {path} not written: {err}");
        }
        cx.notify();
    }

    /// Sets one value by its name in the settings file; the
    /// development channel does this, as the page does with a slider.
    pub fn lights_set(&mut self, name: &str, value: &str, cx: &mut Context<Self>) -> bool {
        let Some(lights) = &mut self.events.lights else { return false };
        let slider = match name {
            "film" => &lights.film,
            "pause" => &lights.pause,
            "credits" => &lights.credits,
            "gap" => &lights.gap,
            "pause_kelvin" => &lights.kelvin,
            "hue" => &lights.hue,
            "aura_glide" => &lights.glide,
            "aura" => {
                lights.aura = value == "true";
                // A light needs the colours of the picture for it.
                if lights.aura && self.config.events_colours != Some(true) {
                    self.set_events_colours(true, cx);
                }
                self.lights_write(cx);
                return true;
            }
            "pause_white" => {
                lights.white = value == "true";
                self.lights_write(cx);
                return true;
            }
            "colour" => {
                lights.colour = match value {
                    "keep" => FilmColour::Keep,
                    "artwork" => FilmColour::Artwork,
                    "fixed" => FilmColour::Fixed,
                    _ => return false,
                };
                self.lights_write(cx);
                return true;
            }
            _ => return false,
        };
        let Some(value) = finite(value) else { return false };
        slider.update(cx, |state, cx| {
            let (min, max, step) = (state.min_value(), state.max_value(), state.step_value());
            *state = lab_slider(min, max, step, value);
            cx.notify();
        });
        self.lights_write(cx);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings file of the lights lies next to the socket, and the
    /// bounds of a slider above 100 are taken (a fresh slider ends at 100).
    #[test]
    fn the_settings_file_is_next_to_the_socket() {
        let (lights, socket) = (lights_path(), socket_path());
        assert!(lights.ends_with(".json") && lights.contains("lights"), "{lights}");
        assert_eq!(std::path::Path::new(&lights).parent(), std::path::Path::new(&socket).parent());
        let warmth = lab_slider(2700., 6500., 100., 3200.);
        assert_eq!((warmth.min_value(), warmth.max_value(), warmth.value().end()), (2700., 6500., 3200.));
        assert_eq!((finite("0.5"), finite("NaN"), finite("inf"), finite("-inf"), finite("x")), (Some(0.5), None, None, None, None));
    }

    /// The lights a look found: a line that is no light is left out, a
    /// name of any length is cut, and one light is known again by the name
    /// it has for good, also at a new address.
    #[test]
    fn the_lights_of_a_look_are_read_with_care() {
        let long = "x".repeat(500);
        let text = format!(
            "{}\nnot json\n{{\"driver\":\"govee\"}}\n[1]\n{}\n",
            json!({ "driver": "govee", "id": "AA:BB", "model": long, "address": "10.0.0.5", "more": 1 }),
            json!({ "driver": "govee", "address": "10.0.0.6" }),
        );
        let found = found_lines(&text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["model"].as_str().unwrap().len(), 64);
        // The id is kept whole (a light is known again by it), and a line
        // with an id of no sane length is no light.
        let ids = format!("{}\n{}\n", json!({ "driver": "govee", "id": "y".repeat(200), "address": "1.1.1.1" }), json!({ "driver": "govee", "id": "y".repeat(300), "address": "1.1.1.2" }));
        let kept = found_lines(&ids);
        assert_eq!((kept.len(), kept[0]["id"].as_str().unwrap().len()), (1, 200));
        assert_eq!(found[0].as_object().unwrap().len(), 4, "a field of the line that is none of a light is kept");
        assert_eq!(found[1], json!({ "driver": "govee", "id": "", "model": "", "address": "10.0.0.6" }));
        let many: String = (0..100).map(|n| format!("{}\n", json!({ "driver": "govee", "address": format!("10.0.0.{n}") }))).collect();
        assert_eq!(found_lines(&many).len(), 32);
        let moved = json!({ "driver": "govee", "id": "AA:BB", "address": "10.0.0.99" });
        assert!(same_lamp(&found[0], &moved), "a light at a new address is another light");
        assert!(!same_lamp(&found[0], &found[1]));
        assert!(same_lamp(&found[1], &json!({ "driver": "govee", "id": "", "address": "10.0.0.6" })));
        assert!(!same_lamp(&found[1], &json!({ "driver": "hue", "id": "", "address": "10.0.0.6" })));
    }
}
