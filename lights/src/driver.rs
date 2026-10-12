// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one interface to a brand of light. A driver finds the lamps of its
//! brand on the local network and speaks to one of them; everything else
//! (the rules, the fades, the glide of the picture's colours) is the same
//! for every brand and lives outside the drivers.
//!
//! A new brand is one file with a [`Driver`] and a [`Lamp`], and a line in
//! [`drivers`]. Add one only with a lamp of that brand at hand to try it.

use std::{sync::Arc, time::Duration};

use serde_json::{Value, json};

/// A lamp a driver found, or one the user chose earlier. The `id` is the
/// name the lamp has for good (its address can change with the network).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub driver: String,
    pub id: String,
    pub model: String,
    pub address: String,
}

impl Found {
    pub fn json(&self) -> Value {
        json!({ "driver": self.driver, "id": self.id, "model": self.model, "address": self.address })
    }

    /// None for an entry with no driver or no address in it.
    pub fn from_json(value: &Value) -> Option<Self> {
        let text = |name: &str| value[name].as_str().map(str::to_string);
        Some(Self {
            driver: text("driver").filter(|driver| !driver.is_empty())?,
            id: text("id").unwrap_or_default(),
            model: text("model").unwrap_or_default(),
            address: text("address").filter(|address| !address.is_empty())?,
        })
    }
}

/// The lamps of a settings file (`"lamps": [...]`); entries that cannot be
/// read are left out.
pub fn lamps_of(file: &Value) -> Vec<Found> {
    file["lamps"].as_array().into_iter().flatten().filter_map(Found::from_json).collect()
}

/// What a lamp shows: its brightness in percent, and a colour or a white.
#[derive(Clone, Debug, PartialEq)]
pub struct LampState {
    /// Switched on. A lamp that is off still answers, and keeps the
    /// brightness and the colour it will have when it is on again.
    pub on: bool,
    pub level: f64,
    /// None when the lamp did not say its colour.
    pub colour: Option<[u8; 3]>,
    /// A white of this temperature, in place of the colour.
    pub kelvin: Option<u32>,
}

/// One lamp. A call sends at once and does not wait for the lamp, but for
/// [`Lamp::state`]. Nothing here fades: a fade is several calls.
pub trait Lamp: Send + Sync {
    /// What the lamp shows now. None when it does not answer in a second.
    fn state(&self) -> Option<LampState>;
    /// Switches the lamp on or off.
    fn power(&self, on: bool);
    /// The brightness, 1 to 100. A lamp that is off can take this as "on"
    /// (a Govee lamp does): an off lamp that must stay off is told nothing.
    fn level(&self, percent: u32);
    fn colour(&self, colour: [u8; 3]);
    /// A white of a colour temperature in kelvin.
    fn white(&self, kelvin: u32);
    /// The colours one frame of the lamp's stream takes; 0 for a lamp with
    /// no stream, which then cannot follow the picture.
    fn frame_colours(&self) -> usize;
    /// Into the frame stream, or out of it. Out of it the lamp shows what
    /// it showed before.
    fn stream(&self, on: bool);
    /// One frame: `frame_colours` colours, from the bottom of the lamp (or
    /// its start) to its top.
    fn frame(&self, colours: &[[u8; 3]]);
}

/// One brand of light.
pub trait Driver {
    /// The name in settings files and on the command line.
    fn name(&self) -> &'static str;
    /// The lamps of this brand that answer within `wait`.
    fn find(&self, wait: Duration) -> Vec<Found>;
    /// A lamp at an address. None for an address that cannot be read.
    fn open(&self, found: &Found) -> Option<Arc<dyn Lamp>>;
}

/// Every driver there is.
pub fn drivers() -> Vec<Box<dyn Driver>> {
    vec![Box::new(crate::govee::Govee::default())]
}

/// The lamps of every brand that answer within `wait`.
pub fn find(wait: Duration) -> Vec<Found> {
    drivers().iter().flat_map(|driver| driver.find(wait)).collect()
}

/// The lamp of a found or chosen entry, by its driver.
pub fn open(found: &Found) -> Option<Arc<dyn Lamp>> {
    drivers().iter().find(|driver| driver.name() == found.driver)?.open(found)
}

/// A lamp that is none: it prints what it is told. For a dry run.
pub struct Printed;

impl Lamp for Printed {
    fn state(&self) -> Option<LampState> {
        None
    }
    fn power(&self, on: bool) {
        println!("light: {}", if on { "on" } else { "off" });
    }
    fn level(&self, percent: u32) {
        println!("light: brightness {percent}");
    }
    fn colour(&self, [r, g, b]: [u8; 3]) {
        println!("light: colour {r},{g},{b}");
    }
    fn white(&self, kelvin: u32) {
        println!("light: white {kelvin} K");
    }
    fn frame_colours(&self) -> usize {
        14
    }
    fn stream(&self, on: bool) {
        println!("light: frame stream {}", if on { "starts" } else { "ends" });
    }
    fn frame(&self, _: &[[u8; 3]]) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chosen lamp is kept with its id and its last address; an entry a
    /// person broke by hand is left out, and does not take the rest with it.
    #[test]
    fn the_lamps_of_a_settings_file_are_read() {
        let lamp = Found { driver: "govee".into(), id: "AA:BB".into(), model: "H6076".into(), address: "10.0.0.5".into() };
        assert_eq!(Found::from_json(&lamp.json()), Some(lamp.clone()));
        let file = json!({ "lamps": [lamp.json(), { "driver": "govee" }, "x", { "driver": "", "address": "1.2.3.4" }, { "driver": "govee", "address": "10.0.0.6" }] });
        let read = lamps_of(&file);
        assert_eq!(read.len(), 2);
        assert_eq!((read[0].clone(), read[1].id.as_str(), read[1].address.as_str()), (lamp, "", "10.0.0.6"));
        assert!(lamps_of(&json!({})).is_empty() && lamps_of(&json!({ "lamps": 3 })).is_empty());
        assert!(open(&Found { driver: "nobody".into(), ..read[0].clone() }).is_none());
        assert!(open(&Found { address: "not an address".into(), ..read[0].clone() }).is_none());
        assert!(open(&read[0]).is_some());
    }
}
