// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rules: which event of Bloom asks for which light. Pure: the events
//! and the settings go in, and what the lights must do comes out. Nothing
//! here knows a brand of light.

use serde_json::{Value, json};

/// The colour of the light while a film plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilmColour {
    /// The colour the light had.
    Keep,
    /// The colour of the artwork of the item, as Bloom tells it.
    Artwork,
    /// One colour for every film.
    Fixed([u8; 3]),
}

/// What the user chose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// The brightness, in percent, for each moment.
    pub film: f64,
    pub pause: f64,
    pub credits: f64,
    /// Seconds between the two commands of a fade down.
    pub gap: f64,
    /// The white of a pause, in kelvin. None: a pause keeps the colour.
    pub pause_kelvin: Option<u32>,
    pub colour: FilmColour,
    /// The light follows the colours of the picture.
    pub aura: bool,
    /// Seconds the light takes to come most of the way to a new colour.
    pub aura_glide: f64,
}

impl Settings {
    /// These settings with the fields of a settings file over them.
    pub fn over(mut self, file: &Value) -> Self {
        let level = |name: &str, old: f64| file[name].as_f64().map_or(old, |v| v.clamp(1., 100.));
        self.film = level("film", self.film);
        self.pause = level("pause", self.pause);
        self.credits = level("credits", self.credits);
        self.gap = file["gap"].as_f64().map_or(self.gap, |v| v.clamp(0., 30.));
        // Each of the two fields counts by itself. 0 kelvin is "no white",
        // as with the option of the command line.
        let white = file["pause_white"].as_bool().unwrap_or(self.pause_kelvin.is_some());
        let kelvin = file["pause_kelvin"].as_f64().map_or(self.pause_kelvin.unwrap_or(2700), |k| k as u32);
        self.pause_kelvin = white.then_some(kelvin).filter(|kelvin| *kelvin > 0).map(kelvin_of_a_light);
        if let Some(colour) = colour_choice(&file["colour"]) {
            self.colour = colour;
        }
        self.aura = file["aura"].as_bool().unwrap_or(self.aura);
        self.aura_glide = file["aura_glide"].as_f64().map_or(self.aura_glide, |v| v.clamp(0.01, 5.));
        self
    }
}

/// A colour temperature a light can show. Outside of this a Govee light
/// shows black or nothing.
pub fn kelvin_of_a_light(kelvin: u32) -> u32 {
    kelvin.clamp(2000, 9000)
}

/// `"keep"`, `"artwork"`, `[r, g, b]` or `"r,g,b"`.
pub fn colour_choice(value: &Value) -> Option<FilmColour> {
    match value.as_str() {
        Some("keep") => Some(FilmColour::Keep),
        Some("artwork") => Some(FilmColour::Artwork),
        Some(text) => {
            // Every part must be a number: "300,1,2,3" is not the colour 1,2,3.
            let parts: Option<Vec<u8>> = text.split(',').map(|part| part.trim().parse().ok()).collect();
            <[u8; 3]>::try_from(parts?).ok().map(FilmColour::Fixed)
        }
        None => rgb(value).map(FilmColour::Fixed),
    }
}

pub fn rgb(value: &Value) -> Option<[u8; 3]> {
    let part = |at: usize| value[at].as_u64().and_then(|v| u8::try_from(v).ok());
    Some([part(0)?, part(1)?, part(2)?])
}

/// What the light must do after an event.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Keep what the light has now, for the end of the playback.
    Remember,
    /// Show a colour.
    Colour([u8; 3]),
    /// Show the colour the light had before the playback.
    OwnColour,
    /// Show the colours of the picture, the top first, along the light.
    Aura(Vec<[u8; 3]>),
    /// Show one light along the whole light, in the frame stream: the
    /// white of a pause, at its brightness in percent.
    AuraWhite { kelvin: u32, level: f64 },
    /// Fade to a brightness; with a colour temperature in kelvin first.
    Fade { to: f64, kelvin: Option<u32> },
    /// Put back what the light had before the playback.
    Restore,
}

