// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frosted glass: a panel that shows a blurred copy of what lies behind it.
//! The blur comes from the patched Metal renderer in `vendor/gpui-pre-apple`,
//! which treats a quad with a dashed border style and no border as a request
//! to blur its backdrop.
//!
//! Liquid glass (a prototype, off by default): the same quad with a curved
//! bezel at its edge that refracts what is behind it, a specular rim and
//! more saturation under the glass, after Apple's Liquid Glass and kube.io's
//! model of it. `BLOOM_GLASS=liquid` at start or the debug verb `glass`
//! turn it on; the parameters ride in fields of the quad the blur path does
//! not use (`encode`, and "liquid glass" in vendor/README.md).

use std::sync::{LazyLock, RwLock};

use gpui_kit::{
    App, BorderStyle, Bounds, Canvas, Corners, Edges, Hsla, PaintQuad, Pixels, Rgba, Styled,
    canvas, linear_color_stop, linear_gradient,
};

use super::theme::{ThemeMode, UiTheme};

/// The tint of every popup of the app (menus, the episode list, the
/// SyncPlay panel): the frosted glass of the web theme, light enough that
/// what is behind shows through.
pub const POPUP_TINT: Rgba = Rgba { r: 0.165, g: 0.165, b: 0.165, a: 0.72 };

/// How a glass quad is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Today's look: the blurred backdrop under a flat tint.
    Frosted,
    /// The prototype: a refracting bezel, a rim light, saturation.
    Liquid,
}

/// The parameters of liquid glass for one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Liquid {
    /// Width of the curved bezel at the edge, in logical pixels.
    pub bezel: f32,
    /// Thickness of the slab of glass under the bezel, in logical pixels:
    /// the backdrop at the very edge moves about 1.1 times this far in.
    pub thickness: f32,
    /// Strength of the specular rim on the edges that face the light
    /// (top left), 0 to 1.
    pub rim: f32,
    /// Width of the specular rim, in logical pixels.
    pub rim_width: f32,
    /// Strength of the counter-light on the far edges, as a share of `rim`.
    pub counter: f32,
    /// Saturation of the backdrop under the glass; 1 leaves it as it is.
    pub saturation: f32,
    /// Share of blur in the bezel: 0 is a sharp lens, 1 the blur of the
    /// frosted glass.
    pub blur: f32,
    /// Share of blur in the flat middle: 1 is the frosted glass, which
    /// text on a panel needs; 0 is clear glass.
    pub frost: f32,
    /// Where the sharp sample of the bezel comes from: `false` the
    /// quarter-size copy of the frame the blur is made from (no extra
    /// pass), `true` a full-size copy (one blit of the frame per glass run).
    pub full: bool,
    /// The tint over the backdrop, for quads that ask for `POPUP_TINT`.
    pub tint: Rgba,
}

/// The settings of the prototype, for both themes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub mode: Mode,
    pub dark: Liquid,
    pub light: Liquid,
    /// A page background for the light theme, for the screenshots of the
    /// prototype (`glass ground=#cfd3da`); `None` is the theme's own.
    pub ground: Option<Rgba>,
}

impl Settings {
    /// The parameter set of a theme mode.
    pub fn for_mode(&self, mode: ThemeMode) -> &Liquid {
        match mode {
            ThemeMode::Dark => &self.dark,
            ThemeMode::Light => &self.light,
        }
    }

    fn for_mode_mut(&mut self, mode: ThemeMode) -> &mut Liquid {
        match mode {
            ThemeMode::Dark => &mut self.dark,
            ThemeMode::Light => &mut self.light,
        }
    }
}

/// The starting values. Dark is tuned by eye: a light tint and a half
/// clear middle, so the glass takes the colour of the picture behind it,
/// and a thin lit edge. Light is not tuned yet: a white tint of 0.34.
const DARK: Liquid = Liquid {
    bezel: 16.,
    thickness: 20.,
    rim: 0.4,
    rim_width: 0.75,
    counter: 0.6,
    saturation: 1.4,
    blur: 0.,
    frost: 0.5,
    full: true,
    tint: Rgba { r: 0.11, g: 0.11, b: 0.12, a: 0.35 },
};
const LIGHT: Liquid = Liquid {
    bezel: 16.,
    thickness: 12.,
    rim: 0.8,
    rim_width: 0.75,
    counter: 0.6,
    saturation: 1.4,
    blur: 0.,
    frost: 1.,
    full: true,
    tint: Rgba { r: 1., g: 1., b: 1., a: 0.34 },
};

