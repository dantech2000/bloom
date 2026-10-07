# Vendored crates

`mpv/lib` is libmpv, one library built from pinned sources by
`dev/build-mpv` (the versions, sources and checksums are in that script;
`mpv/build` is its cache). `sparkle` is the updater framework. The two `gpui-pre-*` crates are copies of the published crates
(crates.io, version 0.3.4, Apache-2.0, license file carried) with small
local changes, wired in by `[patch.crates-io]` in `Cargo.toml`. An upgrade
of GPUI copies the new crate from the registry
(`~/.cargo/registry/src/*/gpui-pre-<name>-<version>/`: `Cargo.toml`,
`LICENSE-APACHE`, `src/`) and applies the changes below again. Each change
is marked `Bloom` in the source.

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
  for the quarter-size copy) and a linear-gradient background (angle: the
  blur share in the bezel; stop 0: the tint, its percentage the width of
  the specular rim; stop 1's colour: h saturation, s rim, l counter-light,
  its percentage the blur share in the flat middle). The
  quad fragment shader binds the quarter-size copy at texture 1 and the
  full-size copy at texture 2; the full copy is a blit made only when a
  quad of the glass run asks for it (`wants_full_backdrop`) and is not
  allocated before. A quad without the marker renders as before.
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
