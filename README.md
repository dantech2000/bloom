# Bloom

A native [Jellyfin](https://jellyfin.org) client for macOS, written in Rust. The
interface is drawn with [GPUI](https://www.gpui.rs/) and video plays inside the
window through an embedded [libmpv](https://mpv.io).

Bloom started as a fork of [jellyui](https://github.com/iamd3vil/jellyui) by
iamd3vil. The first four commits of this repository are that project's.

## What it does

- **Browse.** Home with Continue Watching, Next Up and recent items, libraries
  with sort and filter, series, seasons and episodes, collections, playlists,
  and a search with a local title index.
- **Play.** Direct play of the original file by default. Resume, chapters,
  preview images on the seek bar, intro and credit skip, audio and subtitle
  choice with a timing offset, playback speed, picture in picture, and frame
  pacing locked to the display.
- **Quality.** A Quality menu with a bitrate limit and server transcoding when
  you ask for it. Nothing lowers the quality unless you turn it on.
- **Watch together.** SyncPlay groups with other Jellyfin clients, with drift
  correction.
- **Play on another device.** Other Jellyfin sessions, Chromecast and AirPlay.
  Other clients can also control Bloom.
- **Offline.** Downloads with resume, and playback of downloaded files without
  the server.
- **Server dashboard** for administrators: users and their access, libraries,
  devices, activity, scheduled tasks, plugins and their settings, logs, API
  keys, and the server settings pages.
- **Your account.** Profile picture, password, Quick Connect, playback,
  subtitle, home and display settings.
- **Jellyfin Enhanced** features when the server has that plugin: quality tags,
  bookmarks, hidden content, requests through Jellyseerr.
- **macOS.** Media keys and the Now Playing tile, no display sleep during
  playback, and a layered app icon that follows the system's icon style.

## Requirements

| Dependency | Notes |
| --- | --- |
| macOS | The app has only been built and run on macOS |
| Rust 1.97.1 | Pinned in `rust-toolchain.toml`; `rustup` installs it on the first build |
| Xcode | For the Metal toolchain, and `actool` for the app icon |
| mpv | libmpv and its libraries are copied into `vendor/mpv` by `dev/vendor-mpv` |
| ffmpeg (optional) | The playback tests encode short clips with it |
| [just](https://github.com/casey/just) (optional) | Runs the recipes in `justfile` |

## Build and run

```sh
dev/vendor-mpv          # once: copies libmpv into vendor/mpv
cargo build --release
dev/run                 # starts the app with the debug channel on
```

At the first start, enter the address of your server and sign in, or use Quick
Connect. To get `Bloom.app`, run `dev/bundle`; `dev/install` builds it and
copies it to `/Applications`.

Servers, profiles, access tokens and downloads are in
`~/Library/Application Support/bloom`. Cached images are in
`~/Library/Caches/bloom`.

## Development

| Command | Purpose |
| --- | --- |
| `dev/test` | All tests |
| `dev/smoke` | Opens every page in a test instance and checks the frame cost |
| `dev/run-test` | A second, muted instance for tests, labelled "TEST INSTANCE" |
| `dev/jctl <command>` | Drives an instance through its debug socket (`dev/jctl state`) |
| `dev/shot <file>` | Captures the window of an instance |
| `dev/jctl perf`, `dev/jctl pacing` | Frame cost, and how evenly video frames reach the screen |

`CLAUDE.md` has the working rules of this repository, among them the rules for
tests against a real server.

### Layout

| Path | Purpose |
| --- | --- |
| `src/app.rs` | The root view: session, navigation, player state |
| `src/views/` | Screens: sign-in, shell, home, library, detail, player |
| `src/player.rs`, `src/pacing.rs`, `src/video_surface.rs` | The libmpv worker, the render thread and display sync |
| `src/stream.rs`, `src/adaptive.rs` | Playback negotiation, quality, transcoding |
| `src/syncplay/` | SyncPlay: protocol, clock, drift control, session |
| `src/cast/`, `src/chromecast/`, `src/airplay/` | Playing on other devices |
| `src/downloads/` | Offline downloads |
| `src/admin/`, `src/settings/`, `src/metadata/` | Dashboard, user settings, metadata editor |
| `src/jellyfin.rs`, `src/realtime.rs` | REST client and the server socket |
| `src/ui/` | Components from the gpuicn registry, as editable source |
| `src/brand.rs` | The name of the app, in one place |
| `src/debug.rs` | The debug channel |
| `dev/` | Scripts |
| `vendor/gpui-pre-apple` | GPUI's macOS backend with two patches (backdrop blur, BGRA surfaces) |

## License

Bloom is free software under the
[GNU Affero General Public License v3.0 or later](LICENSE), as is jellyui.

Third-party parts keep their own licenses: the fonts in `assets/fonts` are under
the SIL Open Font License (`assets/fonts/OFL.txt`), the gpuicn components in
`src/ui` are MIT licensed, the Jellyfin logo is CC BY-SA 4.0, the filled icons
are from Material Icons (Apache-2.0), and libmpv is LGPL-2.1+ and linked
dynamically.
