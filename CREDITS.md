# Credits

Bloom stands on the work of other projects. This file names them, says what
Bloom uses each one for, and links back to it. The same list is in the app,
in Settings > About.

Bloom itself is free software under the
[GNU Affero General Public License v3.0 or later](LICENSE). It started as a
fork of [jellyui](https://github.com/iamd3vil/jellyui) by iamd3vil; the first
four commits of this repository are that project's.

Bloom is an independent client. It is not an official Jellyfin app, and
Jellyfin and its logo belong to the Jellyfin project.

## Built on

| Project | Used for | License |
| --- | --- | --- |
| [jellyui](https://github.com/iamd3vil/jellyui) | The project Bloom started from, by iamd3vil | AGPL-3.0-or-later |
| [Jellyfin](https://jellyfin.org) | The media server and its API | GPL-2.0 |
| [GPUI](https://www.gpui.rs) | The UI framework, from the Zed editor | Apache-2.0 |
| [gpui-kit](https://github.com/longbridge/gpui-kit) | Base components for GPUI | Apache-2.0 |
| [gpuicn](https://github.com/devaryakjha/gpuicn) | The components in src/ui | MIT |
| [mpv](https://mpv.io) | Video playback, embedded as libmpv | GPL-2.0-or-later |
| [FFmpeg](https://ffmpeg.org) | Decoding and demuxing inside mpv | GPL-3.0-or-later (as built here) |
| [libmpv2](https://github.com/kohsine/libmpv2-rs) | Rust bindings for libmpv | LGPL-2.1 |
| [Sparkle](https://sparkle-project.org) | Updates of the app in release builds | MIT |
| [Mozilla CA certificate list](https://curl.se/docs/caextract.html) | The certificate authorities the player trusts for HTTPS streams | MPL-2.0 |

## Inside libmpv

Linked into the one libmpv library that `dev/build-mpv` builds.

| Project | Used for | License |
| --- | --- | --- |
| [libass](https://github.com/libass/libass) | Subtitle rendering: ASS and SSA styling, SRT | ISC |
| [HarfBuzz](https://github.com/harfbuzz/harfbuzz) | Text shaping for subtitles | MIT |
| [FreeType](https://freetype.org) | Font rendering for subtitles | FTL or GPL-2.0-or-later |
| [FriBidi](https://github.com/fribidi/fribidi) | Right-to-left text in subtitles | LGPL-2.1-or-later |
| [dav1d](https://code.videolan.org/videolan/dav1d) | AV1 decoding in software | BSD-2-Clause |
| [libplacebo](https://code.videolan.org/videolan/libplacebo) | Colour helpers mpv is built on | LGPL-2.1-or-later |
| [Lua](https://www.lua.org) | mpv's scripts: the playback info panel and ytdl_hook | MIT |
| [Mbed TLS](https://github.com/Mbed-TLS/mbedtls) | TLS for HTTPS streams, inside FFmpeg | Apache-2.0 or GPL-2.0-or-later |
| [zvbi](https://github.com/zapping-vbi/zvbi) | Teletext subtitles of TV recordings, inside FFmpeg | LGPL-2.0-or-later |
| [uchardet](https://www.freedesktop.org/wiki/Software/uchardet/) | Character set detection for subtitle files | MPL-1.1, GPL-2.0-or-later or LGPL-2.1-or-later |

## Look, icons and fonts

| Project | Used for | License |
| --- | --- | --- |
| [Abyss theme](https://github.com/AumGupta/abyss-jellyfin) | The Jellyfin theme whose look Bloom follows | MIT |
| [Jellyfin logo](https://github.com/jellyfin/jellyfin-ux) | The mark in the top bar and on the sign-in page | CC BY-SA 4.0 |
| [Lucide](https://lucide.dev) | Line icons | ISC |
| [gpui-icons](https://github.com/devaryakjha/gpui-icons) | Lucide icons for GPUI | MIT and ISC |
| [Material Design Icons](https://github.com/google/material-design-icons) | Filled icons | Apache-2.0 |
| [Geist](https://github.com/vercel/geist-font) | Typeface | OFL-1.1 |
| [Google Sans](https://github.com/googlefonts/googlesans) | Typeface | OFL-1.1 |

## Server plugins Bloom works with

| Project | Used for | License |
| --- | --- | --- |
| [Jellyfin Enhanced](https://github.com/n00bcodr/Jellyfin-Enhanced) | Quality tags, bookmarks, hidden content, requests; Bloom follows its rules and storage | GPL-3.0 |
| [Intro Skipper](https://github.com/intro-skipper/intro-skipper) | The intro and credit segments Bloom can skip | GPL-3.0 |

## Studied for protocols and behaviour

| Project | Used for | License |
| --- | --- | --- |
| [jellyfin-web](https://github.com/jellyfin/jellyfin-web) | The web client: SyncPlay, playback negotiation, remote control | GPL-2.0 |
| [jellyfin-chromecast](https://github.com/jellyfin/jellyfin-chromecast) | The Cast receiver Bloom talks to | GPL-2.0 |
| [IINA](https://github.com/iina/iina) | How a libmpv player drives rendering from the display | GPL-3.0 |
| [pyatv](https://github.com/postlund/pyatv) | How AirPlay devices pair and play | MIT |

## Rust crates

| Project | Used for | License |
| --- | --- | --- |
| [rustls and ring](https://github.com/rustls/rustls) | TLS | Apache-2.0, ISC or MIT |
| [ureq](https://github.com/algesten/ureq) | HTTP client | MIT or Apache-2.0 |
| [tungstenite](https://github.com/snapview/tungstenite-rs) | WebSocket client | MIT or Apache-2.0 |
| [serde and serde_json](https://github.com/serde-rs/serde) | JSON | MIT or Apache-2.0 |
| [image and resvg](https://github.com/image-rs/image) | Image and SVG decoding | MIT or Apache-2.0 |
| [jiff](https://github.com/BurntSushi/jiff) | Dates and times | Unlicense or MIT |

## Everything else

Bloom links several hundred more Rust crates, almost all under MIT or
Apache-2.0, a few under BSD, ISC, Zlib, MPL-2.0 or Unicode licenses.
`Cargo.lock` names each one, and `cargo metadata` prints their licenses.

The app bundle made by `dev/bundle` also carries libmpv, one library with
FFmpeg and the projects of the group "Inside libmpv" linked in. mpv is
GPL-2.0-or-later and FFmpeg, as built, GPL-3.0-or-later (no GPL encoder is
in; mbedTLS asks for version 3), so that library as a whole is under the
GPL-3.0-or-later. `dev/build-mpv` names every source, version and checksum.

## Notes on the licenses

- **Fonts.** Geist and Google Sans are under the SIL Open Font License 1.1;
  the license texts are in `assets/fonts`.
- **Jellyfin logo.** CC BY-SA 4.0, from the Jellyfin UX repository. A changed
  version of the logo must stay under that license.
- **Studied projects.** Bloom reimplements protocols and behaviour it learned
  from the projects in that group. It does not contain their code.
