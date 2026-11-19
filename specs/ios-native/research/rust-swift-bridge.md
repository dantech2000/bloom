# Bridge from bloom-core to a SwiftUI iOS app (research, 2026-11-13)

Legend: **[repo]** = read in this repository today; **[web]** = checked on the
web today (source given); **[memory]** = from my knowledge, not checked today.
Verify every [memory] item with a spike before you rely on it.

## Summary

- **Recommendation: UniFFI 0.32.x** (proc-macro mode, no UDL), in a new
  crate `bloom-ffi` on top of `bloom-core`. Build it as a **dynamic
  XCFramework** (cdylib). A dylib keeps its Rust runtime private, so the clash
  with MPVKit's Libdovi on `rust_eh_personality` goes away without a prelink.
- bloom-core has **no async runtime**. It uses blocking `ureq` 3,
  blocking `tungstenite`, `std::thread` (28 spawns outside tests) and
  `async-channel` for events. So no `async_runtime = "tokio"` is needed. Export
  blocking calls as sync functions, and call them from Swift off the main
  actor. Where an `async` shape helps, wrap a thread plus a one-shot
  `async_channel` in Rust.
- Events: export a Rust **subscription object** with `async fn next() ->
  Option<Event>`, which reads the existing `async_channel::Receiver`. Swift
  runs one `Task` per stream and writes into an `@MainActor @Observable` model.
- Crux is a poor fit: it asks for a rewrite of the core into an Elm loop with
  I/O in the shell, and bloom-core does its I/O itself. BoltFFI is promising
  but young; keep it as the alternative to re-check in 2027.
- About 8 to 10k lines of non-UI logic sit in the app crate (`src/`) and are
  tied to `gpui_kit::Context` through `impl Bloom` methods. The SwiftUI app
  needs most of it. Move it into core behind small seams, first
  `syncplay/session.rs` (a `Player` trait) and `stream.rs` (playback info and
  track map).

## 1. Bridge options

### UniFFI (Mozilla) — recommended

- **Version:** 0.32.2 (2026-09-22); 0.32.0 on 2026-06-30, 0.31.0 on
  2026-01-14. [web: [CHANGELOG](https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md)]
