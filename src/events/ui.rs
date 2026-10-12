// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Automation page of the settings.

use gpui_kit::{Context, Entity};

use super::{FilmColour, colours::hue_colour, hub, lights_path, socket_path};
use crate::{app::Bloom, ui::slider::SliderState};

impl Bloom {
    /// The Automation page of the settings: the switch of the stream, what
    /// an event may say, where a program reads it, and how.
    pub(crate) fn render_settings_automation(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        use gpui_kit::{ClickEvent, ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px};

        use crate::{
            settings::{checkbox, field, group, raised},
            ui::slider::Slider,
        };

        let on = self.events_on();
        let titles = self.config.events_titles == Some(true);
        let path = socket_path();
        let mut events = group("Events for other programs", cx).child(field(
            "Send events (experimental)",
            if on {
                "On. A program on this Mac can read what the player does, as it happens. It cannot control the app."
            } else {
                "Off. When on, a program on this Mac can read what the player does, as it happens: for lights that dim at the start of a film, a status, a notice. It cannot control the app."
            },
            checkbox("automation.events", on, cx)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_events(!on, cx))),
            cx,
        ));
        if on {
            events = events
                .child(field(
                    "Name the title in events",
                    if titles {
                        "On. An event has the title, the series and the episode of what plays. A program that reads the events can keep them."
                    } else {
                        "Off. An event has the id and the kind of the item only; a program that reads the events learns no title."
                    },
                    checkbox("automation.titles", titles, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_events_titles(!titles, cx))),
                    cx,
                ))
                .child(field(
                    "Tell the colours of the picture",
                    if self.config.events_colours == Some(true) {
                        "On. While a film plays and a program reads, an event has the colours of the picture, from its top to its bottom, 12 times a second. For a light that follows the film."
                    } else {
                        "Off. When on, an event has the colours of the picture 12 times a second, for a light that follows the film."
                    },
                    checkbox("automation.colours", self.config.events_colours == Some(true), cx).on_click(cx.listener({
                        let on = self.config.events_colours == Some(true);
                        move |this, _: &ClickEvent, _, cx| this.set_events_colours(!on, cx)
                    })),
                    cx,
                ))
                .child(field(
                    "Where a program reads them",
                    format!("{path}. Programs that read now: {}.", hub().readers()),
                    raised("automation.copy", "Copy the path", cx).on_click(cx.listener({
                        let path = path.clone();
                        move |this, _: &ClickEvent, _, cx| {
                            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(path.clone()));
                            this.toast("Automation", "The path is copied.", cx);
                        }
                    })),
                    cx,
                ));
        }
        let how = group("What a program gets", cx)
            .child(field(
                "One line for each event",
                "Each event is one line of JSON: the start, a pause, a seek, the end of an item, the start of the credits, full screen, picture in picture. The access token, the address of the server and the paths of files are never in an event.",
                div(),
                cx,
            ))
            .child(field(
                "Try it",
                format!("In a terminal: nc -U \"{path}\" . Then play something. The format is in specs/outbound-events.md; the lights helper (lights/) is a program that reads it."),
                div(),
                cx,
            ));
        let page = div().flex().flex_col().gap(px(18.)).child(events).child(how);
        let Some(panel) = &self.events.lights else { return page };
        // The lights (experimental): a helper program that Bloom starts
        // reads the events and tells the lights. Its settings are here.
        let settings = self.lights_json(cx);
        let slider = |state: &Entity<SliderState>, label: &'static str| {
            div().w(px(220.)).child(Slider::new(state).aria_label(label).w_full())
        };
        let whole = |name: &str| settings[name].as_f64().unwrap_or(0.).round() as i64;
        let percent = |name: &str| format!("{} %", whole(name));
        let (white, colour, aura) = (panel.white, panel.colour, panel.aura);
        let running = self.events.test_run.is_some();
        let lights_on = self.lights_on();
        let mut lights = group("Lights (experimental)", cx).child(field(
            "Control lights",
            if lights_on {
                "On. A helper program of Bloom dims the lights you choose below while a film plays, and gives them back at its end. Experimental: Govee lights with \"LAN Control\" on in the Govee Home app. Nothing goes to the internet."
            } else {
                "Off. When on, a helper program of Bloom dims the lights you choose while a film plays, or lets them follow the picture. Experimental: Govee lights with \"LAN Control\" on in the Govee Home app."
            },
            checkbox("automation.lights", lights_on, cx)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_lights(!lights_on, cx))),
            cx,
        ));
        if !lights_on {
            return page.child(lights);
        }
        lights = lights.child(field(
            "Find lights",
            if panel.scanning {
                "Looks on your network for two seconds."
            } else {
                "Looks on your network for lights that answer. A light that is missing: is \"LAN Control\" on for it in the Govee Home app, and is it on the same network as this Mac?"
            },
            raised("automation.find", if panel.scanning { "Looking" } else { "Find" }, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.lights_scan(cx))),
            cx,
        ));
        let rows = self.lights_rows();
        if rows.is_empty() {
            lights = lights.child(field("No light is chosen", "Find the lights, then tick the ones Bloom may use.", div(), cx));
        }
        for (at, (lamp, chosen)) in rows.into_iter().enumerate() {
            let text = |name: &str| lamp[name].as_str().unwrap_or_default().to_string();
            let name = match (text("driver").as_str(), text("model")) {
                ("govee", model) if model.is_empty() => "Govee light".to_string(),
                ("govee", model) => format!("Govee {model}"),
                (driver, model) => format!("{driver} {model}"),
            };
            let detail = [text("id"), text("address")].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" \u{b7} ");
            let (identify, choose) = (lamp.clone(), lamp.clone());
            lights = lights.child(field(
                name,
                if chosen { format!("{detail}. Bloom uses this light.") } else { detail },
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(raised(format!("automation.identify.{at}"), "Blink", cx).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.lights_identify(&identify, cx),
                    )))
                    .child(checkbox(format!("automation.lamp.{at}"), chosen, cx).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.lights_choose(&choose, cx),
                    ))),
                cx,
            ));
        }
        // A test film, in a development instance only.
        if crate::options::options().debug_sock.is_some() {
            lights = lights.child(field(
                "Test film (development)",
                if !on {
                    "Turn the events on first."
                } else if running {
                    "Runs: start, pause, resume, credits, end, 3 seconds apart."
                } else {
                    "Sends the events of a short film to the programs that read: start, pause, resume, credits, end, 3 seconds apart. No film plays."
                },
                raised("automation.test", if running { "Run again" } else { "Run" }, cx).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        let answer = this.events_test_run(cx);
                        this.toast("Automation", format!("Test film: {answer}."), cx);
                    },
                )),
                cx,
            ));
        }
        lights = lights
            .child(field(
                "Follow the picture",
                if aura {
                    "On. The light shows the colours of the picture along its length, the top of the picture at its top. The brightness and colour settings below are not used, but for the white of a pause."
                } else {
                    "Off. When on, the light shows the colours of the picture along its length."
                },
                checkbox("automation.aura", aura, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.lights_set("aura", if aura { "false" } else { "true" }, cx);
                })),
                cx,
            ))
            .child(field(
                "Speed of the picture's light",
                format!("{} s to come to a new colour. Low follows a cut at once, high is calm.", settings["aura_glide"]),
                slider(&panel.glide, "Speed of the picture's light"),
                cx,
            ))
            .child(field("Light during a film", percent("film"), slider(&panel.film, "Light during a film"), cx))
            .child(field("Light at a pause", percent("pause"), slider(&panel.pause, "Light at a pause"), cx))
            .child(field("Light at the credits", percent("credits"), slider(&panel.credits, "Light at the credits"), cx))
            .child(field(
                "Time of a fade down",
                format!("{} s between its two steps. The light glides to each step by itself.", settings["gap"]),
                slider(&panel.gap, "Time of a fade down"),
                cx,
            ))
            .child(field(
                "White light at a pause",
                if white { "On. A pause shows white light." } else { "Off. A pause keeps the colour of the film." },
                checkbox("automation.white", white, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.lights_set("pause_white", if white { "false" } else { "true" }, cx);
                })),
                cx,
            ));
        if white {
            lights = lights.child(field(
                "Warmth of the white",
                format!("{} K. Low is warm, high is cool.", whole("pause_kelvin")),
                slider(&panel.kelvin, "Warmth of the white"),
                cx,
            ));
        }
        let (label, detail, next) = match colour {
            FilmColour::Keep => ("The light's own", "A film keeps the colour the light has.".to_string(), "artwork"),
            FilmColour::Artwork => (
                "From the artwork",
                match self.events.colour {
                    Some([r, g, b]) => format!("A film shows the strongest colour of its artwork. The last one: {r}, {g}, {b}."),
                    None => "A film shows the strongest colour of its artwork.".to_string(),
                },
                "fixed",
            ),
            FilmColour::Fixed => ("One colour", "Every film shows the colour below.".to_string(), "keep"),
        };
        lights = lights.child(field(
            "Colour during a film",
            detail,
            raised("automation.colour", label, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.lights_set("colour", next, cx);
            })),
            cx,
        ));
        if colour == FilmColour::Fixed {
            let [r, g, b] = hue_colour(panel.hue.read(cx).value().end());
            lights = lights.child(field(
                "The colour",
                format!("{r}, {g}, {b}"),
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(div().size(px(22.)).rounded_full().bg(gpui_kit::rgb(
                        u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
                    )))
                    .child(slider(&panel.hue, "The colour")),
                cx,
            ));
        }
        lights = lights.child(field(
            "Where the helper reads these",
            format!("{}. A change is used from the next start, pause or resume; a colour for the film from the next film.", lights_path()),
            div(),
            cx,
        ));
        page.child(lights)
    }
}
