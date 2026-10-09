// SPDX-License-Identifier: AGPL-3.0-or-later
//! Drives lights from the events of Bloom (`specs/outbound-events.md`).
//! Experimental.
//!
//! Bloom tells what its player does, as lines of JSON on a socket of this
//! Mac. This program reads the lines and tells the lights: it dims them
//! while a film plays, shows a warm, low light at a pause, raises them a
//! little at the credits, and at the end puts back what each light had. Or
//! the lights follow the picture ("aura"): Bloom tells the colours of the
//! picture some times a second, and each light shows them along its length.
//!
//! Bloom starts this program when the user turns the lights on in its
//! settings (Settings > Automation), and can be run by hand as well:
//!
//!   bloom-lights --scan                    the lights that answer, one line each
//!   bloom-lights --identify govee <address>  makes one light blink
//!   bloom-lights --ip 192.168.1.50         follows Bloom with one Govee light
//!   bloom-lights --dry-run                 prints what a light would be told
//!
//! The parts: the rules (`rules`: which event asks for which light, no
//! brand in it), one light of any brand (`light`: fades, the glide of the
//! picture's colours, what the lamp had), the interface to a brand
//! (`driver`), and the brands (`govee`).
//!
//! The lights and the values are options of the command line. A settings
//! file (`lights.json` next to the socket; the settings of Bloom write it)
//! goes over them, and is read again at each event, so a change needs no
//! restart.

mod driver;
mod govee;
mod light;
mod rules;