- **Proc macros vs UDL:** proc macros (`uniffi::setup_scaffolding!()`,
  `#[uniffi::export]`, `#[derive(uniffi::Record | Enum | Object | Error)]`)
  are the current path; do not mix them with `include_scaffolding!`. [web:
  [proc_macro docs](https://mozilla.github.io/uniffi-rs/latest/proc_macro/index.html)]
- **Swift generation:** `uniffi-bindgen-swift` (since 0.28.2) works in
  library mode on the built library, with `--swift-sources`, `--headers`,
  `--modulemap`, `--modulemap-filename` and `--xcframework` (a modulemap fit
  for an XCFramework). [web: [uniffi-bindgen-swift](https://mozilla.github.io/uniffi-rs/latest/swift/uniffi-bindgen-swift.html)]
  `uniffi.toml` `[bindings.swift]` has `module_name`, `ffi_module_name`,
  `generate_immutable_records`, `generate_codable_conformance`,
  `link_frameworks`, `omit_checksums`. [web: [Swift configuration](https://mozilla.github.io/uniffi-rs/latest/swift/configuration.html)]
- **Swift 6 / Sendable:** "partial support". Most generated code conforms to
  `Sendable`. Generated classes are `@unchecked Sendable` (PR #2318, Jan 2025).
  Async glue code does not conform yet (issue #2448). A Swift type that
  implements a foreign trait must be `Sendable`. 0.31.2 fixed a strict
  concurrency warning in callback tables, 0.31.1 a leak in Swift async code.
  [web: [Swift overview](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html),
  changelog] Expect to compile the bindings target with Swift 5 language mode
  or `@preconcurrency import`, and the app target in Swift 6 mode.
- **Async:** a Rust `async fn` becomes a Swift `async` function. The foreign
  side polls the future; Rust needs no executor. There is no built-in
  cancellation: a Swift `Task` cancel does not stop the Rust future. Add a
  `cancel()` method where it matters (a download, a probe). [web:
  [futures](https://mozilla.github.io/uniffi-rs/latest/futures.html)]
- **`async_runtime = "tokio"`:** `#[uniffi::export(async_runtime = "tokio")]`
  with uniffi's `tokio` feature wraps the future in `async_compat`. Only
  `tokio` is accepted. 0.32.0 extends it to trait exports. Later internals
  docs say UniFFI does not want to depend on a Rust runtime, so its future is
  open. [web: changelog, [async overview 0.31](https://mozilla.github.io/uniffi-rs/0.31/internals/async-overview.html)]
  bloom-core does not need it (section 3).
- **Foreign traits:** `#[uniffi::export(with_foreign)] trait X: Send + Sync`
  lets Swift implement a Rust trait (a protocol in Swift). Async methods on
  them are possible. [memory for exact attribute; web for the feature]
- **Types:** records (structs), enums with data, objects (`Arc<Self>`),
  errors (`#[derive(uniffi::Error)]`, `flat_error` for string-only),
  `Option`, `Vec`, `HashMap`, `Vec<u8>` → `Data`, `Duration` →
  `TimeInterval`, `SystemTime` → `Date` (precision may be lost). [web:
  [builtin types](https://mozilla.github.io/uniffi-rs/latest/types/builtin_types.html)]
  `Url`/`Uuid`/`PathBuf` need `uniffi::custom_type!` to `String`. [memory]
- **Users in production:** Firefox iOS/Android, Matrix's `matrix-rust-sdk`
  (Element X iOS), Ferrostar, 1Password-style SDKs. [memory] The large user
  base is the main reason to choose it.

### swift-bridge

- 0.1.59, last publish about 2026-01-06; documents Swift 6 compatibility. [web:
  [docs.rs](https://docs.rs/crate/swift-bridge/latest)] Still 0.1.x after
  several years.
- Strengths: zero-copy strings and slices, direct Rust ↔ Swift without a
  serialization buffer, async functions. Weak points: a `#[swift_bridge::bridge]`
  module per surface that restates each type, smaller user base, Swift only.
  [memory]
- Not chosen: bloom-core's surface is wide (`Item` alone has ~50 fields and
  many nested types). UniFFI's derives on the existing types cost less.

### Crux (redbadger)

- crux_core 0.20.0 (2026-08-07). 0.19.0 (2026-06-08) moved binding generation
  from UniFFI to **BoltFFI** and dropped `crux_cli`; 0.17.0 (2026-03-20)
  removed the Capability API (Command API only). [web:
  [releases](https://github.com/redbadger/crux/releases)]
- Model: the core is a pure `update(Event, &mut Model) -> Command`; effects
  (HTTP, timers, storage) are requests that the shell (Swift) performs and
  answers; the shell renders a `ViewModel`.
- **Fit for Bloom: poor.**
  - bloom-core does its own I/O: `ureq` calls in `jellyfin.rs`, a WebSocket in
    `realtime.rs`, a TLS Cast v2 session with its own threads in
    `chromecast/session.rs`, a downloads engine with worker threads. Crux would
    move all of this into Swift (URLSession, Network.framework) and so
    duplicate it for the Mac, or force the Mac app onto Crux as well.
  - The Mac app is GPUI with `Entity`/`Context`; a second state architecture
    beside it doubles the work.
  - Parts of core are already Elm-like (`syncplay::Core::handle(input, now,
    clock, view) -> Vec<Action>`, `connection::Connection::handle(event, now)
    -> Action`). They export well through UniFFI as they are. You get the
    useful half of Crux without the rewrite.

### BoltFFI

- MIT; docs.rs shows 0.26.1 while the README mentions 0.32.1 (two numbers in
  the sources; check crates.io). [web: [docs.rs](https://docs.rs/crate/boltffi/latest/source/README.md),
  [boltffi.dev](https://boltffi.dev)]
- `#[data]` on records, `#[export]` on functions, impls and traits;
  closures (`impl Fn`), callback traits, async functions, "async streams";
  `Vec<u8>` → `Data`, `Duration` → `TimeInterval`. `boltffi pack apple` makes
  an XCFramework plus `Package.swift`. Claims up to 1000x faster calls than
  UniFFI (own benchmark). [web]
- Called "early stage" in a Feb 2026 Show HN. Its docs do not say how a stream
  looks in Swift, nor anything on `Sendable`. [web]
- Crux's move to it is a signal. Bloom's calls are network-bound; call
  overhead does not matter here. **Re-evaluate in 2027**; for now UniFFI is
  the safer choice.

### Events into Swift Observation

Two working patterns with UniFFI:

1. **Pull (recommended):** a Rust object per stream.
   ```rust
   #[derive(uniffi::Object)]
   pub struct SyncPlayEvents(async_channel::Receiver<core::Ui>);
   #[uniffi::export]
   impl SyncPlayEvents {
       pub async fn next(&self) -> Option<SyncPlayEvent> { self.0.recv().await.ok().map(Into::into) }
   }
   ```
   ```swift
   @MainActor @Observable final class SyncPlayModel {
       var state: SyncPlayState = .idle
       func run(_ events: SyncPlayEvents) async {
           while let e = await events.next() { apply(e) }  // resumes on the main actor
       }
   }
   ```
   `async_channel` futures need no executor, so the foreign polling of UniFFI
   wakes them. [memory, reasoned; test in the spike] The existing receivers
   fit directly: `realtime::start` → `Receiver<SocketEvent>`,
   `chromecast::session::Session::connect` → `Receiver<Event>`,
   `downloads::Engine::changes()` → `Receiver<()>` (a "changed" tick; Swift
   then reads `entries()`/`summary()`). Wrap the loop in an `AsyncStream` if
   the views want `for await`.
2. **Push:** a `with_foreign` trait `EventSink { fn on_event(&self, e) }`
   that Swift implements and that yields into an
   `AsyncStream.Continuation`. The call arrives on a Rust thread; the
   implementation must be `Sendable` and must hop to the main actor. Use it
   where Rust has no channel (callbacks inside the downloads engine).

Rules for both: send small enums of plain data, not `serde_json::Value`; coalesce
progress (the engine's bounded(1) channel already does); end the stream by
dropping the Rust owner, so `next()` returns `nil`.

## 2. Packaging

- **Targets:** `aarch64-apple-ios` (device) and `aarch64-apple-ios-sim`
  (Apple Silicon simulator). `x86_64-apple-ios` only for Intel Macs; skip it.
  `rust-toolchain.toml` lists no targets; add `targets = ["aarch64-apple-ios",
  "aarch64-apple-ios-sim"]` there. [repo] Keep `IPHONEOS_DEPLOYMENT_TARGET=17.0`
  as `dev/ios` does. [repo]
- **XCFramework:** `xcodebuild -create-xcframework -library <device lib>
  -headers <dir> -library <sim lib> -headers <dir>` (static) or
  `-framework` (dynamic). One slice per platform; a fat sim slice needs
  `lipo` first. [memory]
- **The Libdovi clash ([repo] `dev/ios` lines 52-67):** today a static archive
  is prelinked with `ld -r -exported_symbols_list ios/host/exports.txt`.
  Choices:
  1. **Dynamic framework (recommended).** Build `bloom-ffi` as
     `crate-type = ["cdylib", "staticlib"]`; ship the dylib in a
     `.framework` (install name `@rpath/BloomCore.framework/BloomCore`).
     A Rust cdylib exports only `#[no_mangle]` C symbols, so its
     `rust_eh_personality` is local and cannot collide with Libdovi's.
     [memory, reasoned] Embedded frameworks are allowed on the App Store and
     cost a few ms of launch. No export list to keep.
  2. **Static with prelink**, as today. The symbols to export are UniFFI's C
     ABI, all with known prefixes: `_uniffi_bloom_ffi_fn_*` (functions,
     methods, constructors, `free`/`clone`), `_uniffi_bloom_ffi_checksum_*`,
     `_ffi_bloom_ffi_rustbuffer_{alloc,from_bytes,free,reserve}`,
     `_ffi_bloom_ffi_rust_future_{poll,cancel,complete,free}_*`,
     `_ffi_bloom_ffi_uniffi_contract_version`, plus the
     `uniffi_*_fn_init_callback_vtable_*` of foreign traits. [memory for the
     exact names] ld64's `-exported_symbols_list` accepts wildcards (`*`,
     `?`), so the list is two lines: `_uniffi_bloom_ffi_*` and
     `_ffi_bloom_ffi_*`. [memory] The `-u` roots can come from the generated
     header (`grep -o 'uniffi_[a-z0-9_]*\|ffi_[a-z0-9_]*'`). Check with
     `nm -gU bloom_ffi.o | grep -v ' _\(uniffi\|ffi\)_'` → empty.
- **Size:** the release dylib with `ureq` + `rustls`/`ring` + `tungstenite` +
  `serde_json` + `image` + `resvg` will be several MB. Cut: put
  `core::images` (`image`, `resvg`) behind a feature, since SwiftUI decodes
  images natively; build with `lto = "fat"`, `codegen-units = 1`,
  `opt-level = "s"`, `strip = true`. Keep `panic = "unwind"`: UniFFI catches
  a panic at the boundary and turns it into a Swift error, and `abort` would
  end the app instead. Measure with `cargo bloat` in the spike. Bitcode is gone since
  Xcode 14; nothing to do.
- **Swift Package:** a local package `BloomCore` with
  `.binaryTarget(name: "BloomCoreFFI", path: "BloomCoreFFI.xcframework")` and a
  source target `BloomCore` holding the generated `bloom_ffi.swift`
  (depends on the binary target). The app (xcodegen, as now) depends on the
  local package. Set the source target to Swift 5 language mode until #2448 is
  fixed.
- **SPM build plugin vs script:** use a **script** (`dev/ios-core`): cargo for
  two targets → `uniffi-bindgen-swift` → `xcodebuild -create-xcframework`.
  SPM plugins run sandboxed, cannot call rustup/cargo with network or write
  to `target/` freely, and would hide cargo errors. Tools that do the script's
  work: `cargo-swift` 0.11.0 (version must match UniFFI; check which uniffi it
  targets) [web: [docs.rs](https://docs.rs/crate/cargo-swift/latest)], the
  `swift-package` crate (uniffi_bindgen 0.29) [web]. A 40-line script is
  easier to keep in step with UniFFI 0.32.

## 3. Async runtime in bloom-core [repo]

- `core/Cargo.toml`: `ureq = "3"` (blocking), `tungstenite = "0.29"`
  (blocking WebSocket, rustls), `rustls 0.23` with `ring`, `async-channel 2`,
  `jiff 0.2`, `image`, `resvg`. **No tokio, no reqwest, no futures executor.**
- `grep`: 28 `thread::spawn`/`std::thread` uses outside tests; one
  `async fn` (`images.rs:159`, a permit queue on `async_channel`). Threads
  run in `realtime.rs` (socket), `chromecast/session.rs` (session loop,
  `mpsc` for commands, `async_channel` for events), `downloads/engine`
  (workers), `dnssd.rs`.
- Consequences for the bridge:
  - Export the `Client` request methods as **sync** UniFFI functions. A sync
    call blocks the calling thread; Swift must not call it from the main
    actor. Put the calls in a non-isolated `actor` or `nonisolated async`
    wrapper in Swift, or
  - make thin Rust `async fn` wrappers: spawn a thread (or a small pool),
    run the blocking call, send the result on `async_channel::bounded(1)`,
    `await` the receiver. No tokio, and Swift sees `async throws`. A pool
    caps thread count when a grid asks for 40 items.
  - Do **not** add tokio for the bridge alone; it would add a second I/O model
    beside `ureq`.
- Clock: `syncplay::Core::handle(input, now: f64, ...)` takes a float clock,
  and `connection::Connection::handle(event, now: Instant)` takes an
  `Instant`. An `Instant` cannot cross the FFI; give the wrapper a
  monotonic `f64` or read `Instant::now()` on the Rust side. [repo]

## 4. bloom-core public surface [repo]

Modules (`core/src/lib.rs`), with size and how easy each is to export:

| Module | Lines | Main pub items | Export |
|---|---|---|---|
| `jellyfin` (+ `server.rs`) | 2384 + 357 | `Client` (~140 pub fns: auth, views, items, resume, next_up, seasons, episodes, reports, quick connect, Seerr), `Item`, `ItemQuery`, `MediaStream`, `UserData`, `ItemKind`, `PersonKind`, `StreamKind` | Easy for data (serde structs, enums with an unknown-value variant); `Client` as an `Object`. Generic `get<T>`/`post<B>`/`send<T>` and `agent() -> &ureq::Agent` stay Rust-only |
| `pages` | 659 | `load_home`, `load_detail`, `load_search`, `library_query`, `HomeData`, `LibraryData`, `DetailData` | Easy; good first surface |
| `config` | 556 | `Config`, `Server`, `Profile`, `Token`, `load/save`, `normalize_url` | Easy; `Config::path()` uses `dirs`; on iOS pass the path in. `Token` must stay opaque (never cross as text) |
| `connection` | 561 | `Connection` state machine, `classify`, `probe`, `backoff` | Medium: `Instant`, `&anyhow::Error`; a global offline flag |
| `syncplay` (`core`, `protocol`, `clock`, `drift`) | ~3000 | `Core::handle -> Vec<Action>`, `Input`, `Action`, `Notice`, `PlayerView` | Medium: pure, but many enums; wrap in an `Object` with a `Mutex<Core>` |
| `chromecast` | ~3700 | `Session::connect -> (Session, Receiver<Event>)`, `Device`, `MediaStatus`, discovery | Medium: channel → subscription object; `request()` returns `std::mpsc::Receiver<Value>` → make it `async` |
| `cast` | ~590 | protocol of Jellyfin remote sessions | Easy |
| `downloads` | ~4100 | `Engine::open`, `add`, `cancel/resume/remove` (generic `impl Into<Key<'a>>`), `entries`, `summary`, `changes()` | Medium: lifetimes and generics need concrete `String` wrappers; `PathBuf` → `String` |
| `realtime` | 263 | `start(client) -> (Realtime, Receiver<SocketEvent>)` | Medium: subscription object |
| `images` | 1651 | fetch, decode, LRU, permits | Skip on iOS (native decode) |
| `bitrate`, `playback`, `quality_tags`, `subtitles`, `languages`, `trailers`, `queue`, `search_index`, `news`, `events`, `draft`, `options`, `dnssd`, `fsutil`, `brand` | small | pure functions and tables | Mostly easy; `quality_tags::Input<'a>`, `subtitles::new_stream<'a>`, `queue::replacement<'a>` return borrows → return clones or indices |
| `test_time` | 44 | test helper | Not exported |

Types that need a wrapper or a custom type:

- `Arc<str>` fields of `Client` (`base`, `token`, ...) → keep `Client`
  as an object; no field access from Swift.
- `Arc<Mutex<..>>`, `MutexGuard`, `std::sync::mpsc` → hide inside objects.
- `async_channel::Receiver<T>` → subscription object (section 1).
- `Instant`/`Duration`: `Duration` crosses as `TimeInterval`; `Instant` does
  not cross.
- `jiff` timestamps (`syncplay/protocol.rs`, `events/hub.rs`) →
  `SystemTime` or an `i64` of ms through `custom_type!`.
- `PathBuf` → `String` custom type. `uuid::Uuid` → `String`.
- `serde_json::Value` (admin editors, Cast `request`) → pass JSON text, or
  leave the admin dashboard out of iOS v1.
- `anyhow::Error` → one `#[derive(uniffi::Error)] enum BloomError { Network
  { why: Why }, Http { status: u16 }, Auth, NotFound, Other { message } }`
  built from `connection::classify`, so Swift can tell offline from 401.

How to put the derives in: `#[cfg_attr(feature = "ffi", derive(uniffi::Record))]`
on core's plain data types, with an `ffi` feature that only `bloom-ffi`
turns on. The Mac build then does not compile uniffi, and core keeps one
definition of `Item`. UniFFI also has "remote" types for types of another
crate, which restate the fields; with ~50 fields in `Item` the `cfg_attr`
route is shorter. [memory for remote types]

## 5. Non-UI logic in the app crate (`src/`) [repo]

Line counts from `wc -l`; "gpui refs" counts lines that name `gpui`,
`Context<`, `Entity<`, `SharedString`, `cx.` or `Window`.

| File | Lines | gpui refs | Why move it / seam needed |
|---|---|---|---|
| `syncplay/session.rs` | 1095 | 0 | Runs `syncplay::Core` against the player and server. Depends on `crate::player::Player` (status, schedule, set_sync_speed, follow, seek_exact, set_paused, stop: ~12 methods). **Seam:** a `SyncPlayer` trait in core; Swift implements it over MPVKit as a foreign trait. Highest value |
| `stream.rs` | 1357 | 19 | PlaybackInfo request, quality choice, server track ↔ mpv id map, transcode end. Pure parts plus `impl Bloom` menu builders with `Context`. Split: logic to core, menus stay |
| `cast/target.rs` | 456 | 0 | The one playback target as a state machine. Moves as is |
| `cast/remote.rs` | 648 | 1 | `RemoteTarget` trait over three backends; uses `gpui_kit::Task`. Seam: return a handle or make the methods blocking |
| `cast/mod.rs` | 1246 | 72 | Cast panel actions; logic tangled with `cx`. Extract the session handling only |
| `airplay/mod.rs` | 1333 | 0 | AVPlayer over objc on the Mac. On iOS SwiftUI does AirPlay natively (AVRoutePickerView); do not move, rewrite in Swift (small) |
| `queue.rs` | 413 | 17 | Up next, followers, `refind_lost_episode`, timeline. Methods on an entity with `Context<Self>` and a `RenderImage`. Extract the decisions (what plays next, refind) as pure functions |
| `lists.rs` | 1170 | 50 | Playlist/collection calls are an `easy_ext` block on `Client` → move that block to core; dialog state stays |
| `hidden.rs` | 1115 | 24 | Jellyfin Enhanced hidden content (file format, filter). Model + `easy_ext` → core |
| `bookmarks.rs` | 1049 | 44 | Jellyfin Enhanced bookmarks; same split as `hidden.rs` |
| `enhanced.rs` | 1458 | 37 | Plugin settings, auto pause/skip rules, Seerr links. Rules → core; keyboard shortcuts stay |
| `subtitles.rs` | 929 | 38 | Subtitle choice and style; the file fetch is in `player/subtitles.rs`. Choice logic → core |
| `adaptive.rs` | 609 | 8 | Auto quality: bitrate test, stall step-down. Nearly pure → core |
| `seasons.rs` | 797 | 27 | Season/episode loading and the dialog. Loader → core |
| `metadata/mod.rs`, `tabs.rs` | 2760 | 144 | Metadata editor; heavily UI. Leave out of iOS v1 |
| `downloads/mod.rs` | 517 | 22 | App glue around `core::downloads::Engine` (offline mode, the session). Glue → core `DownloadsService` |
| `socket.rs` | 164 | 10 | Dispatch of server socket messages and the "server news" debounce. Dispatch → core as a function `SocketEvent -> Vec<AppEffect>` |
| `connection.rs` | 267 | 18 | Drives `core::connection` with timers in `cx`. Timer loop → core thread or Swift `Task` |
| `app/sessions.rs` | 437 | 36 | Sign-in, switch, sign-out, `session_epoch` guards. The epoch logic is the important part: a Swift app needs the same guard (drop answers of an old session). Move a `Session` object to core |
| `nowplaying.rs` | 813 | 11 | MPNowPlayingInfoCenter / media keys; platform. Rewrite in Swift |
| `playback.rs`, `credits.rs`, `quality_tags.rs` | 67, 191, 48 | few | Small helpers; move with their callers |
| `admin/*` | ~9000 | high | Server dashboard; `serde_json::Value` editors. Leave for later |

The common shape: logic lives in `impl Bloom` methods that take
`&mut Context<Self>`, spawn with `cx.background_spawn`, and call
`cx.notify()`. The seam for each is the same: a core function or object that
takes plain inputs and returns data or `Vec<Effect>`; the GPUI method and the
Swift model both call it and apply the result. Order by value for iOS v1:
`app/sessions.rs` → `stream.rs` + `adaptive.rs` → `queue.rs` →
`syncplay/session.rs` → `cast/target.rs` + `cast/remote.rs` → `lists`,
`hidden`, `bookmarks`, `enhanced`. About 8 to 10k lines; each move also
helps the Mac (the logic gets tests without the player).

Note on the player: on iOS today the mpv worker of `src/player/` runs in
Rust over MPVKit's libmpv. With SwiftUI, two options: (a) Swift drives
MPVKit's libmpv C API itself and implements the `SyncPlayer` trait, or (b)
the Rust worker moves to a crate the FFI exports. (a) keeps the FFI small;
(b) shares the worker's hard-won rules (no network on the worker thread,
subtitle fetch threads, external sid map) with the Mac. Decide this before
the SyncPlay seam.

## 6. Testing across the boundary

- **Rust tests stay** (`cargo test --release -p bloom-core`, 192 tests per
  CLAUDE.md). Add tests in `bloom-ffi` for the conversions (`Item` →
  record, error mapping).
- **Swift tests on the bindings:** a Swift package test target
  (`swift test` cannot run iOS; use `xcodebuild test -destination 'platform=iOS
  Simulator,...'`), or build a macOS slice of the XCFramework too and run
  `swift test` on the Mac for speed. Test: the call round trip, an error
  case, an event stream that ends, cancel. UniFFI objects are protocols in
  Swift, so view models can take a fake in their tests.
- **Mock server [repo]:** `dev/mock-jellyfin` is a 368-line Python 3
  `ThreadingHTTPServer` with a fixed library (movies, two shows, a
  collection), drawn pictures, auth (`/Users/AuthenticateByName` → token
  `mock-token`, user `tester`), played/favourite toggles, similar items,
  branding, quick connect off, media segments empty. It has no media files
  (a play fails) and answers `/socket` with 404 (no WebSocket, so no SyncPlay
  or realtime). Unknown requests get an empty list and a stderr line.
- **Can iOS UI tests use it?** Yes, for browse, detail, search and sign-in:
  the simulator shares the Mac's network, so the app reaches
  `http://127.0.0.1:<port>`. Start it with `--port-file`, pass the URL to the
  app by a launch argument or environment (`XCUIApplication().launchEnvironment`),
  and allow plain HTTP for localhost in the test build's ATS
  (`NSAllowsLocalNetworking`). [memory for ATS key] A physical device needs
  the Mac's LAN address. Gaps: playback, SyncPlay and the socket need
  `dev/jellyfin-local` (the container) or the live server under the safety
  rules of CLAUDE.md. Extending the mock with a WebSocket for `/socket` would
  let Swift test the realtime stream.

## Spike plan (one to two days)

1. Create `bloom-ffi` with `uniffi = "0.32"`; export `Client::new`,
   `public_info`, `authenticate`, `pages::load_home` (sync), and one
   `async_channel` subscription (`realtime::start`).
2. Script: build both targets, `uniffi-bindgen-swift`, dynamic XCFramework,
   local Swift package; link it into the existing xcodegen project **with
   MPVKit present**, to prove the Libdovi clash is gone.
3. A SwiftUI screen against `dev/mock-jellyfin` with an `@Observable` model;
   check Swift 6 mode of the app target and the dylib size.
4. Decide on the player (option a or b in section 5).

## Sources

- UniFFI changelog: https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md
- UniFFI Swift overview: https://mozilla.github.io/uniffi-rs/latest/swift/overview.html
- UniFFI Swift configuration: https://mozilla.github.io/uniffi-rs/latest/swift/configuration.html
- uniffi-bindgen-swift: https://mozilla.github.io/uniffi-rs/latest/swift/uniffi-bindgen-swift.html
- UniFFI futures: https://mozilla.github.io/uniffi-rs/latest/futures.html
- UniFFI async overview 0.31: https://mozilla.github.io/uniffi-rs/0.31/internals/async-overview.html
- UniFFI builtin types: https://mozilla.github.io/uniffi-rs/latest/types/builtin_types.html
- UniFFI PR #2318 (Sendable classes): https://github.com/mozilla/uniffi-rs/pull/2318
- Crux releases: https://github.com/redbadger/crux/releases
- BoltFFI: https://boltffi.dev, https://docs.rs/crate/boltffi/latest/source/README.md, https://boltffi.dev/docs/types.md
- swift-bridge: https://docs.rs/crate/swift-bridge/latest
- cargo-swift: https://docs.rs/crate/cargo-swift/latest
