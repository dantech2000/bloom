## Install

1. Download `Bloom-{{VERSION}}.zip`, unzip it, and move `Bloom.app` to Applications.
2. The app is not signed with an Apple Developer ID. At the first start macOS blocks it: open System Settings > Privacy & Security and choose "Open Anyway", or run `xattr -dr com.apple.quarantine /Applications/Bloom.app`.

Apple Silicon only, macOS 14 or later.

## Bundled libraries

`Bloom.app` carries one library, libmpv, built by `dev/build-mpv` from pinned sources: mpv and FFmpeg with libass, libplacebo, dav1d, Lua, Mbed TLS and a few more linked in (GPL-3.0-or-later as built; no encoder is in it). It also carries the Sparkle update framework (MIT) and Mozilla's list of certificate authorities (MPL-2.0). Bloom itself is under AGPL-3.0-or-later; its source is this repository at the tag of the release. `CREDITS.md` names every project with its license and a link to its source.
