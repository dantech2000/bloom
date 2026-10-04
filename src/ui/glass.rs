// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frosted glass: a panel that shows a blurred copy of what lies behind it.
//! The blur comes from the patched Metal renderer in `vendor/gpui-pre-apple`,
//! which treats a quad with a dashed border style and no border as a request
//! to blur its backdrop.

use gpui_kit::{
    BorderStyle, Bounds, Canvas, Corners, Edges, PaintQuad, Pixels, Rgba, Styled, canvas,
};

/// The tint of every popup of the app (menus, the episode list, the
/// SyncPlay panel): the frosted glass of the web theme, light enough that
/// what is behind shows through.
pub const POPUP_TINT: Rgba = Rgba { r: 0.165, g: 0.165, b: 0.165, a: 0.72 };

/// A layer that fills its parent with blurred backdrop, tinted by `tint`.
/// Put it first in a `relative()` parent, under the panel's content.
pub fn glass(radius: Pixels, tint: Rgba) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            // The shader does not clamp the radius; a pill passes a large one.
            let radius = radius.min(bounds.size.width.min(bounds.size.height) / 2.);
            // Bench switch: plain tint without the blur pass.
            let style = if std::env::var_os("BLOOM_NO_GLASS").is_some() {
                BorderStyle::Solid
            } else {
                BorderStyle::Dashed
            };
            window.paint_quad(PaintQuad {
                bounds,
                corner_radii: Corners::all(radius),
                background: tint.into(),
                border_widths: Edges::default(),
                border_color: gpui_kit::transparent_black(),
                border_style: style,
            });
        },
    )
    .absolute()
    .inset_0()
}