static SETTINGS: LazyLock<RwLock<Settings>> = LazyLock::new(|| {
    let mode = match std::env::var("BLOOM_GLASS").as_deref() {
        Ok("liquid") => Mode::Liquid,
        _ => Mode::Frosted,
    };
    RwLock::new(Settings { mode, dark: DARK, light: LIGHT, ground: None })
});

/// The current settings.
pub fn settings() -> Settings {
    *SETTINGS.read().unwrap()
}

/// Whether liquid glass is on.
pub fn liquid() -> bool {
    settings().mode == Mode::Liquid
}

/// Whether the panels are liquid glass in the light theme, where the ink
/// inside them is the dark ink of the theme and not the light ink of the
/// dark panels. The prototype uses it in the few components it shows.
pub fn liquid_light(cx: &App) -> bool {
    liquid() && UiTheme::read(cx).mode == ThemeMode::Light
}

/// The page background the prototype asks for in the light theme.
pub fn ground() -> Option<Rgba> {
    settings().ground
}

/// A layer that fills its parent with blurred backdrop, tinted by `tint`.
/// Put it first in a `relative()` parent, under the panel's content.
pub fn glass(radius: Pixels, tint: Rgba) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, cx| {
            // The shader does not clamp the radius; a pill passes a large one.
            let radius = radius.min(bounds.size.width.min(bounds.size.height) / 2.);
            // Bench switch: plain tint without the blur pass.
            let style = if std::env::var_os("BLOOM_NO_GLASS").is_some() {
                BorderStyle::Solid
            } else {
                BorderStyle::Dashed
            };
            let settings = settings();
            let liquid = (settings.mode == Mode::Liquid && style == BorderStyle::Dashed)
                .then(|| *settings.for_mode(UiTheme::read(cx).mode));
            let quad = match liquid {
                Some(liquid) => encode(bounds, radius, tint, &liquid, window.scale_factor()),
                None => PaintQuad {
                    bounds,
                    corner_radii: Corners::all(radius),
                    background: tint.into(),
                    border_widths: Edges::default(),
                    border_color: gpui_kit::transparent_black(),
                    border_style: style,
                },
            };
            window.paint_quad(quad);
        },
    )
    .absolute()
    .inset_0()
}

/// The quad of a liquid glass panel. The renderer's marker for a backdrop
/// quad stays (dashed border style, no border); the parameters go in fields
/// that such a quad does not draw:
/// - `border_color`: h the bezel width, s the glass thickness (both in
///   device pixels), l 1 for the full-size source, 0 for the quarter-size.
/// - `background`, a linear gradient: the angle is the blur share of the
///   bezel; stop 0 is the tint with the rim width (device pixels) as its
///   percentage; stop 1's colour is (h saturation, s rim, l counter-light),
///   opaque so the quad never counts as transparent, with the blur share
///   of the flat middle as its percentage.
/// Alpha fields carry no parameter: `paint_quad` scales them by the
/// element's opacity.
fn encode(bounds: Bounds<Pixels>, radius: Pixels, tint: Rgba, liquid: &Liquid, scale: f32) -> PaintQuad {
    let tint = if tint == POPUP_TINT { liquid.tint } else { tint };
    PaintQuad {
        bounds,
        corner_radii: Corners::all(radius),
        background: linear_gradient(
            liquid.blur,
            linear_color_stop(tint, liquid.rim_width * scale),
            linear_color_stop(
                Hsla { h: liquid.saturation, s: liquid.rim, l: liquid.counter, a: 1. },
                liquid.frost,
            ),
        ),
        border_widths: Edges::default(),
        border_color: Hsla {
            h: (liquid.bezel * scale).max(0.5),
            s: liquid.thickness * scale,
            l: if liquid.full { 1. } else { 0. },
            a: 0.,
        },
        border_style: BorderStyle::Dashed,
    }
}

/// The keys of the debug verb, with their ranges, for `glass help`.
pub const HELP: &str = "glass [frosted|liquid] [subtle|medium|strong] [key=value ...]\n\
  modes:   frosted (today's look), liquid (the prototype)\n\
  presets: subtle, medium, strong (liquid; bezel, thickness, rim, frost)\n\
  keys of the current theme's set (a number unless said):\n\
    bezel=px        width of the curved edge, 0 to 40 (16)\n\
    thickness=px    of the glass; the edge bends about 1.1 times this, 0 to 40 (20)\n\
    rim=0..1        specular line on the edges facing the light (dark 0.4, light 0.8)\n\
    rimwidth=px     width of that line (0.75)\n\
    counter=0..1    the same line on the far edges, share of rim (0.6)\n\
    saturation=x    of the backdrop under the glass, 1 is unchanged (1.4)\n\
    blur=0..1       blur share in the bezel, 0 sharp lens, 1 all blur (0)\n\
    frost=0..1      blur share in the flat middle, 0 clear, 1 frosted (dark 0.5, light 1)\n\
    full=0|1        sharp source: 1 full-size copy of the frame, 0 quarter (1)\n\
    alpha=0..1      opacity of the tint (dark 0.35, light 0.34)\n\
    tint=#rrggbb    colour of the tint (dark #1c1c1f, light #ffffff)\n\
    ground=#rrggbb  page background of the light theme, or none";

