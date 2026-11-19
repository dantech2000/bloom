# Bloom for iOS, native: the spec

Status: draft 3, 2026-11-13, for Daniel's review. Draft 3: the app is for
Daniel's own devices, not the App Store (§2), and the Mac app is not
touched (§1). Draft 1 was reviewed by a
second model (Fable); its review is [review.md](review.md), and §15 says
what changed because of it.

| File | What it holds |
|---|---|
| README.md (this file) | What we build and the decisions behind it |
| [plan.md](plan.md) | The phases, their order, what each delivers and how each is checked |
| [inventory.md](inventory.md) | Every screen, menu, player feature and rule of today's app |
| [research/](research/) | Sourced notes: Apple docs and WWDC25/26 sessions, media APIs, UniFFI, Swiftfin |
| [review.md](review.md) | The review of draft 1 |

Claims from outside the repo carry their source in the research notes,
marked [V] (read in the source) or [M] (from memory). An [M] fact is
checked in a spike before code relies on it.

---

## 1. Goal

A Bloom for iPhone and iPad that is an iOS app in every way a user can
tell: system navigation, system text fields, system sheets and menus,
Liquid Glass from the system, Dynamic Type, VoiceOver, Picture in Picture,
AirPlay, the Lock Screen, background downloads, and an iPad that behaves
like an iPad. The look stays Bloom's: dark, one ink, colour only for
state, the hero, the artwork carrying the colour.

PRODUCT.md: "A Mac app on the Mac, a phone app on the phone; shared code is
an implementation detail the user never sees." DESIGN.md: "Don't reinvent
a native control on the phone for flavour; the glass is the brand, the
structure is the platform's." Daniel, 2026-10-21: "the menus and the
navigation should feel native on each platform."

### What changes

- The iOS interface is rewritten in Swift: SwiftUI, with UIKit where it is
  the better tool (§3).
- The Rust core (`bloom-core`) is reused through generated Swift bindings
  (§5).
- The GPUI iOS port is frozen when the native work starts (crash fixes
  only) and its iOS build is removed when Daniel's phone switches
  (milestone M1, plan).

### The Mac app is not touched

This work is the iOS app only. Daniel, 2026-11-13: "the desktop isn't
being touched this is just the IOS app". So:

- **No file of the Mac app changes**: `src/`, `vendor/gpui-pre-apple`,
  `vendor/gpui-pre-macos` and the other Mac crates, the Mac's dev scripts.
- **Core changes are additive**: new modules, or code behind the `ffi`
  feature or `cfg(target_os = "ios")`. Nothing the Mac build compiles
  changes behaviour. Existing core functions the Mac calls keep their
  signatures and behaviour.
- Logic the iOS app needs that lives in `src/` today is **copied** into
  new core modules (or written in Swift), not moved. The Mac keeps its
  own copy; whether the Mac ever adopts the core copies is a separate,
  later decision.
- Every step checks it: the Mac build is unchanged (`cargo build --release
  -p bloom` with no new warnings), and `dev/test` and `dev/lint` stay
  green.

### Non-goals

- No change to the Mac app (above).
- No App Store or public distribution (§2).
- No music, books or Live TV (the app is video only, inventory §0).
- No tvOS or visionOS in this plan; the structure leaves room for them.
- No feature the Mac lacks before M1, except what iOS needs to be a proper
  iOS app (PiP, Keychain, background downloads, Now Playing, Dynamic
  Type, haptics, the life-cycle rules of §8.10).

---

## 2. Distribution: Daniel's own devices (settled)

The app is for Daniel's personal use on his own devices, installed with
development signing (team T46QDHL88N), as the GPUI build is today. It is
not distributed through the App Store or TestFlight. So the licence
question (Bloom is AGPL-3.0, which conflicts with the store's terms) does
not arise, and the store's requirements (privacy manifest, review notes,
a demo server, export compliance, a unique store name, a privacy policy)
do not apply. If public distribution ever comes up, the licence is the
first question again: the jellyui commits (Sarat Chandra) and the GPL
sources credited as "studied" in `src/credits.rs`.

---

## 3. SwiftUI, UIKit, or both

**Both, SwiftUI first.** Apple (WWDC26 session 272): start new scenes and
components in SwiftUI, mix in UIKit where it is the better tool; Apple's
own controls and Liquid Glass are built on SwiftUI now.

