// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The splash at the start: the logo blooms, the name fades in under it,
//! and the whole thing fades away over the page that loaded meanwhile.
//!
//! The splash is a layer over the root element, so the page under it (the
//! connect page, or the home page with its requests) builds and loads as
//! if it were not there; the splash never adds a wait. `BLOOM_NO_SPLASH`
//! skips it, for tests and screenshots.

use std::time::{Duration, Instant};

use gpui_kit::{
    AnyElement, BoxShadow, Context, IntoElement, ParentElement as _, Styled, Window, div, point,
    px, rgba,
};

use crate::{app::Bloom, ui::theme::UiTheme};

/// How long the splash shows before it starts to fade.
const HOLD: Duration = Duration::from_millis(1200);
/// The fade of the whole splash at the end.
const FADE: Duration = Duration::from_millis(300);
/// The logo grows and the glow blooms over this time.
const BLOOM: Duration = Duration::from_millis(700);
/// The wordmark starts to show at this time and takes `WORD` to show.
const WORD_AT: Duration = Duration::from_millis(350);
const WORD: Duration = Duration::from_millis(450);
/// The ring swells out over this time.
const RING: Duration = Duration::from_millis(1000);

/// The size of the icon when it has grown.
const LOGO: f32 = 112.;
/// The ink of the theme.
const INK: u32 = 0xf5f5f7;

/// The state of the splash: when its first frame was drawn, and whether
/// it is over.
pub struct Splash {
    /// The first frame, not the start of the app: the window shows about
    /// half a second after `Bloom::new`, and the bloom is for the eye.
    started: Option<Instant>,
    done: bool,
}

impl Default for Splash {
    fn default() -> Self {
        Self {
            started: None,
            done: crate::options::options().no_splash,
        }
    }
}

/// The share of `duration` that has passed since `at` after the start,
/// clamped to 0..=1.
fn phase(elapsed: Duration, at: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        return if elapsed >= at { 1. } else { 0. };
    }
    (elapsed.saturating_sub(at).as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.)
}

impl Bloom {
    /// The splash while it runs, or nothing once it is over. Asks for a
    /// frame while it runs; every frame is one small tree, no blur.
    pub fn render_splash(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.splash.done {
            return None;
        }
        let elapsed = self.splash.started.get_or_insert_with(Instant::now).elapsed();
        if elapsed >= HOLD + FADE {
            self.splash.done = true;
            return None;
        }
        let theme = UiTheme::read(cx);
        let still = theme.motion.reduced || cx.reduce_motion();
        let ease_out = gpui_kit::ease_out_quint();
        // A gentler ease-out than the theme's, so the growth is seen.
        let ease_out_cubic = |p: f32| 1. - (1. - p).powi(3);
        // The logo grows from half its size while it shows.
        let grow = if still { 1. } else { ease_out_cubic(phase(elapsed, Duration::ZERO, BLOOM)) };
        let logo_in = phase(elapsed, Duration::ZERO, Duration::from_millis(250));
        // The glow blooms out past the logo and thins as it goes; the ring
        // goes further and takes longer, with a gentler curve, so the eye
        // follows it out.
        let glow = if still { 1. } else { ease_out(phase(elapsed, Duration::from_millis(80), BLOOM)) };
        let ring = if still { 1. } else { phase(elapsed, Duration::from_millis(120), RING) };
        let ring_out = ease_out_cubic(ring);
        let word = if still {
            phase(elapsed, Duration::ZERO, Duration::from_millis(250))
        } else {
            gpui_kit::ease_in_out(phase(elapsed, WORD_AT, WORD))
        };
        let out = gpui_kit::ease_in_out(phase(elapsed, HOLD, FADE));
        window.request_animation_frame();

        let logo = LOGO * (0.5 + 0.5 * grow);
        // The ring starts at the logo and swells to about two and a half
        // times it, thinning on the way.
        let ring_size = LOGO * (1. + 1.5 * ring_out);
        let ring_alpha = 0.5 * (1. - ring);
        // The soft glow behind the logo: the blurred shadow of a quad with
        // no fill, so it has no edge. It peaks as it blooms and settles at
        // a faint halo.
        let disc = LOGO * (0.4 + 0.6 * glow);
        let disc_alpha = 0.10 + 0.20 * (1. - (2. * glow - 1.).abs());
        let alpha = |a: f32| (INK << 8) | ((a.clamp(0., 1.) * 255.) as u32);
        // The box that holds the mark; the glow is centred in it.
        let stage = LOGO * 2.4;
        let centred = |size: f32| {
            div()
                .absolute()
                .left(px((stage - size) / 2.))
                .top(px((stage - size) / 2.))
                .size(px(size))
                .rounded_full()
        };

        Some(
            div()
                .absolute()
                .inset_0()
                .opacity(1. - out)
                .bg(theme.colors.background)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .relative()
                        .size(px(stage))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(centred(disc).shadow(vec![BoxShadow {
                            color: rgba(alpha(disc_alpha)).into(),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(disc * 0.6),
                            spread_radius: px(0.),
                            inset: false,
                        }]))
                        .child(centred(ring_size).border_1().border_color(rgba(alpha(ring_alpha))))
                        .child(
                            div()
                                .opacity(logo_in)
                                // The icon of the app, the jellyfish, as on
                                // the Dock and the home screen.
                                .child(crate::icons::app_icon(logo).rounded(px(logo * 0.22))),
                        ),
                )
                .child(
                    div()
                        .mt(px(-6.))
                        // Rises a little as it shows.
                        .pt(px(10. * (1. - word)))
                        .opacity(word)
                        .text_size(px(26.))
                        .line_height(px(32.))
                        .font_family(theme.fonts.heading.clone())
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(rgba(alpha(1.)))
                        .child(crate::brand::NAME),
                )
                .into_any_element(),
        )
    }
}