/// Named parameter sets for a quick comparison; the tint stays.
fn preset(name: &str) -> Option<[f32; 4]> {
    // bezel, thickness, rim, frost
    Some(match name {
        "subtle" => [12., 6., 0.45, 1.],
        "medium" => [16., 12., 0.6, 1.],
        "strong" => [24., 22., 0.8, 0.6],
        _ => return None,
    })
}

/// The debug verb `glass [frosted|liquid] [preset] [key=value ...]`: the
/// mode, a preset, then parameters of the set of `mode` (the current
/// theme); see `HELP`. Returns the settings, or an error line.
pub fn apply(line: &str, mode: ThemeMode) -> Result<Settings, String> {
    let mut settings = settings();
    for word in line.split_whitespace() {
        match word {
            "frosted" => settings.mode = Mode::Frosted,
            "liquid" => settings.mode = Mode::Liquid,
            "help" => return Err(HELP.to_string()),
            _ if preset(word).is_some() => {
                let [bezel, thickness, rim, frost] = preset(word).unwrap();
                settings.mode = Mode::Liquid;
                let set = settings.for_mode_mut(mode);
                set.bezel = bezel;
                set.thickness = thickness;
                set.rim = rim;
                set.frost = frost;
            }
            _ => {
                let (key, value) = word.split_once('=').ok_or_else(|| format!("error: {word:?} is not key=value"))?;
                let number = || value.parse::<f32>().map_err(|_| format!("error: {key}={value:?} is not a number"));
                let set = settings.for_mode_mut(mode);
                match key {
                    "bezel" => set.bezel = number()?.max(0.),
                    "thickness" | "refraction" => set.thickness = number()?.max(0.),
                    "rimwidth" => set.rim_width = number()?.max(0.25),
                    "rim" => set.rim = number()?.max(0.),
                    "counter" => set.counter = number()?.max(0.),
                    "saturation" | "sat" => set.saturation = number()?.max(0.),
                    "blur" => set.blur = number()?.clamp(0., 1.),
                    "frost" => set.frost = number()?.clamp(0., 1.),
                    "full" => set.full = number()? >= 0.5,
                    "alpha" => set.tint.a = number()?.clamp(0., 1.),
                    "tint" => {
                        let a = set.tint.a;
                        set.tint = Rgba { a, ..parse_color(value)? };
                    }
                    "ground" => {
                        settings.ground = if value == "none" { None } else { Some(parse_color(value)?) };
                    }
                    _ => return Err(format!("error: no key {key:?}\n{HELP}")),
                }
            }
        }
    }
    *SETTINGS.write().unwrap() = settings;
    Ok(settings)
}

fn parse_color(value: &str) -> Result<Rgba, String> {
    let hex = value.trim_start_matches('#');
    if hex.len() != 6 {
        return Err(format!("error: {value:?} is not #rrggbb"));
    }
    let number = u32::from_str_radix(hex, 16).map_err(|_| format!("error: {value:?} is not #rrggbb"))?;
    Ok(gpui_kit::rgb(number))
}

/// The mode and the set of `mode` on one line, the answer of the verb;
/// with `help`, the keys and their ranges as well.
pub fn describe(settings: &Settings, mode: ThemeMode, help: bool) -> String {
    let set = settings.for_mode(mode);
    let ground = settings.ground.map(|g| format!(" ground={}", hex(g))).unwrap_or_default();
    let help = if help { format!("\n{HELP}") } else { String::new() };
    format!(
        "glass {} {:?}: bezel={} thickness={} rim={} rimwidth={} counter={} saturation={} blur={} frost={} full={} tint={} alpha={:.2}{ground}{help}",
        match settings.mode {
            Mode::Frosted => "frosted",
            Mode::Liquid => "liquid",
        },
        mode,
        set.bezel,
        set.thickness,
        set.rim,
        set.rim_width,
        set.counter,
        set.saturation,
        set.blur,
        set.frost,
        u8::from(set.full),
        hex(set.tint),
        set.tint.a
    )
}

fn hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", (c.r * 255.).round() as u8, (c.g * 255.).round() as u8, (c.b * 255.).round() as u8)
}