| Part | Framework | Why |
|---|---|---|
| App, scenes, tabs, navigation, pages, sheets, menus, forms, settings, dashboard | SwiftUI | System structure and Liquid Glass; Observation drives it |
| Video surface (mpv's sample-buffer layer; AVPlayerLayer for AirPlay) | UIKit view in `UIViewRepresentable` | No SwiftUI surface for a custom renderer |
| Picture in Picture | `AVPictureInPictureController` on that layer | UIKit-level API |
| AirPlay picker | `AVRoutePickerView` in `UIViewRepresentable` | No SwiftUI view (Apple's sample wraps it the same way) |
| Gestures on the video | `UIGestureRecognizerRepresentable` | Simultaneous recognizers, failure requirements |
| Poster grids and rows | SwiftUI lazy stacks | WWDC26 session 321 endorses nested `LazyHStack` rows; `UICollectionView` + `UIHostingConfiguration` only if Instruments shows a bottleneck |
| Profile picture, branding image | `PhotosPicker`, `fileImporter` | System pickers, no permission prompt |
| Trailers | `SFSafariViewController` | As today |

A UIKit piece is a leaf. The tree, the navigation and the state are
SwiftUI and `@Observable`; UIKit views read the same models.

---

## 4. Platform targets

- **Minimum iOS 27.** Daniel's iPhone Air runs iOS 27.3 (checked with
  `devicectl`, 2026-11-13), and the app is for his devices only. So the
  app uses the iOS 27 APIs directly, with no `#available` forks: the Now
  Playing framework, `AsyncImage`'s URL session where it fits,
  `TabRole.prominent`, toolbar priority, `reorderable()`, `swipeActions`
  outside `List`, `MetricManager`.
- **Built with the iOS 27 SDK** (Xcode 27; scene life cycle, which a
  SwiftUI `App` has).
- **iPhone first, iPad in the same target.** Every phase from 3 on checks
  one pass on the iPad simulator and one resized window; the iPad's own
  design is phase 8.
- **Swift 6 language mode** for the app, with Approachable Concurrency and
  Default Actor Isolation = MainActor. The generated bindings compile in
  Swift 5 mode until UniFFI's async glue is `Sendable` (issue #2448).
- **Resizable from day one.** iOS 27 resizes iPhone apps (iPhone
  Mirroring, an iPhone app on an iPad); Apple: the idiom and the
  orientation are "no longer meaningful for any kind of layout decision".
  Layout reads size classes and the view's size, never the idiom or
  `UIScreen.main`. No `UIRequiresFullScreen`.
- `UIApplicationSupportsMultipleScenes = NO` for v1 (two windows would mean
  two players on one audio session).
- `UIUserInterfaceStyle = Dark` in Info.plist (launch screen, system
  alerts, Safari and share sheets). Widgets render in the system's modes;
  "dark only" does not hold there.
- **Bundle ids.** `dev.bloom.ios.next` for the native app until M1, with
  its own container, config and device id, so the GPUI app on Daniel's
  phone keeps working beside it. At M1 the native app takes
  `dev.bloom.ios` and migrates (§9). UI tests use a third id,
  `dev.bloom.ios.uitest` (§10).
- The Rust build for iOS uses `IPHONEOS_DEPLOYMENT_TARGET=27.0`.

---

## 5. Architecture

```
 SwiftUI views ── @Observable models (MainActor) ── Swift services
                                                       │
                    BloomCore Swift package (generated bindings)
                                                       │
                    bloom-ffi (Rust, UniFFI) ── bloom-core (Rust, shared with the Mac)
```

### 5.1 The bridge

- **UniFFI 0.32** (MPL-2.0), proc-macro mode, crate `ffi/` (`bloom-ffi`).
  Derives on core's types behind a feature
  (`#[cfg_attr(feature = "ffi", derive(uniffi::Record))]`); the Mac build
  never compiles UniFFI (CI checks `cargo tree -p bloom`).
- **Packaging**: a dynamic XCFramework holding a real `.framework`
  (Info.plist, `MinimumOSVersion`, signed; iOS loads embedded code only
  from a framework), device + Apple Silicon simulator, in a local Swift package
  `BloomCore`, built by `dev/ios-core` (cargo, `uniffi-bindgen-swift`,
  `xcodebuild -create-xcframework`), with a dSYM. A dylib keeps its Rust
  runtime to itself, which should end the clash with MPVKit's Libdovi on
  `rust_eh_personality`; spike S1 proves it (fallback: today's `ld -r`
  prelink with the export list `_uniffi_bloom_ffi_*`, `_ffi_bloom_ffi_*`).
- **No blocking call reaches Swift's thread pool.** Core is blocking
  (`ureq`, threads; no tokio). Swift's cooperative pool has about one
  thread per core and must not block. So every request is exported as a
  Rust `async fn` that runs the blocking call on a bounded Rust pool
  (8 threads) and answers through a one-shot `async_channel`. Swift sees
  `async throws`.
- **Cancellation**: cancelling a Swift task does not stop a Rust call.
  Long jobs return a handle with `cancel()`; short calls finish and their
  answer is dropped by the session guard.
- **Events**: one Rust subscription object per stream with
  `async fn next() -> Option<Event>` and `close()`, over the existing
  `async_channel` receivers (socket, Chromecast session, downloads,
  SyncPlay, fatal errors). A Swift task loops on it and writes into a
  MainActor model; the session's end calls `close()` (no leaked tasks).
- **Foreign traits** (Swift implements, Rust calls) are called on Rust
  threads. Rule: **no foreign-trait method waits for the main actor.**
  Each is a small `final class`, not isolated, over a lock-protected
  state: `SecretStore` (Keychain, §8.1), `SyncPlayer` (a snapshot of the
  player state the engine writes, and a command queue the engine reads),
  `Platform` (free space with `volumeAvailableCapacityForImportantUsage`,
  the network path, the remote commands this platform supports).
- **Panics**: UniFFI turns a panic into a Swift error only at a call. A
  panic hook in `bloom-ffi` logs to `os_log`, writes the message to a
  file, and sends a `Fatal` event; the app shows "Something went wrong.
  Restart Bloom." A bridge test covers it.
- **The session guard lives in core.** Core's `Session` carries the
  epoch; a call whose session ended returns `BloomError::Stale`, which
  Swift drops silently. A test fails with the guard removed (CLAUDE.md's
  race rule).
- **Errors**: `BloomError` from `connection::classify`: offline (with the
  cause), local network denied (§8.4), sign in again (401), not found,
  HTTP status, stale, other. Codes, not sentences.
- **Core returns codes and values; Swift owns every sentence.** Error
  texts (`explain`, `explain_load`), toasts, "2 min ago", sizes, the
  trigger descriptions, language names: Swift formats them through String
  Catalogs and `FormatStyle`. This makes localization real.
- **Logging**: Rust's `log` bridged to `os.Logger` with today's redaction
  rules.
- **TLS**: on iOS only (`cfg(target_os = "ios")`), core uses
  `rustls-platform-verifier`, so a private CA installed on the phone works
  in core as it does in Safari, AVPlayer and the image loader. The Mac
  keeps `webpki-roots`. Addresses resolve through `getaddrinfo`
  (IPv6-only networks with NAT64); if that changes a shared path, it goes
  behind the same cfg.

### 5.2 What the iOS app takes from `src/`, and when

About 8–10k lines of non-UI logic the iOS app needs live in `impl Bloom`
methods of the Mac app. They are **copied** into new core modules (or
written in Swift where the logic is iOS-shaped), never moved: `src/` is
not edited (§1). Each copy is made **directly before the phase that first
calls it from Swift**, so its seam is proven by its caller the same week:

| Logic in `src/` | Copied before | Becomes |
|---|---|---|
| `app/sessions.rs` | phase 3 | Core `ios::session`: sign-in, switch, sign-out, the epoch guard |
| hidden-content filter, quality tags read path | phase 3 | Filters the pages read |
| `stream.rs`, `adaptive.rs`, `queue.rs` | phase 4 | PlaybackInfo, quality, track map, transcode end, Auto, what plays next, refind |
| downloads coordination | phase 5 | Decided by spike S4 (§8.5) |
| `cast/target.rs`, `cast/remote.rs`, `syncplay/session.rs` | phase 6 | The target state machine; SyncPlay behind `SyncPlayer` |
| request blocks of `lists`, `hidden` (writes), `bookmarks`, `enhanced`, `seasons`, subtitle choice | phase 7 | Requests and rules |
| `admin/*` requests and field specs | phase 9 | Dashboard |

The copies live under `core/src/ios/` (compiled only with the `ffi`
feature), bring their tests, and do not change what the Mac compiles. The
cost is two copies of this logic until the Mac adopts them, if ever;
the doc comment of each copy names its source file in `src/`, so a fix in
one is easy to carry to the other.

### 5.3 Swift side

- `@Observable` models, MainActor by default. `AppModel` (servers,
  session, connection), a model per page owned by its view in `@State`,
  `PlayerModel`, `CastModel`, `DownloadsModel`.
- Loads in `.task` / `.task(id:)`, never `Task {}` in `onAppear`. Search
  debounces with `.task(id: query)` and `Task.sleep`.
- A `NavigationStack` per tab with `Hashable`, `Codable` routes.
- **`SessionScope`**: every store that outlives a view registers with it
  (image cache, `URLCache`, stored navigation paths, Now Playing,
  Spotlight entities, the widget cache, Handoff). A sign-out or a switch
  clears them all, as `cast_off` does today. A test per store.
- **Images**: an `ImageLoader` actor: a shared `URLSession` with a large
  `URLCache` (Jellyfin image URLs carry a tag), ImageIO downsampling off
  the main actor, an `NSCache` of decoded images, local files for
  downloaded items. Memory warning: drop the decoded cache and trickplay
  sheets.
- Logging: `os.Logger` per area; signposts around loads and player start;
  never a token or a URL with a key.

---

## 6. Structure and navigation

### 6.1 Tabs: a fixed set (→ Q9)

A SwiftUI `TabView` with the `Tab` API and four tabs that do not change
with the server:

- **Home**: the hero and the rows.
- **Library**: every library, then Favorites, Playlists, Collections, as
  a list of large rows with artwork; each opens its page.
- **Downloads**: the downloaded titles; the offline home. Offline mode
  selects this tab by itself.
- **Search**: `Tab(role: .search)` (it becomes the search field in the bar).

On iPad, `.tabViewStyle(.sidebarAdaptable)` shows the same in a sidebar,
with the libraries in a `TabSection` so each is one click away. Pinning a
library as its own tab (`TabViewCustomization`) can come after M1.

Why fixed: draft 1's per-library tabs left Playlists, Favorites with three
libraries or more, and a fourth library without a way in, and the HIG
says "don't disable or hide tab bar buttons". Every page of the inventory
now has a path (plan's parity table checks it).

- `tabBarMinimizeBehavior(.onScrollDown)`.
- `tabViewBottomAccessory`: what plays elsewhere — the cast target ("Living
  Room · Dune · 1:02:11", play/pause) or SyncPlay's group; active
  downloads show as a badge on the Downloads tab instead.
- **The account button** (the avatar) is a toolbar item on each tab's root.
  It opens the account sheet: profiles by server, "Servers &
  profiles…" (the connect screen as a full-screen sheet with "Back to
  {user}": add or remove a server, forget a profile), Play on, SyncPlay,
  Random item, Quick Connect, Settings, Dashboard (admin), About, Sign
  out. **Settings and the Dashboard open as large sheets with their own
  `NavigationStack`** (a list of sections, each pushed), so they never
  push onto a tab.

### 6.2 Pages

- Push with `navigationDestination`. Poster → detail uses the zoom
  transition (`matchedTransitionSource` + `.navigationTransition(.zoom)`);
  off under Reduce Motion.
- Large titles on root pages; inline titles on pushed pages. The system
  back button and edge swipe.
- `refreshable` on every page; `redacted(reason: .placeholder)` skeletons;
  `ContentUnavailableView` for empty and failed states.

### 6.3 Menus, sheets, popups

- **Card menu**: `.contextMenu` with a preview (the poster). The
  long-press work of the GPUI port comes free. Its entries arrive with
  their phase (plan's parity table): Play, played, favourite, remove from
  a row (phase 3); download (5); cast and group entries (6); lists (Add
  to playlist / collection, Remove from collection) and hide (7);
  metadata (9).
- **Menus**: SwiftUI `Menu` and `Picker` for library View/Sort/Filter,
  detail More, the player's gear.
- **Panels** (Play on, SyncPlay, episodes, bookmarks, subtitle search,
  request seasons) are sheets with detents; in the player on a phone in
  landscape, sheets at the trailing side.
- **Confirms** are `confirmationDialog` with the destructive role.
- **One popup at a time** holds by construction: one presented value per
  scene in the model, as `Bloom::popups.panel` today.
- **Toasts**: an overlay of our own (the system has none), glass, one
  queue, announced to VoiceOver.
- **Share**: `ShareLink` for an item's Jellyfin Web URL (beside "Open in
  Jellyfin Web").

### 6.4 iPad (plan phase 8)

- The sidebar; `NavigationSplitView` in Settings and the Dashboard.
- Regular width: adaptive grids, the detail page's two columns with the
  floating poster.
- `.commands` for the iPadOS 26 menu bar: View (tabs, sidebar, back,
  search) and Playback (play/pause, skip, next/previous, subtitles), with
  the Mac's shortcuts (Space, J/K/L, ←/→, E, F, ⌘F, ⇧⌘H).
- `.hoverEffect` on cards; the hover More button.
- Several windows: after M1.

---

## 7. Look and feel

### 7.1 What stays (DESIGN.md)

- **Dark only**, page `#101010`.
- **One ink; colour only for state**: text and icons `#f5f5f7`, quieter
  text the same ink at 70/60/50%; green, amber, red for state only.
- **No gradients on cards, no shadows.** Colour from the artwork.
- **The hero** as the signature surface.
- **Copy**: Jellyfin's words, plain sentences, an error says the cause and
  what to do.
- **Geist Mono** for the Quick Connect code, logs and paths.

### 7.2 What changes, each a decision for Daniel

- **The glass is the system's.** Bars, sheets, menus, tab bar and toolbars
  are system Liquid Glass with no custom backgrounds behind them (Apple:
  "remove background colours behind bars and sheets"). Custom
  `glassEffect` only on the player's controls and the toast. Reduce
  Transparency, Increase Contrast and Reduce Motion then work for free.
  The tuned shader values stay on the Mac.
- **The player bar over a bright picture** (→ Q13): `.regular`, `.clear`,
  or `.clear` with Apple's 35% dimming. Daniel rejected "a dark scrim over
  glass" and said the phone's bar "is glass over the picture, not a dark
  slab". Spike S3 shows the three over a bright frame; he picks.
- **One ink as the tint** (checked in S3): a near-white tint makes a
  prominent button white (the one white primary: good) but also every
  prominent button on a page, a selected iPad sidebar row white on white,
  and removes colour from links and swipe actions. S3 shows: a `Form`
  with switches and pickers, a prominent and a plain button, a
  destructive dialog, the iPad sidebar with a selection, a context menu,
  a medium sheet over posters (Daniel found phone menus "hard to see over
  background"). Controls that fail the One Ink Rule get a style of their
  own.
- **Motion** (Q14, decided): the system's push, pop, zoom and sheet motion
  replace `ui::motion` on iOS. That ends three rules on iOS: "going back fades"
  (the Quiet Return Rule; the system's pop slides), "no spring" and
  "nothing over 200 ms". Our own motion (hero cross-fade, toast, skip
  button, splash) keeps DESIGN.md's curve and stops under Reduce Motion.
  If Daniel agrees, DESIGN.md gets an iOS note.
- **Type** (→ Q2): SF through text styles, so Dynamic Type works (HIG:
  at least 200%); Google Sans for display titles (the hero title without
  logo art, the splash) through `Font.custom(_:size:relativeTo:)`.

### 7.3 Accessibility (a requirement in every phase)

- Every control: label, value, actions; stable `accessibilityIdentifier`s
  (not localized) for tests.
- Cards: label = title, value = "watched" / "3 unplayed" / "45% watched";
  custom actions for the context menu entries.
- Seek bar and volume `.adjustable`; a rotor of seasons and episodes on a
  series.
- Tap targets ≥ 44 × 44 pt for the app's own controls; Dynamic Type to the
  accessibility sizes with layouts that stack (`ViewThatFits`).
- **The player under VoiceOver or Switch Control**: the controls do not
  hide; the video is one element "Video" with the hint "Double tap to
  show the controls"; Magic Tap plays and pauses; the escape gesture
  closes a panel, then the player; Skip intro and Up next are announced.
- **Default tracks**: the server's subtitle mode first, then the system's
  "Closed Captions + SDH" and "Audio Descriptions" settings.
- **Subtitle look, in order**: Bloom's own setting when set, else the
  system's caption style (MediaAccessibility), else the Enhanced plugin's
  style. ASS styling and image tracks ignore all three.
- **Haptics** (`sensoryFeedback`): favourite or played toggled, a scrub
  crossing a chapter, a download done, an action failed, a skip.
- Right-to-left checked once in S3 (layout mirrors; the seek bar and skip
  glyphs do not).

---

## 8. Features by area

The full list is [inventory.md](inventory.md); the plan's parity table
gives each item its phase. This section says how each area is built and
what changes.

### 8.1 Sign-in, accounts, credentials (inventory §1.2, §5.1)

- Server list, address field, profiles, public users, sign in, Quick
  Connect both ways, multiple servers and profiles, sign out, remove
  server; the server's splash image and login disclaimer.
- Fields: address `.textContentType(.URL)` + `.keyboardType(.URL)` + Go;
  username **untyped**, no autocorrection, no capitals (Daniel's choice);
  password `SecureField` + `.password` + Go; Quick Connect code
  `.oneTimeCode` + number pad; new passwords (Change password, New user)
  `.newPassword` with the strong-password offer (→ Q18). `@FocusState`,
  `.submitLabel`.
- **Local network, before it is needed**: when the typed address is
  private (10/8, 172.16/12, 192.168/16, link-local) or `.local`, the
  connect screen says first: "Bloom asks to use your local network to
  reach this server." Then the system prompt.
- **Credentials**:
  - Core reads and writes tokens through the `SecretStore` foreign trait;
    Swift implements it over the Keychain
    (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`). `Config` holds
    no token on iOS. The token is never logged, never in a record Swift
    keeps, never in a URL but the AirPlay and Cast ones; Swift's engines
    and download session get it in a header from core, per request.
  - **The device id lives in the Keychain** (`ThisDeviceOnly`). A config
    restored from a backup with no matching id gets a new one.
  - A first-launch flag in `UserDefaults`: when absent (a fresh install),
    the app deletes its old Keychain items.
  - A profile with no token shows "Sign in again" and keeps its place; it
    is never removed by itself. A Keychain read that fails because the
    phone is locked (`errSecInteractionNotAllowed`, before the first
    unlock) means **locked**, not **absent**.
  - `config.json` and the download index use the file protection class
    `completeUntilFirstUserAuthentication`.
- **Device name**: since iOS 16 `UIDevice.current.name` is "iPhone".
  Settings › Profile lets the user name this device for the server's
  device list.
- Profile picture: `PhotosPicker`.
- Quick Connect's 3 s poll stops in the background and polls once at
  return (the user approves on the same phone).
- The splash: the bloom animation over the loading page, 1.5 s, never a
  wait, still under Reduce Motion.
- Server discovery (UDP 7359) needs the multicast entitlement: not in v1
  (→ Q5).

### 8.2 Browse (inventory §1.5–1.10)

- **Home**: the hero as a paging `ScrollView` with
  `backgroundExtensionEffect()` under the bars; a swipe steps it;
  auto-advance every 7 s stops while touched, under Reduce Motion, in Low
  Power Mode and in Low Data Mode. Rows in the server's `homesection`
  order as nested `LazyHStack`s with fixed card heights and
  `containerRelativeFrame` widths (three posters across on a phone).
  Hero trailers: not on iOS.
- **Library pages**: `LazyVGrid`, View/Sort/Filter in toolbar menus,
  **infinite scroll** (→ Q6) loading 100 at a time; **Random sort keeps
  one fixed page** with a "Shuffle again" button (the server shuffles
  again per page, which would repeat items). Play All / Shuffle. A
  section index on iPad when sorted by name. Server news reloads keep the
  place by item id.
- **Detail**: one column on a phone; actions as glass buttons under the
  title; tracks as `Menu` pickers; seasons, episodes, cast & crew, More
  Like This, Seerr rows, Enhanced lines; two columns on iPad regular.
- **Search**: the local title index answers as the user types; the server
  search on submit or a pause; Movies, Shows, Episodes, Discover on Seerr.
- **Hidden content and quality tags filter and mark these pages from
  phase 3** (they are read paths in core); their settings and menus come
  in phase 7.
- Favorites, Playlists, Collections from the Library tab. Playlist order
  with `reorderable()`.

### 8.3 Player (inventory §3)

**v1: mpv plays everything on the device; AVPlayer only when the route is
AirPlay** (→ Q16). This keeps "Play the file as it is" (PRODUCT.md), is
Swiftfin 1.7's default, and halves the test matrix. Direct play on
AVPlayer (Dolby Vision 5, Atmos passthrough) is added after M1 if S2's
numbers show mpv's path lacks them.

- **mpv renders into an `AVSampleBufferDisplayLayer`** (MPVUI's
  AVFoundation output: Swiftfin's default since 2026-11-11). It makes PiP
  possible and makes the video a normal layer that the controls' glass
  can blur (the `screenshot-sw` copy goes away). **Fallback** if S2 fails
  its numbers: the Metal output (`gpu-next` into a `CAMetalLayer`, as
  today), without PiP.
- **Swift drives mpv** (`MpvEngine`, MPVKit's C API; → Q3). The Mac's
  worker rules are rules of this class: nothing on the mpv thread waits
  for the network; subtitle files are fetched on their own task and added
  by path; external subtitle ids are a map; image subtitles are fetched
  when chosen; burn-in once as the fallback. Decisions (track map, device
  profile, quality, subtitle choice) come from core.
- `PlayerEngine` protocol with `EngineCapabilities`; a menu entry the
  engine lacks is hidden. On the AirPlay engine: no audio delay, no
  subtitle timing, no frame step, image subtitles burned in.
- **Presentation**: the player is a `fullScreenCover` over the tabs,
  `persistentSystemOverlays(.hidden)`, status bar hidden. On an iPhone it
  asks for landscape (`requestGeometryUpdate`, with the app delegate's
  `supportedInterfaceOrientationsFor`) and back to portrait when it
  closes; its layout works at any size, since iOS 27 can ignore the
  request. The iPad keeps its orientation. S3 checks both (the GPUI
  port's geometry request "windowed" the iPad app).
- **Controls**: custom SwiftUI, modelled on the system player (HIG). A tap
  toggles them (Daniel's iOS rule); a double tap on the left or right
  third skips (the single tap then waits ~300 ms for the double to fail:
  Daniel feels it in S2 before it is fixed); a drag on the seek bar
  scrubs with the trickplay preview; pinch toggles fit/fill. Hide after
  2.5 s; stay while paused, buffering, scrubbing, with a menu open, or
  under VoiceOver.
- **Back stops the player**, as today; PiP is the way to keep watching
  outside it (→ Q12).
- **Kept**: skip intro/credits/recap and auto-skip; Up next and autoplay;
  the queue and the lost-episode re-find; chapters, bookmarks, trickplay;
  audio and subtitle menus (image subtitles, timing, delay); quality with
  Auto and the ladder; speed (locked in SyncPlay); pause screen;
  playback info; episode picker; resume; Previous (restart, or previous
  item in the first 5 s); reporting.
- **PiP**: `AVPictureInPictureController` with
  `ContentSource(sampleBufferDisplayLayer:playbackDelegate:)`; started by
  the user's button, or automatically when the app goes to the background
  during playback; the player UI restores when PiP stops.
- **Now Playing**: the iOS 27 Now Playing framework (`MediaSession` with
  `MovieContent` / `TVShowContent`), never mixed with
  `MPNowPlayingInfoCenter` (Apple: undefined behaviour). Lock Screen,
  Control Center, Dynamic Island, headphones, the Watch.
- **Audio session**: `.playback` / `.moviePlayback`, activated at play.
  Interruptions: resume only when the system says `shouldResume`. Route
  change with the old device gone (headphones out): pause. The idle timer
  off while video plays.
- **HDR**: the device profile takes the HDR ranges from the device
  (`AVPlayer.eligibleForHDRPlayback`, the screen's EDR headroom), not a
  table. S2 checks HDR on the sample-buffer layer (tone-mapped or
  clipped).
- **Memory**: mpv's demuxer cache capped for a phone from
  `os_proc_available_memory`.
- **Heat and power**: in a serious thermal state or Low Power Mode, no
  trickplay preload; the quality stays as chosen (the product principle).
- **External screens** (USB-C to HDMI) mirror the phone in v1; a scene of
  their own can come later.
- **AirPlay**: `AVRoutePickerView` with `prioritizesVideoDevices`, hidden
  when `AVRouteDetector` finds no route. Picking a route reloads the item
  on AVPlayer at the position over the server's HLS, as `src/airplay`
  does on the Mac.

### 8.4 Cast, remote control, SyncPlay (inventory §4)

- **Play on** sheet: this device, Jellyfin devices, Chromecasts, AirPlay.
- **Chromecast**: the Rust Cast v2 session stays (pinning, reconnects,
  seek coalescing). Discovery in Swift with `NWBrowser` for
  `_googlecast._tcp`, handed to core. Info.plist:
  `NSLocalNetworkUsageDescription` naming both uses ("to reach your
  Jellyfin server on this network and find Chromecasts") and
  `NSBonjourServices` (`_googlecast._tcp`, `_CC1AD845._googlecast._tcp`).
  No multicast entitlement (TN3179).
- **Local network denied**: detected (`NWBrowser` `.waiting` with
  `PolicyDenied`, or an `NWConnection` probe when a private address fails)
  and shown as "Bloom may not use the local network. Allow it in Settings
  › Privacy & Security › Local Network", with a button to Settings — never
  as "server is offline". `connection::classify` gets the case.
- **Casting with the phone asleep** (v1): the phone plays no audio, so iOS
  suspends the app and the Cast socket ends. The cast goes on; at return
  the app reconnects and reads the receiver's state. The Jellyfin
  receiver app reports its own progress; the Default Media Receiver's
  reports stop while the phone sleeps (a known loss). Lock Screen control
  of a cast: not in v1 (iOS 27's remote sessions need a push server).
- **Remote control**: core's protocol. The commands iOS advertises come
  from the `Platform` trait: mute (the player's own), audio and subtitle
  stream, DisplayMessage; not system volume or full screen.
- **SyncPlay**: the sheet; the engine in core with `SyncPlayer`; **mpv
  only** — an AirPlay route in a group is refused with a sentence. The
  socket lives while the app plays; after a suspension it reconnects and
  rejoins at foreground; an interruption tells the group "this player
  waits".

### 8.5 Downloads and offline (inventory §5.6, §1.3)

- **Spike S4 decides the shape** before phase 5 (plan). Expected: on iOS,
  core keeps the index, `meta.json`, sidecars, segments, the storage rules
  and offline positions; a Swift `DownloadCoordinator` owns the transfers
  on one background `URLSession` and reports state into core. (The other
  shape, a `Transport` trait under core's engine, is kept if S4 shows
  resume data and validators fit core's `.part` + `If-Range` rules.)
- **Every file of a season is enqueued at once** (Apple: tasks started
  after a background relaunch are delayed more each time), limited by
  `httpMaximumConnectionsPerHost` (the "1 or 2 at a time" setting maps to
  it). `meta.json`, sidecars and images are fetched first, in the
  foreground.
- **What a force quit does**: the system cancels the transfers and does
  not relaunch the app (Apple, [V]). The entries show Paused with
  "Resume", and resume from resume data when the user returns.
- Files in Application Support, excluded from backup. Free space from
  `volumeAvailableCapacityForImportantUsage`. "Disk full" names the
  amount needed.
- **Cellular** (→ Q11): "Download over cellular" off by default
  (`allowsCellularAccess`); Low Data Mode honoured
  (`allowsConstrainedNetworkAccess`).
- Offline mode: the Downloads tab selected, the offline page and badge,
  positions flushed on reconnect; a cold launch in airplane mode opens
  Downloads.
- A local notification when a download ends (asked at the first
  download); a Live Activity only if it can be honest (it updates at
  completions only).

### 8.6 Settings (inventory §1.12)

A large sheet: a list of sections, each a `Form` (a `NavigationSplitView`
on iPad). Profile (picture, password, device name, Quick Connect, sign
out), Playback (with the cellular limit of Q11), Subtitles, Home, Display
(quality tags, Enhanced switches; no glass switch), Downloads (limits,
cellular; no "Show in Finder" — the Files app does not see the folder),
Hidden content, About (licences and credits incl. mpv/FFmpeg LGPL;
"Export diagnostics"). Server-side prefs saved as today; a pending save
flushes when the scene goes to the background.

### 8.7 Enhanced, Seerr, hidden content, lists (inventory §3.13, §5.7–5.10)

Through core. "Pause when hidden" / "resume on return" become the
background rule of §8.10. Keyboard shortcuts on an iPad with a keyboard.
A Seerr request stays behind an explicit button with a confirm.

### 8.8 Server dashboard (inventory §1.13)

After M1. `List`s and `Form`s; JSON editors with `Draft`'s merge in core,
never typed. Red confirms on restart, shut down, delete user/device,
revoke, Networking and Transcoding saves. Branding image with
`fileImporter`. The metadata editor: iPad first. Subset first (→ Q8).

### 8.9 System integration (after M1)

Widgets (Continue watching, Next up) and App Intents with item entities
(Spotlight, Siri, Shortcuts) — **off by default, a switch in Settings**,
since a family shares the phone (→ Q19), and never a hidden title.
Handoff, deep links (`bloom://item/<server>/<id>`), quick actions on the
icon (Search, Continue watching), a Control Center control for
play/pause. Not possible: the TV app.

### 8.10 Life cycle

What each part does at each event. Each row is a test (§11).

| Event | Player | Reports | Socket, SyncPlay | Cast | Downloads | Polls, timers, saves |
|---|---|---|---|---|---|---|
| To background while playing | Automatic PiP when allowed; else pause (→ Q12) | Progress report now, inside `beginBackgroundTask` | In PiP: stays. Paused: closes on suspension | Goes on (§8.4) | Background session goes on | Hero, dashboard reload, Quick Connect poll stop; pending saves flush |
| To background while paused | mpv stops decoding video; layer flushed on return (`requiresFlushToResumeDecoding`) | Progress report now; **Stopped** after 10 min paused in the background | Closes on suspension | — | — | As above |
| Return (`.active`) | Restore; if the transcode died (404s), PlaybackInfo again at the position | — | Reconnect; rejoin the group; reload what news would have reloaded | Reconnect, read the receiver | Reconcile with the session | Quick Connect polls once; timers restart |
| Interruption begins / ends | Pause / resume only on `shouldResume` | Report the pause | Group hears "waits" / rejoin | — | — | — |
| Route lost (headphones out) | Pause | Report | Group hears the pause | — | — | — |
| Network path change (`NWPathMonitor`) | — | — | Feeds `core::connection` at once (no probe timeout) | — | Session handles it | Expensive/constrained flags to the bitrate decision |
| Memory warning | Lower mpv cache | — | — | — | — | Drop decoded images, trickplay sheets |
| Next launch after the app was killed while playing | — | **Stopped** for the session that never ended | — | — | Reconcile | — |
| Thermal serious / Low Power Mode | No trickplay preload | — | — | — | — | Hero stops auto-advance |
| Play on sheet open, or a Jellyfin session is the target | — | — | — | Sessions polled (2 s, 15 s with the socket), foreground only; one poll at return | — | — |

---

## 9. Migration at M1

Until M1 the native app is `dev.bloom.ios.next`, beside the GPUI app. At
M1 (plan):

- The native build takes `dev.bloom.ios` and installs over the GPUI app.
- First launch: copy the GPUI build's `config.json` to `config.gpui.json`
  (kept until the first successful session), move each token through
  `SecretStore` into the Keychain, move the device id into the Keychain
  (same id: the server sees the same device), keep servers, profiles, cast
  pins and settings, write the config back without tokens.
- Downloads: the index and files are core's format; read in place.
- The GPUI iOS code is removed in the same milestone (plan).

---

## 10. Safety

CLAUDE.md's rules apply to every test. The native app enforces them; a
sentence in a plan is not a guard.

- **Test mode**, compiled into non-release configurations, **enforced in
  core**: `Client` and the cast session refuse every host that is not
  loopback or the named container; the Chromecast mock is forced; no real
  discovery (`NWBrowser` off); the AirPlay row is a stub; no playback
  report; playback muted. No Swift path can go around it.
- **UI tests and the random walk** run as `dev.bloom.ios.uitest` with a
  fresh container. The run asserts at its start that the config holds
  only the mock or the container, and stops if not.
- **Random walk on pages with writes**: an allow list per page of the
  roles it may tap, and no random-point taps inside a presented dialog.
  The deny list (labels never tapped, even on the mock) covers every
  write: Delete, Remove, Sign out, Forget, Revoke, Restart, Shut Down,
  Uninstall, Request, Save, Run, Stop, Scan, Enable, Disable, Use this
  match, Upload, Download to the library, New API key, Change password,
  New user, Mark played, Favorite, Hide, Play next on, Add to the queue
  of, and SyncPlay queue edits.
- **Device debug builds**: a persisted "Safe mode" setting (debug only)
  that survives a launch from the icon: no reports, muted, mock cast; a
  visible badge while on.
- **Never in a test**: the dashboard on Daniel's live server (writes run
  on the container only; Networking and Transcoding saves never on the
  live server); App Intents on a real session; a Seerr request; a cast
  device beyond discovery and status; printing a token.
- **Checks only Daniel runs**: an AirPlay hand-off to the Apple TV and a
  Chromecast load, with him present.

---

## 11. Testing

Daniel's concern is touch, gesture and animation bugs. The layers:

| Layer | Tool | What |
|---|---|---|
| Core | Rust tests, `dev/test-stress` | Every moved module brings tests |
| Bridge | Swift Testing on `BloomCore` | Round trip, errors, `Stale`, a stream that ends, `close()`, cancel, a panic, 60 parallel calls with a slow server under Thread Sanitizer |
| Models | Swift Testing | Models over protocol fakes of the core objects; each `SessionScope` store cleared |
| UI flows | XCUITest | Sign-in (both keyboard walks), browse, detail, play, settings, offline, cast on the mock; one pass on the iPad and one resized |
| Life cycle | XCUITest + the hook | Each row of §8.10 |
| Accessibility | `performAccessibilityAudit` in each flow | With an allow list in the repo, each entry with its reason (contrast over artwork) |
| Snapshots (required) | swift-snapshot-testing | 12 screens × 3 widths × 3 text sizes, mock data, stub images: catches "the label sits 5 points too high" |
| Random walk | XCUITest + the input path of spike S5 | Below |
| Performance | `XCTOSSignpostMetric` scroll metrics on the iPhone Air in the Release configuration; Instruments | A budget per page, checked from the result bundle (§18, "Performance budget"); a baseline in Xcode for a regression under it |
| Sanitizers | One flow under Thread Sanitizer, one under the Main Thread Checker | Rust threads call into Swift |
| Device checklist | Written per phase | What no simulator shows: Passwords key, local network prompt and denial, PiP, AirPods, a call, haptics, heat, HDR, the network matrix of §12 |

**The test hook** (debug builds only): a debug socket in the simulator
(`/tmp/bloom-ios-test.sock`, as today's channel) reporting page, presented
sheet, player state, errors, runtime faults, memory; on a device, a 1 pt
element with an identifier (not hidden from accessibility, so XCUITest
sees it).

**The extended mock** (`dev/mock-jellyfin`, plan phase 3 step 1): one
short clip (from `dev/make-sub-clip`) with segments, chapters and
trickplay; a WebSocket on `/socket`; 1,000- and 10,000-item libraries
behind a flag; fault modes (a delay per request, a 401 mid-session, a 404
for one item, a 503, a body cut short, a socket that drops). The random
walk and the flows then reach the player and the error states with no
real server.

**The random walk** (`BloomMonkey`):

- A seed drives every choice; every action is logged with its time
  offset in ms.
- Actions: tap an allowed element, tap a point (not in dialogs), swipe in
  four directions at ~200, 600 and 1,000 pt/s, edge swipes, long press,
  double tap, rotate, type, Back; interruptions (Home and return, app
  switcher, lock and unlock, Control Center, a memory warning, the mock's
  socket drop, offline and back). Every other action fires inside a
  transition, through the input path S5 proves (XCUITest waits for idle by
  default).
- Each run randomises the environment: text size, Reduce Motion, Increase
  Contrast, Bold Text, orientation, the smallest iPhone iOS 27 runs on, an iPad
  in three window sizes, the mock's fault mode.
- Invariants that hold on correct screens: the app runs; no crash log; no
  main-thread hang over 250 ms (the hook's watchdog); no runtime fault
  (SwiftUI "already presenting", "Modifying state during view update",
  "AttributeGraph: cycle", unsatisfiable constraints, read from
  `OSLogStore`); the focused field is above the keyboard; after settling,
  no spinner older than 5 s on the mock; Back returns to the page before;
  a button that presents something changed the hook's state; the player
  is never under a sheet of the page behind it; memory under a bound after
  500 actions; the app's own controls (by identifier prefix) ≥ 44 pt.
- On a failure: screenshot, video, steps, seed. Replay with the same time
  offsets 5 times and report the rate; shrink only failures that repeat.
- Runs: 10 minutes per PR touching `ios/`; CI runs the flows from phase 3
  on, the walk nightly when Daniel wants it.

Daniel's rule stays: no build goes to the phone before the simulator walk
passes, the sign-in fields by hand included.

---

## 12. Building and installing

- **Network**: ATS `NSAllowsArbitraryLoads` **alone** (adding
  `NSAllowsLocalNetworking` makes iOS ignore it, Apple [V]); ATS is
  enforced at run time whatever the distribution. ATS does not cover core's
  `rustls`, so both stacks are tested on a device matrix (plan phase 3):
  `http://` LAN IP, `http://` `.local`, `http://` public host, `https://`
  public, `https://` with a private CA, IPv6-only with NAT64; each for
  browse, images, mpv, a download, AirPlay.
- **Install**: `dev/ios --device <id>` with development signing, as
  today. A development profile lasts a year; the build itself does not
  expire.
- Icon Composer `.icon`; a launch screen.
- **Versioning**: the marketing version from the Cargo workspace; a build
  number that only rises, so a phone can tell builds apart.
- **Diagnostics**: MetricKit crash and hang diagnostics; the Rust dylib's
  dSYM kept; Settings › About "Export diagnostics" writes the last hour
  of the log (`OSLogStore`), the last panic file and the playback info
  for the share sheet. No analytics.
- String Catalogs from the start (English first; core sends no sentence),
  which costs little and keeps the text in one place.

---

## 13. Decisions (settled 2026-11-14)

Daniel, 2026-11-14: "Go with what you think and we can refine later I just
want to get this in a working state". So every open question takes the
recommendation; each can be revisited.

| # | Question | Decision |
|---|---|---|
| Q1 | Licence | Moot: Daniel's own devices (§2) |
| Q2 | Type | SF for the interface; Google Sans for display titles |
| Q3 | Who drives mpv | Swift (`MpvEngine`) |
| Q5 | Multicast entitlement (server discovery) | Not in v1 |
| Q6 | Library paging on a phone | Infinite scroll; Random keeps a fixed page |
| Q7 | iPad | Same target; its own pass in phase 8 |
| Q8 | Dashboard | Dashboard, Users, Tasks, Logs first; after M1 |
| Q9 | Tabs | Fixed: Home, Library, Downloads, Search |
| Q10 | Minimum iOS | 27 (§4) |
| Q11 | Cellular | A cellular streaming limit (default Original); "Download over cellular" off by default |
| Q12 | Background while playing; Back | Automatic PiP, else pause; Back stops the player |
| Q13 | Player glass over a bright picture | `.regular` (S3: `.clear` loses the controls over white) |
| Q14 | Motion | The system's motion on iOS; Bloom's curve for Bloom's own motion |
| Q16 | Engines | mpv for local playback; AVPlayer only for AirPlay |
| Q18 | Strong-password offer | Yes, on New user and Change password |
| Q19 | Spotlight and widgets | Off by default |
| — | Sheets over posters | The system's glass (it frosts more under Reduce Transparency) |
| — | The ink as the tint | Accepted: no system blue anywhere |

## 14. Decisions that rest on spikes

| Decision | Spike | Fallback |
|---|---|---|
| Dynamic XCFramework ends the Libdovi clash | S1 — **passed 2026-11-13** (app links MPVKit and `bloom_ffiFFI.framework`; no prelink) | Static prelink with the export list |
| `async_channel` wakes under UniFFI polling | S1 — **passed** (a 50 ms stream kept ticking through 60 parallel 2 s calls on the 8-thread pool, iOS 26.5 and 27.0 simulators) | A `with_foreign` push sink |
| mpv's sample-buffer output (PiP, HDR, 4K) | S2 | Metal output, no PiP |
| Download shape | S4 | `Transport` trait under core's engine |
| Input inside a transition | S5 | idb events from a host script, as `dev/ios-ui` |
| Glass, tint, motion with one ink | S3 — **run 2026-11-14**, findings below | Styles of our own per control |

---

## 15. What changed after the review

| Review finding | Change |
|---|---|
| Must 1: native build replaces Daniel's app | Separate bundle id until M1; migration at M1 (§4, §9) |
| Must 2: tests reach the live server and cast devices | Test mode enforced in core, uitest bundle id, allow lists, persisted safe mode, no live dashboard tests (§10) |
| Must 3: downloads check cannot pass; seam too early | S4; seam decided before phase 5; enqueue all; force-quit behaviour written (§8.5) |
| Must 4: bridge threading | Rust async over a bounded pool; foreign-trait rule; panic hook; guard in core (§5.1) |
| Must 5: token and credentials | `SecretStore`; device id in the Keychain; first-launch flag; locked ≠ absent; file protection (§8.1) |
| Must 6: network permissions | ATS shape fixed; device matrix; local network sentence; denial detection (§8.1, §8.4, §12) |
| Must 7: life cycle | §8.10 |
| Must 8: two engines | v1 mpv + AirPlay-only AVPlayer; capabilities; fallback; S2 with numbers; phase 4 split (§8.3, plan) |
| Must 9: pages with no way in | Fixed tabs; Settings/Dashboard as sheets (§6.1) |
| Must 10: testing | Hook, extended mock, invariants, timed replay, environments, interruptions, required snapshots, device hitches, sanitizers, checklist, harness first (§11, plan) |
| Must 11: design decisions dropped | Q13, Q14, the tint list in S3, Geist Mono (§7) |
| Must 12: plan order | Just-in-time copies into core, M1, parity table, filters in phase 3, iPad pass per phase (§5.2, plan) |
| Should 1–16 | Licence (moot: personal use, §2); one OS (§4, iOS 27); cellular (§8.5, Q11); TLS and NAT64 (§5.1); sentences in Swift (§5.1); diagnostics (§12); `SessionScope` (§5.3); restoration rules (plan phase 3); player accessibility (§7.3); SyncPlay mpv-only (§8.4); casting asleep (§8.4); platform commands (§8.4); storage and memory (§8.3, §8.5); HDR, screens, heat (§8.3); API points (§4, §8); store points (moot, §2) |

---

## 16. Gaps the parity table found (settled in draft 3, for Daniel's review)

| # | Gap | Decision |
|---|---|---|
| 1 | Lists entries in phase 3 or 7? | Phase 7, with the lists request block: Add to playlist / collection, Remove from collection, in the card menu and the detail's More |
| 2 | No way to the server list while signed in | "Servers & profiles…" in the account sheet (§6.1), phase 3 |
| 3 | "Search for subtitles…" had no phase | Phase 7 (it writes to the server: admin or subtitle management; on the deny list) |
| 4 | The player's phone trims | As today on a phone: transport, subtitles, episodes, gear, SyncPlay, Play on, PiP, AirPlay; no rating chip, Ends at, favourite, volume or bookmark button. On an iPad in regular width: all of them but the volume slider (the hardware buttons are the volume). No mute control on screen; a remote "mute" mutes the player |
| 5 | The keyboard help panel | Dropped as a panel: on an iPad with a keyboard the menu bar and the hold-⌘ overlay list the shortcuts (phase 8) |
| 6 | Settings › Automation | Dropped on iOS: a Unix socket and a helper process for Govee lamps are Mac-only |
| 7 | The sessions poll | Only while the Play on sheet is open or a Jellyfin session is the target, in the foreground; one poll at return (§8.10) |
| 8 | The link speed test | Not on an expensive or constrained path (cellular, Low Data Mode); there the cellular limit of Q11 applies |
| 9 | The offline badge | The bottom accessory shows "{server} is offline" with Retry, ahead of a cast or a group; the Downloads tab is selected; Home shows the offline page with Retry and Open Downloads |

---

## 17. Spike results

### S1: the bridge (2026-11-13, passed)

`ffi/` (bloom-ffi, UniFFI 0.32.2), `dev/ios-core`, `ios/BloomCore`. On the
iOS 26.5 and 27.0 simulators, against `dev/mock-jellyfin`: the app links
MPVKit and `bloom_ffiFFI.framework` (dynamic) with no clash and no
prelink; sign-in and Home load through the bridge; the token goes to the
Keychain through `SecretStore`; 60 parallel 2 s calls take 16 s on the
8-thread pool while a 50 ms stream keeps ticking (Swift's pool never
blocked); a panic in a call comes back as `Internal` and the pool lives on;
a panic on a core thread is kept for `last_panic`. The framework is 7.1 MB
for both slices (release, no LTO yet). The Mac app's tree has no UniFFI;
`dev/lint` is clean; `Cargo.lock` only gained entries.

### S3: the shell (2026-11-14)

`ios/app` (Bloom Next) and the XCUITest target `BloomShots`, which writes
screenshots for an iPhone 17 Pro and an iPad Pro 11 on iOS 27, with the
largest accessibility text and right to left. Findings:

1. **Player glass over a bright picture (Q13)**: `.clear` loses the ink
   controls over white (the times and the slider vanish); `.regular` and
   `.clear` with 35% dimming both read; `.regular` adapts to the picture
   instead of laying grey over it. Recommendation: `.regular`.
2. **Orientation**: asking for portrait while landscape is still allowed
   leaves the scene in landscape. Fixed: on an iPhone the pages allow
   portrait only, the player switches the mask to landscape, and closing
   switches it back before the request (as the GPUI app does).
3. **A fill-scaled picture widens its parent**: the hero showed half of two
   slides. A backdrop sits in a frame the container sizes (`Backdrop`).
4. **Per-library tabs leak into the phone's tab bar** (and push Search into
   "More") even with `defaultVisibility(.hidden, for: .tabBar)`; the
   section is built only in regular width. On the iPad the section still
   shows as a "Libraries" item in the top tab bar: phase 8.
5. **Presentations do not take the page's tint**: each sheet and the
   player set the ink themselves.
6. **The largest accessibility text** breaks the detail page's action row
   and clips the cast accessory: the stacked layouts of §7.3 are needed
   from phase 3, and the snapshot matrix will hold them.
7. **iPad**: "three posters across" makes huge posters; cards need an
   adaptive count by width (phase 8).
8. **System glass on a medium sheet over posters** lets the poster colours
   show strongly behind the rows (Daniel found phone menus "hard to see
   over background"): for Daniel to judge from the screenshot.
9. The native context menu, `confirmationDialog`, `Form`, and the tab bar
   follow the One Ink Rule with the ink as tint; red only on destructive
   entries.

### S2: the player (2026-11-14, done)

- **Built in the app on MPVKit** (mpv's own output, Metal through
  MoltenVK into a `CAMetalLayer`): direct play and the server's transcode
  through `PlaybackInfo`, audio and subtitle menus with the server's
  subtitle files fetched off mpv's thread, the server's track choice
  (`timeline_info`), segments and auto-skip, Up next and autoplay,
  quality, scrub previews from trickplay, Lock Screen through
  MediaPlayer, interruptions, background audio, offline play of
  downloads. Findings: mpv's MoltenVK output takes the layer's size at
  its start and does not follow a resize (on a phone the layer goes to
  mpv once it is landscape); a resume point past the end of a file must
  start it from 0, by the file's length, not by a clock; the end of a
  file must be an event, not a state flag.
- **MPVUI** (`ios/pipspike`, its own project: MPVUI and MPVKit both
  declare `Libmpv-GPL`, which SwiftPM refuses in one graph): mpv with
  the AVFoundation sample-buffer output plays the mock's clip in the
  iOS 27 simulator, sized to its view with no rotation work-around. The
  simulator reports Picture in Picture as not supported; the device test
  is open (PiP Spike is on Daniel's iPhone, playing Apple's test HLS
  stream). MPVUI also has an external-playback controller (HLS repackaged
  on a local server) and a route picker: the likely AirPlay path.
- **Device test, 2026-11-14, iPhone Air** (PiP Spike as a bench: stream,
  subtitles, decoding): Picture in Picture works on all three test
  streams (Apple's two HLS examples, Mux's test film): the window starts,
  plays on the Home Screen and in other apps. One failure: hardware
  decoding (VideoToolbox) with subtitles or on-screen text stops the
  player with `nativeVideoOutputUnavailable("no color space for native
  CI overlay")`. MPVUI 0.10.0 (the newest) draws the overlay with Core
  Image and needs the frame's colour space, which a hardware-decoded
  frame of those streams does not carry. Software decoding (`hwdec=no`)
  plays with subtitles, in PiP too.
- **Decision**: the app's engine moves from MPVKit to MPVUI (sample
  buffer). Hardware decoding by default; software decoding while mpv
  draws subtitles, until MPVUI tags the frame. Its mpv build is GPL,
  which is fine for personal use (§2). To measure on the phone after the
  switch: battery and heat of software decoding on a 4K HEVC film.
- **The small player** (Daniel, 2026-11-14: the Mac has its own small
  window so that the player keeps its own controls). iOS lets no app
  draw a window over other apps, and the system's Picture in Picture
  shows Apple's controls only. Daniel's choice, both: inside Bloom, the
  app's own small player over the pages (`MiniPlayer`: picture, play and
  pause, Skip or Next, full screen, close; a drag moves it to a corner);
  on leaving Bloom, the system's Picture in Picture takes the picture
  (`allowsAutomaticStartFromInline`), and auto-skip and autoplay go on.
  The player model belongs to `AppModel` (`player`, `playerFor`,
  `minimizePlayer`, `expandPlayer`, `closePlayer`), so both views and
  Picture in Picture share one playback.
- **Bloom on the phone, 2026-11-14**: system Picture in Picture plays a
  film from Daniel's server in Bloom Next (iPhone Air), the player view
  showing the system's "playing in picture in picture" placeholder.
- **MPVUI's mpv has no `ytdl` option**: setting it stops mpv at its
  start ("option not found"); a failure of mpv now shows as the player's
  error, not as an endless spinner.

---

## 18. Working on the native app

| Path | What |
|---|---|
| `ffi/` | `bloom-ffi`: the Swift face of bloom-core (UniFFI 0.32, proc macros). Wraps core, changes nothing the Mac compiles |
| `dev/ios-core` | Builds `bloom-ffi` for device and simulator, the Swift bindings, and `ios/BloomCore` (a local Swift package over a dynamic XCFramework) |
| `ios/app/` | The app (`project.yml` for xcodegen): bundle id `dev.bloom.ios.next` until M1, "Bloom Next" on the phone |
| `ios/app/BloomShots/` | The UI tests (`Shots`), the random walk (`Monkey`) and the performance tests (`Performance`) |
| `ios/pipspike/` | Spike S2 on MPVUI, a project of its own |
| `dev/ios-next` | `build`, `test [name]`, `perf [name]`, `perf-device [id] [name]`, `monkey [steps] [seed]`, `device [id]` |
| `dev/mock-jellyfin` | Gained, for these tests and only when asked: a clip for streams and downloads (`BLOOM_MOCK_CLIP`), a subtitle file and an intro segment with it, Quick Connect (`BLOOM_MOCK_QUICKCONNECT`), a controllable session (`BLOOM_MOCK_SESSION`), the server's socket and `POST /mock/socket` to push a message (`BLOOM_MOCK_SOCKET`), the Jellyfin Enhanced plugin with hidden content, bookmarks and Seerr lists, never a request (`BLOOM_MOCK_ENHANCED`), a user who may download (`BLOOM_MOCK_DOWNLOADS`). Playlists and the user's settings are kept for a run. Without the variables it answers as before |

The UI tests launch the app with `BLOOM_SERVER` (the mock),
`BLOOM_CHROMECAST_MOCK`, `BLOOM_MUTED` and `BLOOM_NO_REPORT`; they never reach
Daniel's server or a real cast device. `BLOOM_CONTROLS_STAY` keeps the
player's controls up for them. SyncPlay and the dashboard need a real
server: `SHOT_SYNCPLAY=http://127.0.0.1:8097 dev/ios-next test` runs
`testSyncPlay` and `testDashboard` against `dev/jellyfin-local` (account
`bloom-test`, through `BLOOM_USER`/`BLOOM_PASSWORD`); without it they skip.

What the tests taught (2026-11-14):

- XCUITest waits for the app to go idle before each tap, and a playing
  video never does: a tap waits 60 s and then lands on a menu that moved.
  Pause first, then open a player menu.
- The player turns the interface to landscape while the simulated device
  stays portrait; taps on a button work, taps inside a submenu can land on
  Close. Pausing first avoided it in every test so far.
- A lazy list keeps rows below the fold out of the accessibility tree: a
  row a sheet grew past (more saved profiles, a queue) needs a swipe first.
- `dev/ios-next test` does not make the bindings again; after a change to
  `ffi/`, run `dev/ios-next build` first. The first Xcode build after new
  bindings can stop with "exit code 0 but produced no further output";
  `build` tries once more.

### Performance budget (spec §7; replaces the Mac's `frame_cpu`)

The class `Performance` (`ios/app/BloomShots/Performance.swift`) measures,
against the mock with 200 extra films (`BLOOM_MOCK_EXTRA_MOVIES`), in the
Release configuration, five runs per test (`PERF_ITERATIONS`):

| Test | Metric | Budget |
|---|---|---|
| `testScrollHome`, `testScrollRow`, `testScrollLibrary`, `testScrollSearch` | Hitch time ratio of `XCTOSSignpostMetric.scrollingAndDecelerationMetric` | Under 5 ms/s (the mean of the runs) |
| `testColdLaunch` | `XCTApplicationLaunchMetric`, to the first frame | 400 ms |
| `testOpenDetail`, `testOpenPlayer` | Clock, CPU, memory | None: watched for a trend |

The numbers come from Apple's guidance. A hitch time ratio under 5 ms/s
is "good", 5 to 10 is a "warning", over 10 is "critical". 400 ms is
the length of the launch animation, the target for the first frame. The
clock of the two pages holds XCUITest's own queries (0.1 to 0.3 s per
query), so it is no budget.

XCTest does not hand a measured value to the test. `dev/ios-next` reads
the result bundle after the run (`xcresulttool get test-results
metrics`), prints one `PERF` line per metric (the mean, the maximum, the
budget) and fails a metric over its budget. No baseline in Xcode is
needed for that. A baseline is still useful for a regression under the
budget: open `target/ios-next/perf-<port>.xcresult` in Xcode, select the
diamond of a measure, and choose "Set Baseline".

How to run:

1. Run `dev/ios-next perf [name]` for the simulator. The `PERF` lines
   are printed, not checked (`BLOOM_PERF_GATE=1` checks them).
2. Connect the iPhone Air and put it on the house network.
3. Run `dev/ios-next perf-device [udid] [name]`. The mock listens on
   every interface (`--host 0.0.0.0`), and the app reaches it at the
   Mac's address (`ipconfig getifaddr en0`, or `BLOOM_PERF_HOST`).
4. If macOS asks whether python3 may accept incoming connections, allow
   it. If the phone asks for the local network, the test taps "Allow".
5. Read the `PERF` lines. The run fails when a metric is over its budget.

What to know:

- A simulator records no hitch time ratio: only the duration of the
  scroll (about 2.6 s for three flicks on 2026-11-14). Its launch took
  2.3 s. Neither number is the budget. Only the phone's numbers count.
- `dev/ios-next test` skips the class (`-skip-testing:BloomShots/Performance`):
  it is slow (about 8 minutes at three runs) and noisy beside other work.
- The device run installs the test build over the installed app (the
  same bundle id) and signs in to the mock. The mock's "tester" account
  is then the active one; switch back to the own account in the app, or
  run `dev/ios-next device`. A bundle id of its own for test builds
  (a build setting in `project.yml`) would end this.