use std::{
    io::{BufRead as _, BufReader, Read as _},
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use serde_json::Value;

use crate::{
    driver::{Found, Printed, lamps_of},
    light::{Light, Lights},
    rules::{Action, Film, chosen, ended, option, react, restart, settings_path, switched},
};

const USAGE: &str = "usage: bloom-lights [--ip <address of a Govee light>] [--dry-run] [--socket <path>] [--settings <path>] [--film 5] [--pause 25] [--credits 40] [--gap 0.4] [--pause-kelvin 2700 | 0 to keep the colour] [--colour keep | artwork | r,g,b] [--aura] [--aura-glide 0.07]
       bloom-lights --scan [seconds]
       bloom-lights --identify <driver> <address>";

/// The Govee light of `--ip`, with `--port` when it is not the usual one.
/// None for a port that is none.
fn lamp_of_the_options(args: &[String]) -> Option<Option<Found>> {
    let port = match option(args, "--port") {
        Some(port) => Some(port.parse::<u16>().ok()?),
        None => None,
    };
    Some(option(args, "--ip").map(|ip| Found {
        driver: "govee".into(),
        id: String::new(),
        model: String::new(),
        address: port.map_or(ip.clone(), |port| format!("{ip}:{port}")),
    }))
}

/// Whether two entries of the settings are one lamp: by the name it has
/// for good, or by its address when it has no such name.
fn same_lamp(a: &Found, b: &Found) -> bool {
    a.driver == b.driver && if a.id.is_empty() || b.id.is_empty() { a.address == b.address } else { a.id == b.id }
}

/// The lights of the lamps the user chose. A light that was there before
/// is kept as it is, with what it knows of its lamp (what the lamp had, a
/// fade that runs); a lamp that is new is opened. A new lamp with an id is
/// looked for on the network first: its address can have changed since it
/// was chosen. The second list has the lights that are new.
fn lights_of(lamps: &[Found], before: &[(Found, Light)], dry_run: bool) -> (Vec<(Found, Light)>, Lights) {
    if dry_run {
        let printed = before.first().cloned().unwrap_or_else(|| (Found::default(), Light::new(Arc::new(Printed))));
        let new = if before.is_empty() { Lights(vec![printed.1.clone()]) } else { Lights::default() };
        return (vec![printed], new);
    }
    let kept = |lamp: &Found| before.iter().find(|(known, _)| same_lamp(known, lamp)).cloned();
    let look = lamps.iter().any(|lamp| !lamp.id.is_empty() && kept(lamp).is_none());
    let found = if look { driver::find(Duration::from_millis(1500)) } else { Vec::new() };
    let mut new = Vec::new();
    let lights = lamps.iter().filter_map(|lamp| {
        if let Some(kept) = kept(lamp) {
            return Some(kept);
        }
        let now = found.iter().find(|found| !lamp.id.is_empty() && same_lamp(found, lamp)).unwrap_or(lamp);
        let opened = driver::open(now);
        match &opened {
            Some(_) => eprintln!("bloom-lights: light {} {} at {}", now.driver, now.model, now.address),
            None => eprintln!("bloom-lights: no light for {} at {:?}", now.driver, now.address),
        }
        let light = Light::new(opened?);
        new.push(light.clone());
        Some((lamp.clone(), light))
    });
    (lights.collect(), Lights(new))
}

/// What the program is run for: the first word decides, so a text further
/// on (the address of a light) is never taken for it.
#[derive(Debug, PartialEq)]
enum Run {
    Scan(f64),
    Identify(String, String),
    Follow,
}

fn run_of(args: &[String]) -> Option<Run> {
    match args.first().map(String::as_str) {
        Some("--scan") => {
            let wait = args.get(1).and_then(|wait| wait.parse::<f64>().ok()).filter(|wait| wait.is_finite());
            Some(Run::Scan(wait.unwrap_or(2.).clamp(0.2, 30.)))
        }
        Some("--identify") => Some(Run::Identify(args.get(1)?.clone(), args.get(2)?.clone())),
        _ => Some(Run::Follow),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |name: &str| args.iter().any(|arg| arg == name);
    match run_of(&args) {
        Some(Run::Scan(wait)) => {
            for lamp in driver::find(Duration::from_secs_f64(wait)) {
                println!("{}", lamp.json());
            }
            return;
        }
        Some(Run::Identify(driver, address)) => {
            let lamp = Found { driver: driver.clone(), address: address.clone(), ..Found::default() };
            return match driver::open(&lamp) {
                Some(lamp) => Light::new(lamp).identify(),
                None => eprintln!("bloom-lights: no light for {driver} at {address:?}"),
            };
        }
        Some(Run::Follow) => {}
        None => return eprintln!("{USAGE}"),
    }
    let dry_run = has("--dry-run");
    let socket = option(&args, "--socket").unwrap_or_else(|| {
        format!("{}/Library/Application Support/bloom/events.sock", std::env::var("HOME").unwrap_or_default())
    });
    let file = option(&args, "--settings").unwrap_or_else(|| settings_path(&socket));
    let (Some(chosen), Some(first)) = (chosen(&args), lamp_of_the_options(&args)) else {
        return eprintln!("{USAGE}");
    };
    // The file can change between two events: the values, and the lights.
    let settings = || {
        let read = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let lamps: Vec<Found> = first.iter().cloned().chain(read.iter().flat_map(lamps_of)).collect();
        (read.map_or(chosen, |read| chosen.over(&read)), lamps)
    };
    // Started by Bloom: the end of the pipe from it (Bloom quit, or the
    // user turned the lights off) ends this program, after the lights have
    // what they had.
    let leave = Arc::new(AtomicBool::new(false));
    let connection: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    if has("--with-parent") {
        let (leave, connection) = (leave.clone(), connection.clone());
        thread::spawn(move || {
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            leave.store(true, Ordering::Release);
            // The read of the events ends, and the lights are put back.
            if let Some(stream) = &*connection.lock().unwrap() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
    }
    let (first_settings, mut lamps) = settings();
    let mut chosen_lights = lights_of(&lamps, &[], dry_run).0;
    let all = |chosen: &[(Found, Light)]| Lights(chosen.iter().map(|(_, light)| light.clone()).collect());
    let mut lights = all(&chosen_lights);
    if lights.0.is_empty() {
        eprintln!("bloom-lights: no light is chosen yet");
    }
    let mut film = Film::default();
    let mut aura = first_settings.aura;
    while !leave.load(Ordering::Acquire) {
        if let Ok(stream) = UnixStream::connect(&socket) {
            eprintln!("bloom-lights: reading {socket}");
            *connection.lock().unwrap() = stream.try_clone().ok();
            // The pipe from Bloom ended between the look above and here:
            // its thread found no connection to end. This one ends now.
            if leave.load(Ordering::Acquire) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
                let name = event["event"].as_str().unwrap_or_default();
                let (now, chosen_now) = settings();
                let (gap, glide) = (Duration::from_secs_f64(now.gap), now.aura_glide);
                // Other lights were chosen. A light that is not chosen any
                // more gets back what it had; a light that stays goes on as
                // it is; a new light takes a film that plays, from its start
                // (the rules for it run on a copy: the film itself goes on).
                if chosen_now != lamps {
                    lamps = chosen_now;
                    let (now_chosen, new) = lights_of(&lamps, &chosen_lights, dry_run);
                    let gone = chosen_lights.iter().filter(|(lamp, _)| !now_chosen.iter().any(|(kept, _)| same_lamp(kept, lamp)));
                    Lights(gone.map(|(_, light)| light.clone()).collect()).act(&Action::Restore, gap, glide);
                    for action in restart(now, &mut film.clone()) {
                        new.act(&action, gap, glide);
                    }
                    chosen_lights = now_chosen;
                    lights = all(&chosen_lights);
                }
                let before = switched(&mut aura, now, &mut film, lights.streams());
                for action in before.into_iter().chain(react(name, &event["data"], now, &mut film)) {
                    lights.act(&action, gap, glide);
                }
            }
            *connection.lock().unwrap() = None;
            for action in ended(&mut film) {
                lights.act(&action, Duration::from_secs_f64(settings().0.gap), 0.);
            }
        }
        if leave.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_secs(2));
    }
    // A light on its way back has a command left to send: the program
    // ends after it, or the light would stop half-way.
    lights.settle(Duration::from_secs(35));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The light of the command line: an address, with its port when one
    /// is given; a port that is none is refused.
    #[test]
    fn the_light_of_the_command_line() {
        let args = |text: &str| text.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(lamp_of_the_options(&args("--dry-run")), Some(None));
        let lamp = lamp_of_the_options(&args("--ip 10.0.0.5")).unwrap().unwrap();
        assert_eq!((lamp.driver.as_str(), lamp.address.as_str()), ("govee", "10.0.0.5"));
        assert_eq!(lamp_of_the_options(&args("--ip 10.0.0.5 --port 5000")).unwrap().unwrap().address, "10.0.0.5:5000");
        for port in ["70000", "-1", "nan"] {
            assert_eq!(lamp_of_the_options(&args(&format!("--ip 10.0.0.5 --port {port}"))), None);
        }
        assert!(driver::open(&lamp).is_some());
    }

    /// The first word says what the program is run for: a light whose
    /// address or driver reads "--scan" (a settings file edited by hand)
    /// does not turn "blink" into a look on the network.
    #[test]
    fn the_first_word_decides_what_the_program_does() {
        let args = |text: &str| text.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(run_of(&args("--scan")), Some(Run::Scan(2.)));
        assert_eq!(run_of(&args("--scan 0.5")), Some(Run::Scan(0.5)));
        assert_eq!(run_of(&args("--scan nan")), Some(Run::Scan(2.)));
        assert_eq!(run_of(&args("--identify govee 10.0.0.5")), Some(Run::Identify("govee".into(), "10.0.0.5".into())));
        assert_eq!(run_of(&args("--identify govee --scan")), Some(Run::Identify("govee".into(), "--scan".into())));
        assert_eq!(run_of(&args("--identify --scan --scan")), Some(Run::Identify("--scan".into(), "--scan".into())));
        assert_eq!(run_of(&args("--identify govee")), None);
        assert_eq!(run_of(&args("--ip 10.0.0.5 --scan")), Some(Run::Follow));
        assert_eq!(run_of(&[]), Some(Run::Follow));
    }

    /// Other lights chosen: a light that stays is the same light as before
    /// (it keeps what it knows of its lamp), also at a new address; only a
    /// lamp that was not there is new; a lamp with no id is known by its
    /// address.
    #[test]
    fn a_light_that_stays_is_kept_as_it_is() {
        let lamp = |id: &str, address: &str| Found { driver: "govee".into(), id: id.into(), address: address.into(), ..Found::default() };
        let (a, b, c) = (lamp("", "127.0.0.1:1"), lamp("", "127.0.0.1:2"), lamp("", "127.0.0.1:3"));
        let (first, new) = lights_of(&[a.clone(), b.clone()], &[], false);
        assert_eq!((first.len(), new.0.len()), (2, 2));
        first[0].1.act(Action::Fade { to: 100., kelvin: None }, Duration::ZERO, 0.05);
        let (second, new) = lights_of(&[c.clone(), a.clone()], &first, false);
        assert_eq!((second.len(), new.0.len()), (2, 1), "only the lamp that was not there is new");
        assert!(same_lamp(&second[1].0, &a) && same_lamp(&second[0].0, &c));
        assert!(same_lamp(&lamp("AA", "1.1.1.1"), &lamp("AA", "2.2.2.2")) && !same_lamp(&lamp("AA", "1.1.1.1"), &lamp("BB", "1.1.1.1")));
        assert!(same_lamp(&lamp("", "1.1.1.1"), &lamp("AA", "1.1.1.1")) && !same_lamp(&a, &b));
        // An entry that cannot be opened is left out, and is not new.
        let (third, new) = lights_of(&[lamp("", "not an address")], &second, false);
        assert!(third.is_empty() && new.0.is_empty());
    }
}