/// The playback the light follows.
#[derive(Clone, Debug, Default)]
pub struct Film {
    pub on: bool,
    pub paused: bool,
    /// The colour of this film; None is the light's own.
    pub colour: Option<[u8; 3]>,
    /// The pause changed the colour of the light.
    pub whitened: bool,
    /// The light already follows the artwork of a detail page: what the
    /// light had is kept, and is not asked for again at the start.
    pub early: bool,
    /// A playback runs in picture in picture: the light is not with it,
    /// and takes it when the window is large again.
    pub parked: bool,
}

/// The rules: which event asks for which light.
pub fn react(event: &str, data: &Value, settings: Settings, film: &mut Film) -> Vec<Action> {
    // In picture in picture the room is not a cinema.
    let pip = data["window"]["pip"] == true || data["pip"] == true;
    let down = Action::Fade { to: settings.film, kelvin: None };
    match event {
        // A program that connects in the middle of a film is told what
        // plays: that is a start for it (a start says whether it is paused).
        "state" => {
            let playback = &data["playback"];
            return if playback.is_null() || film.on { Vec::new() } else { react("playback.started", playback, settings, film) };
        }
        // A playback that starts small: the light is not with it. A detail
        // page that had the light gives it back.
        "playback.started" if pip => {
            *film = Film { parked: true, paused: data["paused"] == true, early: film.early, ..Film::default() };
            return if std::mem::take(&mut film.early) { vec![Action::Restore] } else { Vec::new() };
        }
        // A playback that is paused as it starts: the start, then its pause
        // (the first line a program gets can say so, and no pause event
        // follows it).
        "playback.started" if data["paused"] == true => {
            let mut plain = data.clone();
            plain["paused"] = json!(false);
            let mut actions = react("playback.started", &plain, settings, film);
            actions.extend(react("playback.paused", &plain, settings, film));
            return actions;
        }
        // While the film is small the light is not with it, but what the
        // film does is kept for the moment the window is large again.
        "playback.paused" | "playback.resumed" if film.parked => {
            film.paused = event == "playback.paused";
            return Vec::new();
        }
        "artwork.colour" if film.parked => {
            film.colour = rgb(&data["colour"]);
            return Vec::new();
        }
        // The artwork of a page comes late, and the film already plays small.
        "detail.opened" if film.parked => return Vec::new(),
        // The window is large again: the light takes the film, with its
        // pause and its colour.
        "window.changed" if film.parked && !pip && data["player_open"] != false => {
            let again = json!({ "again": true, "paused": film.paused });
            film.parked = false;
            return react("playback.started", &again, settings, film);
        }
        "playback.stopped" if film.parked && data["reason"] != "replaced" => {
            *film = Film::default();
            return Vec::new();
        }
        _ => {}
    }
    if settings.aura {
        // The light follows the picture: no fades, the picture is the light.
        return match event {
            "playback.started" if !pip => {
                // The light it had is kept once: not again after a detail
                // page, and not for the next item of a queue.
                let kept = film.early || film.on;
                // The colour of the artwork is not shown here, but kept for
                // the other rules, when the user turns this off again.
                let colour = film.colour.filter(|_| data["again"] == true);
                *film = Film { on: true, early: film.early, colour, ..Film::default() };
                if kept { Vec::new() } else { vec![Action::Remember] }
            }
            // A detail page: the light takes the colours of its artwork,
            // so it is with the film from its first frame.
            "detail.opened" if !film.on => {
                let colours: Vec<[u8; 3]> = data["colours"].as_array().into_iter().flatten().filter_map(rgb).collect();
                if colours.is_empty() {
                    return Vec::new();
                }
                let remember = (!std::mem::replace(&mut film.early, true)).then_some(Action::Remember);
                remember.into_iter().chain([Action::Aura(colours)]).collect()
            }
            "detail.closed" if !film.on && film.early => {
                film.early = false;
                vec![Action::Restore]
            }
            "artwork.colour" if film.on => {
                film.colour = rgb(&data["colour"]);
                Vec::new()
            }
            "frame.colours" if film.on && !film.paused => {
                let colours: Vec<[u8; 3]> = data["colours"].as_array().into_iter().flatten().filter_map(rgb).collect();
                if colours.is_empty() { Vec::new() } else { vec![Action::Aura(colours)] }
            }
            "playback.paused" if film.on => {
                film.paused = true;
                // With no white for a pause the light keeps the last picture.
                settings.pause_kelvin.map(|kelvin| Action::AuraWhite { kelvin, level: settings.pause }).into_iter().collect()
            }
            "playback.resumed" if film.on => {
                film.paused = false;
                Vec::new()
            }
            // The next item took its place: the light stays with the film.
            "playback.stopped" if data["reason"] == "replaced" => Vec::new(),
            "window.changed" | "playback.stopped" if film.on && (pip || event == "playback.stopped") => {
                let parked = event == "window.changed";
                *film = Film { parked, paused: parked && film.paused, colour: film.colour.filter(|_| parked), ..Film::default() };
                vec![Action::Restore]
            }
            _ => Vec::new(),
        };
    }
    match event {
        "playback.started" if !pip => {
            // For the next item of a queue the light it had is kept as it is.
            let kept = film.on;
            *film = Film {
                on: true,
                colour: match settings.colour {
                    FilmColour::Fixed(colour) => Some(colour),
                    // Told a moment later, when Bloom has looked at the
                    // artwork; a film that is taken again (after picture in
                    // picture, after a change of the rules) has it already.
                    FilmColour::Artwork => film.colour.filter(|_| data["again"] == true),
                    FilmColour::Keep => None,
                },
                ..Film::default()
            };
            let colour = film.colour.map(Action::Colour);
            (!kept).then_some(Action::Remember).into_iter().chain(colour).chain([down]).collect()
        }
        "artwork.colour" if film.on && settings.colour == FilmColour::Artwork => {
            film.colour = rgb(&data["colour"]);
            // During a pause the light keeps the light of the pause.
            film.colour.filter(|_| !film.paused).map(Action::Colour).into_iter().collect()
        }
        "playback.resumed" if film.on && !pip => {
            film.paused = false;
            let colour = std::mem::take(&mut film.whitened).then(|| film.colour.map_or(Action::OwnColour, Action::Colour));
            colour.into_iter().chain([down]).collect()
        }
        "playback.recovered" if film.on && !film.paused && !pip => vec![down],
        "playback.paused" if film.on => {
            film.paused = true;
            film.whitened |= settings.pause_kelvin.is_some();
            vec![Action::Fade { to: settings.pause, kelvin: settings.pause_kelvin }]
        }
        "segment.entered" if film.on && data["kind"] == "Outro" => {
            vec![Action::Fade { to: settings.credits, kelvin: None }]
        }
        // The window went into picture in picture in the middle of a film.
        "window.changed" if film.on && pip => {
            *film = Film { parked: true, paused: film.paused, colour: film.colour, ..Film::default() };
            vec![Action::Restore]
        }
        // The next item took its place: the light stays down.
        "playback.stopped" if data["reason"] == "replaced" => Vec::new(),
        "playback.stopped" if film.on => {
            film.on = false;
            vec![Action::Restore]
        }
        _ => Vec::new(),
    }
}

