# Vendored crates

`mpv/lib` is libmpv, one library built from pinned sources by
`dev/build-mpv` (the versions, sources and checksums are in that script;
`mpv/build` is its cache). `sparkle` is the updater framework. The two `gpui-pre-*` crates are copies of the published crates
(crates.io, version 0.3.4, Apache-2.0, license file carried) with small
local changes, wired in by `[patch.crates-io]` in `Cargo.toml`. Most changes
are marked `Bloom` in the source; the list says where one is not.

## The changes as patches

`vendor/patches/<crate>.patch` is the whole local change of a crate, and
`vendor/patches/crates.txt` names the version and the SHA-256 of the
published `.crate` it applies to. `dev/vendor-crates` keeps the three in
step:

- `dev/vendor-crates check` builds each crate again from crates.io and its
  patch and compares the result with `vendor/`. It changes nothing.
- `dev/vendor-crates diff` writes the patches again from `vendor/`. Run it
  after an edit of vendored code and commit the patch with the edit.
- `dev/vendor-crates upgrade <crate> <version>` fetches that version,
  checks it against the index of crates.io, applies the patch and puts the
  result in `vendor/<crate>`. A hunk that does not apply stops the run and
  leaves `vendor/` as it was; the `.rej` files in `target/vendor-crates`
  say what to redo by hand.

Tried on 2026-10-12, with nothing kept: against `gpui-pre-apple` 0.3.9, 14
of the 17 hunks of `metal_renderer.rs` apply and 4 hunks in all need a
hand; `gpui-pre-macos` 0.3.9 needs one. `taffy` 0.15.0 has the change of
the patch below already (`unwrap_or_else`), so the vendored `taffy` can go
once GPUI builds with that version.

The parameters of the liquid glass ride in fields of the quad that the blur
path does not draw (see below, and `encode` in `src/ui/glass.rs`). A field
of their own would be in `Quad`, which is in `gpui-pre` itself: that crate
is not vendored, and it is not worth a third copy.

## gpui-pre-apple 0.3.4

- `src/metal_renderer.rs`, `src/shaders.metal`: a backdrop blur pass for
  the frosted glass of the popups (`ui::glass`): the frame so far is scaled
  down and blurred into a texture that backdrop-blur quads sample.
- Liquid glass (`liquid_glass` in `src/shaders.metal`, a prototype that
  `ui::glass` turns on with `BLOOM_GLASS=liquid` or the `glass` debug
  verb): a backdrop-blur quad whose border colour has `h > 0` refracts its
  backdrop in a bezel at its edge, with a specular rim and more
  saturation. Its parameters ride in fields the blur path does not draw:
  `border_color` (h bezel width, s glass thickness, both in device
  pixels; l 1 for a full-size copy of the frame as the sharp source, 0
  for the quarter-size copy, 0.5 for a card of the page, which takes the
  copy the frame already has) and a linear-gradient background (angle: the
  blur share in the bezel; stop 0: the tint, its percentage the width of
  the specular rim; stop 1's colour: h saturation, s rim, l counter-light,
  its percentage the blur share in the flat middle). The
  quad fragment shader binds the quarter-size copy at texture 1 and the
  full-size copy at texture 2; the full copy is a blit made only when a
  quad of the glass run asks for it (`wants_full_backdrop`) and is not
  allocated before. A quad without the marker renders as before.
- `src/metal_renderer.rs` (`surfaces_bgra_pipeline_state`, the choice of
  the pipeline in the surface draw, `set_vertex_bytes` moved ahead of it),
  `src/shaders.metal` (`surface_bgra_fragment`): a surface in BGRA is drawn
  as it is. The frames of the player are BGRA (mpv renders them on the
  GPU); the published crate takes only the two-plane YCbCr format of the
  camera and stops at an `assert_eq!` on any other. These lines are not
  marked `Bloom` in the source, and neither are those of the blur pass
  above (`new_backdrop_blur_filters`, `is_backdrop_blur_quad`,
  `BACKDROP_BLUR_*`): look for the names.
- `src/metal_renderer.rs` (`draw`), `src/present_trace.rs`,
  `src/gpui_apple.rs`: the time the display showed each draw, from the
  drawable's presented handler, with the GPU start and end times of its
  command buffer. Two consumers: the trace for `dev/jctl pacing` (off until
  `present_trace::enable`, the first `pacing` debug command) and a feedback
  function (`set_feedback`, the phase loop of Bloom's `pacing.rs`, set
  while a video plays). A record is handed out once both handlers ran, or
  at a timeout marked incomplete; a record handed out is never made again.
  With neither consumer on, `draw` records nothing and allocates nothing.

## gpui-pre-macos 0.3.4

- `src/display_link.rs`, `src/gpui_macos.rs`: `wake_frame_sources()`, a
  public function that runs the frame step of every window on the main
  queue at once, as a tick of the display link would. The frames task of
  the player draws a video frame at a set phase of the refresh period, not
  at the tick (`src/pacing.rs`).
- `Cargo.toml`: `[lints.rust] deprecated = "allow"`, as the published
  crate lost the workspace lints of Zed and a path dependency is not
  built with `--cap-lints` as a registry crate is.

## taffy 0.13.0

The layout engine of GPUI (crates.io, MIT), with one line changed.

- `src/compute/flexbox.rs` (`determine_flex_base_size`): `unwrap_or_else`
  for `unwrap_or` at `resolved_minimum_main_size`. The published line
  measures every flex item at its smallest width, a second layout of its
  subtree, and throws the result away when the style has a least size.
  Measured on 2026-10-07: 19% less `frame_cpu` on the server dashboard, 5
  to 9% on the settings and admin pages, the layout unchanged.