pub fn option(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).cloned()
}

/// The settings file that belongs to a socket: `events.sock` has
/// `lights.json`, `events-<name>.sock` has `lights-<name>.json`.
pub fn settings_path(socket: &str) -> String {
    let path = std::path::Path::new(socket);
    let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("events").replacen("events", "lights", 1);
    path.with_file_name(format!("{name}.json")).display().to_string()
}

/// The settings of the command line. A number that is none ("nan", "inf",
/// a word) is the default; a level is brought into 1 to 100. None for a
/// colour that cannot be read.
pub fn chosen(args: &[String]) -> Option<Settings> {
    let number = |name: &str, default: f64| {
        option(args, name).and_then(|v| v.parse().ok()).filter(|v: &f64| v.is_finite()).unwrap_or(default)
    };
    Some(Settings {
        film: number("--film", 5.).clamp(1., 100.),
        pause: number("--pause", 25.).clamp(1., 100.),
        credits: number("--credits", 40.).clamp(1., 100.),
        gap: number("--gap", 0.4).clamp(0., 30.),
        pause_kelvin: Some(number("--pause-kelvin", 2700.) as u32).filter(|kelvin| *kelvin > 0).map(kelvin_of_a_light),
        colour: colour_choice(&json!(option(args, "--colour").unwrap_or_else(|| "keep".into())))?,
        aura: args.iter().any(|arg| arg == "--aura"),
        aura_glide: number("--aura-glide", 0.07).clamp(0.01, 5.),
    })
}

/// "Follow the picture" went on or off in the settings: what the lights
/// do before the next event. The two sets of rules do not know each other,
/// so the lights get back what they had (which ends the stream and a fade
/// that runs), and a film that plays starts again under the new rules.
pub fn switched(aura: &mut bool, now: Settings, film: &mut Film, streams: bool) -> Vec<Action> {
    if std::mem::replace(aura, now.aura) == now.aura || !(film.on || film.early || streams) {
        return Vec::new();
    }
    [Action::Restore].into_iter().chain(restart(now, film)).collect()
}

/// The film that plays, taken again from its start with what is known of
/// it (its pause, the colour of its artwork): after a change of the rules,
/// and for lights that were chosen in the middle of it. A detail page that
/// had the lights does not have them after this.
pub fn restart(now: Settings, film: &mut Film) -> Vec<Action> {
    let (on, paused) = (film.on, film.paused);
    *film = Film { parked: film.parked, paused: film.parked && paused, colour: film.colour, ..Film::default() };
    if on { react("playback.started", &json!({ "again": true, "paused": paused }), now, film) } else { Vec::new() }
}

/// The stream of events ended: Bloom quit, or the setting went off. A
/// film that ran has ended with it, and so has a detail page that had the
/// light.
pub fn ended(film: &mut Film) -> Vec<Action> {
    let had = film.on || film.early;
    *film = Film::default();
    if had { vec![Action::Restore] } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: Settings =
        Settings { film: 5., pause: 25., credits: 40., gap: 0.4, pause_kelvin: Some(2700), colour: FilmColour::Keep, aura: false, aura_glide: 0.07 };

    fn fade(to: f64) -> Action {
        Action::Fade { to, kelvin: None }
    }

    /// A film from its start to its end: remember and dim, warm light at
    /// the pause, the light's own colour and down again, up a little at
    /// the credits, back at the end.
    #[test]
    fn a_film_dims_the_light_and_its_end_puts_it_back() {
        let mut film = Film::default();
        let window = json!({ "window": { "fullscreen": true, "pip": false } });
        assert_eq!(react("playback.started", &window, SETTINGS, &mut film), [Action::Remember, fade(5.)]);
        assert_eq!(react("playback.paused", &window, SETTINGS, &mut film), [Action::Fade { to: 25., kelvin: Some(2700) }]);
        assert_eq!(react("playback.resumed", &window, SETTINGS, &mut film), [Action::OwnColour, fade(5.)]);
        assert!(react("segment.entered", &json!({ "kind": "Intro" }), SETTINGS, &mut film).is_empty());
        assert_eq!(react("segment.entered", &json!({ "kind": "Outro" }), SETTINGS, &mut film), [fade(40.)]);
        assert_eq!(react("playback.stopped", &window, SETTINGS, &mut film), [Action::Restore]);
        // After the end nothing follows the player.
        assert!(react("playback.paused", &window, SETTINGS, &mut film).is_empty());
        // The next item of a queue: the light stays down, and what the
        // light had before the first item is kept, not asked for again.
        let replaced = json!({ "reason": "replaced" });
        react("playback.started", &window, SETTINGS, &mut film);
        assert!(react("playback.stopped", &replaced, SETTINGS, &mut film).is_empty());
        assert_eq!(react("playback.started", &window, SETTINGS, &mut film), [fade(5.)]);
        assert_eq!(react("playback.stopped", &window, SETTINGS, &mut film), [Action::Restore]);
        let aura = Settings { aura: true, ..SETTINGS };
        react("playback.started", &window, aura, &mut film);
        assert!(react("playback.stopped", &replaced, aura, &mut film).is_empty());
        assert!(react("playback.started", &window, aura, &mut film).is_empty());
        assert_eq!(react("playback.stopped", &window, aura, &mut film), [Action::Restore]);
    }

    /// A program that connects in the middle of a film takes the film:
    /// the state it is told is a start, and a pause when the film is paused.
    #[test]
    fn a_film_that_already_plays_is_taken() {
        for settings in [SETTINGS, Settings { aura: true, ..SETTINGS }] {
            let mut film = Film::default();
            assert!(react("state", &json!({ "playback": null }), settings, &mut film).is_empty());
            let playing = json!({ "playback": { "paused": false, "window": { "pip": false } } });
            assert_eq!(react("state", &playing, settings, &mut film)[0], Action::Remember);
            assert!(film.on && !film.paused);
            // A second state line (a reconnect that kept the film) changes nothing.
            assert!(react("state", &playing, settings, &mut film).is_empty());
            let mut film = Film::default();
            let paused = json!({ "playback": { "paused": true, "window": { "pip": false } } });
            let actions = react("state", &paused, settings, &mut film);
            assert!(film.on && film.paused, "{actions:?}");
            let pause = |actions: &[Action]| {
                actions.iter().any(|action| matches!(action, Action::Fade { kelvin: Some(2700), .. } | Action::AuraWhite { .. }))
            };
            assert!(pause(&actions));
            // The start of that same film comes after the state (it was on
            // its way): the pause holds, and the light is not asked again.
            let again = react("playback.started", &paused["playback"], settings, &mut film);
            assert!(film.paused && pause(&again) && !again.contains(&Action::Remember), "{again:?}");
            // A paused film in a small window, then the window large: the
            // light takes the film with its pause.
            let mut film = Film::default();
            let small = json!({ "playback": { "paused": true, "window": { "pip": true } } });
            assert!(react("state", &small, settings, &mut film).is_empty());
            let large = react("window.changed", &json!({ "pip": false, "player_open": true }), settings, &mut film);
            assert!(film.on && film.paused && pause(&large), "{large:?}");
        }
    }

    /// In picture in picture the light stays as it is, and a film that
    /// goes there gives the light back.
    #[test]
    fn picture_in_picture_leaves_the_light_alone() {
        let mut film = Film::default();
        let small = json!({ "window": { "fullscreen": false, "pip": true } });
        assert!(react("playback.started", &small, SETTINGS, &mut film).is_empty());
        assert!(react("playback.paused", &small, SETTINGS, &mut film).is_empty());
        let full = json!({ "window": { "fullscreen": true, "pip": false } });
        react("playback.started", &full, SETTINGS, &mut film);
        assert_eq!(react("window.changed", &json!({ "pip": true }), SETTINGS, &mut film), [Action::Restore]);
        assert!(!film.on);
        // The window is large again: the light takes the film again.
        let large = json!({ "pip": false, "player_open": true });
        assert_eq!(react("window.changed", &large, SETTINGS, &mut film), [Action::Remember, fade(5.)]);
        assert!(film.on);
        // A film that starts small and is made large, and one that ends small.
        let mut film = Film::default();
        react("playback.started", &small, SETTINGS, &mut film);
        assert_eq!(react("window.changed", &large, SETTINGS, &mut film), [Action::Remember, fade(5.)]);
        let mut film = Film::default();
        react("playback.started", &small, SETTINGS, &mut film);
        assert!(react("playback.stopped", &small, SETTINGS, &mut film).is_empty());
        assert!(react("window.changed", &large, SETTINGS, &mut film).is_empty(), "no film plays");
        // A detail page had the light, and the film starts small: the
        // light goes back, and comes to the film when the window is large.
        let aura = Settings { aura: true, ..SETTINGS };
        let mut film = Film::default();
        react("detail.opened", &json!({ "colours": [[1, 2, 3]] }), aura, &mut film);
        assert_eq!(react("playback.started", &small, aura, &mut film), [Action::Restore]);
        assert_eq!(react("window.changed", &large, aura, &mut film), [Action::Remember]);
        assert_eq!(react("window.changed", &json!({ "pip": true }), aura, &mut film), [Action::Restore]);
        assert_eq!(react("window.changed", &large, aura, &mut film), [Action::Remember]);
        // Paused, made small, made large with no resume: the pause holds.
        react("playback.paused", &json!({}), aura, &mut film);
        react("window.changed", &json!({ "pip": true }), aura, &mut film);
        assert_eq!(
            react("window.changed", &large, aura, &mut film),
            [Action::Remember, Action::AuraWhite { kelvin: 2700, level: 25. }]
        );
        // A pause and a resume while small are kept for the large window.
        react("window.changed", &json!({ "pip": true }), aura, &mut film);
        assert!(react("playback.resumed", &json!({}), aura, &mut film).is_empty());
        assert_eq!(react("window.changed", &large, aura, &mut film), [Action::Remember]);
        // The artwork of a page that comes while the film plays small does
        // not take the light, and the end of the small film leaves no page
        // in charge.
        let mut film = Film::default();
        react("playback.started", &small, aura, &mut film);
        assert!(react("detail.opened", &json!({ "colours": [[1, 2, 3]] }), aura, &mut film).is_empty());
        react("playback.stopped", &small, aura, &mut film);
        assert!(!film.early && !film.parked);
        // The colour of the artwork is kept over a small window.
        let artwork = Settings { colour: FilmColour::Artwork, ..SETTINGS };
        let mut film = Film::default();
        react("playback.started", &full, artwork, &mut film);
        react("artwork.colour", &json!({ "colour": [255, 120, 0] }), artwork, &mut film);
        react("window.changed", &json!({ "pip": true }), artwork, &mut film);
        assert_eq!(
            react("window.changed", &large, artwork, &mut film),
            [Action::Remember, Action::Colour([255, 120, 0]), fade(5.)]
        );
        // And one that comes while the window is small is there after it.
        let mut film = Film::default();
        react("playback.started", &small, artwork, &mut film);
        react("artwork.colour", &json!({ "colour": [0, 255, 0] }), artwork, &mut film);
        assert_eq!(react("window.changed", &large, artwork, &mut film)[1], Action::Colour([0, 255, 0]));
    }

    /// The colour of the film: one the user chose, or the colour of the
    /// artwork when Bloom tells it. A pause that keeps the colour sends no
    /// colour at the resume; the artwork colour that comes during a pause
    /// waits for the resume.
    #[test]
    fn the_film_has_the_colour_the_user_chose() {
        let window = json!({});
        let fixed = Settings { colour: FilmColour::Fixed([0, 80, 255]), pause_kelvin: None, ..SETTINGS };
        let mut film = Film::default();
        assert_eq!(
            react("playback.started", &window, fixed, &mut film),
            [Action::Remember, Action::Colour([0, 80, 255]), fade(5.)]
        );
        assert_eq!(react("playback.paused", &window, fixed, &mut film), [fade(25.)]);
        assert_eq!(react("playback.resumed", &window, fixed, &mut film), [fade(5.)]);

        let artwork = Settings { colour: FilmColour::Artwork, ..SETTINGS };
        let told = json!({ "colour": [255, 120, 0] });
        let mut film = Film::default();
        assert!(react("artwork.colour", &told, artwork, &mut film).is_empty(), "no film runs");
        assert_eq!(react("playback.started", &window, artwork, &mut film), [Action::Remember, fade(5.)]);
        assert_eq!(react("artwork.colour", &told, artwork, &mut film), [Action::Colour([255, 120, 0])]);
        react("playback.paused", &window, artwork, &mut film);
        assert!(react("artwork.colour", &json!({ "colour": [0, 255, 0] }), artwork, &mut film).is_empty());
        assert_eq!(react("playback.resumed", &window, artwork, &mut film), [Action::Colour([0, 255, 0]), fade(5.)]);
        // With another choice the colour of the artwork is not used.
        let mut film = Film::default();
        react("playback.started", &window, SETTINGS, &mut film);
        assert!(react("artwork.colour", &told, SETTINGS, &mut film).is_empty());
    }

    /// With the aura the light follows the picture: no fades; the colours
    /// of the picture while it plays, the white of a pause, and the light
    /// it had at the end.
    #[test]
    fn the_aura_follows_the_picture() {
        let aura = Settings { aura: true, ..SETTINGS };
        let frame = json!({ "colours": [[10, 20, 30], [200, 100, 0]] });
        let mut film = Film::default();
        assert!(react("frame.colours", &frame, aura, &mut film).is_empty(), "no film runs");
        assert_eq!(react("playback.started", &json!({}), aura, &mut film), [Action::Remember]);
        assert_eq!(react("frame.colours", &frame, aura, &mut film), [Action::Aura(vec![[10, 20, 30], [200, 100, 0]])]);
        assert_eq!(react("playback.paused", &json!({}), aura, &mut film), [Action::AuraWhite { kelvin: 2700, level: 25. }]);
        assert!(react("frame.colours", &frame, aura, &mut film).is_empty(), "a pause keeps its light");
        assert!(react("playback.resumed", &json!({}), aura, &mut film).is_empty());
        assert!(react("segment.entered", &json!({ "kind": "Outro" }), aura, &mut film).is_empty());
        assert_eq!(react("frame.colours", &frame, aura, &mut film).len(), 1);
        assert_eq!(react("playback.stopped", &json!({}), aura, &mut film), [Action::Restore]);
        // A detail page takes the light before the film: the light it had
        // is kept once, and the start of the film does not ask again.
        let page = json!({ "colours": [[1, 2, 3]] });
        assert_eq!(react("detail.opened", &page, aura, &mut film), [Action::Remember, Action::Aura(vec![[1, 2, 3]])]);
        assert_eq!(react("detail.opened", &page, aura, &mut film), [Action::Aura(vec![[1, 2, 3]])]);
        assert!(react("playback.started", &json!({}), aura, &mut film).is_empty());
        assert!(react("detail.closed", &json!({}), aura, &mut film).is_empty(), "the film has the light now");
        assert_eq!(react("playback.stopped", &json!({}), aura, &mut film), [Action::Restore]);
        react("detail.opened", &page, aura, &mut film);
        assert_eq!(react("detail.closed", &json!({}), aura, &mut film), [Action::Restore]);
        assert!(react("detail.closed", &json!({}), aura, &mut film).is_empty());
        // Without the aura the colours of the picture are not used.
        let mut film = Film::default();
        react("playback.started", &json!({}), SETTINGS, &mut film);
        assert!(react("frame.colours", &frame, SETTINGS, &mut film).is_empty());
    }

    /// The settings file goes over the options, field by field.
    #[test]
    fn a_settings_file_goes_over_the_options() {
        assert_eq!(SETTINGS.over(&json!({})), SETTINGS);
        // Bloom writes whole numbers with a point.
        let file = json!({ "film": 8.0, "gap": 0.55, "pause_white": true, "pause_kelvin": 3200.0, "colour": "artwork" });
        assert_eq!(
            SETTINGS.over(&file),
            Settings { film: 8., gap: 0.55, pause_kelvin: Some(3200), colour: FilmColour::Artwork, ..SETTINGS }
        );
        let file = json!({ "pause_white": false, "colour": [1, 2, 3], "credits": 400 });
        assert_eq!(
            SETTINGS.over(&file),
            Settings { pause_kelvin: None, colour: FilmColour::Fixed([1, 2, 3]), credits: 100., ..SETTINGS }
        );
        assert_eq!(colour_choice(&json!("10, 20,30")), Some(FilmColour::Fixed([10, 20, 30])));
        assert_eq!(colour_choice(&json!("blue")), None);
        assert_eq!(colour_choice(&json!("300,1,2,3")), None);
        assert_eq!(colour_choice(&json!("10,20,30,abc")), None);
        // A white no light can show is brought into its range; 0 is no white.
        assert_eq!(SETTINGS.over(&json!({ "pause_white": true, "pause_kelvin": 1e12 })).pause_kelvin, Some(9000));
        assert_eq!(SETTINGS.over(&json!({ "pause_white": true, "pause_kelvin": 50 })).pause_kelvin, Some(2000));
        assert_eq!(SETTINGS.over(&json!({ "pause_white": true, "pause_kelvin": 0 })).pause_kelvin, None);
        // The kelvin alone, with no word on the white.
        assert_eq!(SETTINGS.over(&json!({ "pause_kelvin": 0 })).pause_kelvin, None);
        assert_eq!(SETTINGS.over(&json!({ "pause_kelvin": 4000 })).pause_kelvin, Some(4000));
        assert_eq!(Settings { pause_kelvin: None, ..SETTINGS }.over(&json!({ "pause_kelvin": 4000 })).pause_kelvin, None);
        assert_eq!(settings_path("/a/events.sock"), "/a/lights.json");
        assert_eq!(settings_path("/a/events-lab.sock"), "/a/lights-lab.json");
    }

    /// The numbers of the command line: one that is none is the default,
    /// a level stays in 1 to 100, a port must be a port.
    #[test]
    fn the_command_line_takes_no_number_that_is_none() {
        let args = |text: &str| text.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(chosen(&args("--ip 1.2.3.4")), Some(SETTINGS));
        let odd = chosen(&args("--film nan --pause inf --credits 400 --gap nan --aura-glide nan --pause-kelvin 50")).unwrap();
        assert_eq!(odd, Settings { credits: 100., pause_kelvin: Some(2000), ..SETTINGS });
        assert_eq!(chosen(&args("--film -3 --pause 0 --gap -1 --aura-glide 0")).unwrap(), Settings { film: 1., pause: 1., gap: 0., aura_glide: 0.01, ..SETTINGS });
        assert_eq!(chosen(&args("--pause-kelvin 0 --aura --colour 1,2,3")).unwrap(), Settings { pause_kelvin: None, aura: true, colour: FilmColour::Fixed([1, 2, 3]), ..SETTINGS });
        assert_eq!(chosen(&args("--colour blue")), None);
    }

    /// The end of the stream of events gives the light back, also when a
    /// detail page had it and no film; the next connection starts clean.
    #[test]
    fn the_end_of_the_stream_gives_the_light_back() {
        let aura = Settings { aura: true, ..SETTINGS };
        let mut film = Film::default();
        assert!(ended(&mut film).is_empty(), "nothing had the light");
        react("detail.opened", &json!({ "colours": [[1, 2, 3]] }), aura, &mut film);
        assert_eq!(ended(&mut film), [Action::Restore]);
        // After it a detail page keeps what the light has once more.
        assert_eq!(react("detail.opened", &json!({ "colours": [[1, 2, 3]] }), aura, &mut film)[0], Action::Remember);
        react("playback.started", &json!({}), aura, &mut film);
        assert_eq!(ended(&mut film), [Action::Restore]);
        assert!(!film.on && !film.early);
    }

    /// "Follow the picture" switched during a film: the light gets back
    /// what it had, and the film starts again under the other rules, with
    /// its pause.
    #[test]
    fn a_switch_of_the_aura_starts_the_film_again() {
        let aura = Settings { aura: true, ..SETTINGS };
        let (mut film, mut on) = (Film::default(), false);
        assert!(switched(&mut on, SETTINGS, &mut film, false).is_empty(), "nothing changed");
        assert!(switched(&mut on, aura, &mut film, false).is_empty(), "no film and no page");
        react("playback.started", &json!({}), aura, &mut film);
        // Off during a film: back, then the dim of the other rules.
        assert_eq!(switched(&mut on, SETTINGS, &mut film, true), [Action::Restore, Action::Remember, fade(5.)]);
        assert!(film.on && !on);
        react("playback.paused", &json!({}), SETTINGS, &mut film);
        // On during a pause: back, the film again, and its pause in the stream.
        assert_eq!(
            switched(&mut on, aura, &mut film, false),
            [Action::Restore, Action::Remember, Action::AuraWhite { kelvin: 2700, level: 25. }]
        );
        assert!(film.on && film.paused && on);
        // A detail page had the light: back, and nothing more.
        let mut film = Film::default();
        react("detail.opened", &json!({ "colours": [[1, 2, 3]] }), aura, &mut film);
        assert_eq!(switched(&mut on, SETTINGS, &mut film, true), [Action::Restore]);
        assert!(!film.early);
        // The colour of the artwork is kept over the switch: one that came
        // while the picture was followed, and one from before.
        let (artwork, artwork_aura) = (Settings { colour: FilmColour::Artwork, ..SETTINGS }, Settings { colour: FilmColour::Artwork, ..aura });
        let orange = [Action::Restore, Action::Remember, Action::Colour([255, 120, 0]), fade(5.)];
        let (mut film, mut on) = (Film::default(), true);
        react("playback.started", &json!({}), artwork_aura, &mut film);
        assert!(react("artwork.colour", &json!({ "colour": [255, 120, 0] }), artwork_aura, &mut film).is_empty());
        assert_eq!(switched(&mut on, artwork, &mut film, true), orange);
        assert_eq!(switched(&mut on, artwork_aura, &mut film, false), [Action::Restore, Action::Remember]);
        assert_eq!(switched(&mut on, artwork, &mut film, true), orange);
        // A film that starts small with the picture followed, gets its
        // colour, is made large, and then the rules change.
        let (mut film, mut on) = (Film::default(), true);
        react("playback.started", &json!({ "window": { "pip": true } }), artwork_aura, &mut film);
        react("artwork.colour", &json!({ "colour": [255, 120, 0] }), artwork_aura, &mut film);
        react("window.changed", &json!({ "pip": false, "player_open": true }), artwork_aura, &mut film);
        assert_eq!(switched(&mut on, artwork, &mut film, true), orange);
    }
}
