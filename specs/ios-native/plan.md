# Bloom for iOS, native: the plan

Draft 3, 2026-11-13: the app is for Daniel's own devices (no store), and
the Mac app is not touched (spec §1). The spec is [README.md](README.md) ("spec §n");
"inv §n" points into [inventory.md](inventory.md). Draft 1 was reviewed
([review.md](review.md)); spec §15 lists the changes.

Rules for every phase:

- **The Mac app is not touched**: no file in `src/` or the Mac's vendored
  crates changes; core changes are additive (new modules, the `ffi`
  feature, `cfg(target_os = "ios")`). Each step checks it: the Mac builds
  with no new warnings, `dev/test`, `dev/lint` and CI stay green.
- Logic the iOS app needs from `src/` is copied into `core/src/ios/` with
  its tests, directly before the phase whose Swift code first calls it
  (spec §5.2).
- One step at a time, each shown in a build, each on a branch with a PR.
- From phase 3 on, each phase's checks include one pass on the iPad
  simulator and one resized window, and the phase's device checklist.
- No build on the phone before the simulator walk passes, the sign-in
  fields by hand included.
- The safety rules of spec §10 hold in every test.
- A phase is done when its checks pass, not when its code is written.
- The GPUI iOS app is frozen from phase 3 (crash fixes only), and its iOS
  build is retired at M1.

Sizes are focused working days, for ordering, not promises. There is no
phase 1: draft 1's "move logic into core" phase became copies made just
before the phases that use them.

```
0 decisions ─ S1..S5 spikes ─ 2 bridge rules & CI ─ 3 skeleton & browse ─ 4a-4d player
  ─ 5 downloads ─ 6 cast & SyncPlay ─ 7 Enhanced & lists ─ M1 the phone switches
  ─ 8 iPad ─ 9 dashboard ─ 10 system integration ─ 11 release
```

---

## Phase 0: decisions and setup (Daniel; no app code)

1. Answers to the open questions (spec §13), or the recommendations
   accepted.
2. Apple setup for `dev.bloom.ios.next` and `dev.bloom.ios.uitest` (team
   T46QDHL88N): background mode `audio`; App Groups only when widgets
   come (phase 10); no multicast, no associated domains.
3. **The parity table** (`specs/ios-native/parity.md`): one row per
   inventory item, with its phase, its status, and "dropped on purpose"
   with the reason. Every page needs a path in the new structure
   (spec §6.1); a row without one is a bug in the spec.
4. Check that CI's `macos-26` runner has Xcode 27.

**Done when** every Q has an answer and the parity table has a phase or a
reason in every row.

---

## Phase S: spikes (about 12–14 days), each a throwaway branch

Each ends with a note in the spec (§14) and its numbers.

### S1: the bridge (2–3 days)

1. Crate `ffi/` with UniFFI 0.32: `Client::new`, `public_info`,
   `authenticate_by_name` as Rust `async fn` over the bounded pool;
   `pages::load_home`; `realtime::start` as a subscription; a
   `SecretStore` foreign trait; the panic hook.
2. `dev/ios-core`: both targets with `IPHONEOS_DEPLOYMENT_TARGET=27.0`,
   `uniffi-bindgen-swift`, a dynamic XCFramework with a real `.framework`
   and a dSYM, a local package `BloomCore`.
3. An app target that links `BloomCore` **and MPVKit**.
4. Stress: 60 parallel calls against a mock with a 2 s delay while a
   subscription delivers events, under Thread Sanitizer; a foreign-trait
   call during a main-thread call; a panic on a core thread.
5. Size of the dylib with `core::images` behind a feature; the cost of a
   100-item page through the buffer; a 10,000-item title index queried in
   Rust.

**Pass**: the app links with MPVKit; no thread-pool stall (the
subscription keeps delivering under load); no TSan report; the panic
reaches Swift as `Fatal`.

### S2: the player (5–6 days)

On the iPhone Air, MPVUI's sample-buffer output (or its patches in our
MPVKit build), against `dev/jellyfin-local`:

1. A 4K HEVC 10-bit HDR file for 30 minutes: dropped frames, thermal
   state, hardware decode on.
2. A direct MKV with PGS, one with heavy ASS (who draws them into the
   buffer, at what cost); subtitles in PiP for an image track.
3. 23.976 fps cadence on the 120 Hz screen; A/V sync over AirPods; seek
   latency.
4. HDR and Dolby Vision on the sample-buffer layer: tone-mapped or
   clipped (a DV sample is part of the spike, not optional).
5. Life cycle: to background with PiP, without PiP (pause), lock and
   unlock, an interruption (a call), return with
   `requiresFlushToResumeDecoding`.
6. The AirPlay engine: an AirPlay route picked during mpv playback
   reloads on AVPlayer at the position (with Daniel present for the real
   Apple TV; otherwise the simulator's route list only).
7. The tap / double-tap delay: Daniel tries it.

**Pass**: dropped frames under 0.5% on the 4K file; no thermal state
above `.fair` in 30 minutes; PiP shows the PGS track, or the spec says it
does not; the life-cycle cases return to a playing picture. Else the
Metal fallback (spec §14).

### S3: the shell (2 days)

1. The fixed tabs (Q9), `sidebarAdaptable` on iPad with six libraries in a
   `TabSection`, `tabBarMinimizeBehavior`, `tabViewBottomAccessory`.
2. The player as `fullScreenCover`: the landscape request on an iPhone and
   on an iPad (must not window the iPad app), the home indicator hidden.
3. The design screenshots of spec §7.2: the player bar over a bright
   frame three ways (Q13); a `Form`, prominent and plain buttons, a
   destructive dialog, the iPad sidebar with a selection, a context menu,
   a medium sheet over posters, all with the ink as tint.
4. Every Dynamic Type size, Reduce Transparency, Increase Contrast, Reduce
   Motion, right-to-left, resized (Device Hub).

**Pass**: Daniel has picked Q13 and accepted or changed Q14 from the
screenshots.

### S4: background downloads (2 days)

One background `URLSession` against `dev/jellyfin-local`:

1. Does `/Items/{id}/Download` send `ETag` or `Last-Modified` (resume data
   needs one)?
2. Resume data across a pause, a suspension, a system termination
   (`kill` from Xcode), a force quit, a reboot before the first unlock.
3. The delay of tasks 2, 3, 4 when started from the background vs all
   enqueued at once.
4. A server file replaced mid-download.

**Pass**: the shape of spec §8.5 is written down with the answers.

### S5: input inside a transition (1–2 days)

Prove one path that sends a tap 80 ms after a push starts, and a swipe
during a sheet's rise: XCUITest with its idle wait bypassed in the test
runner, or idb events from a host script. Record the choice; the random
walk is built on it.

---

## Phase 2: the bridge's rules and CI (about 3–4 days)

The bridge surface grows phase by phase; this phase sets its rules.

- `bloom-ffi` with the pool, the subscription pattern, `close()`, the
  panic hook, `BloomError` (codes, no sentences), `Stale`, the `ffi`
  feature on core types, `log` → `os.Logger`; `rustls-platform-verifier`
  and `getaddrinfo` behind `cfg(target_os = "ios")`, so the Mac's TLS is
  unchanged.
- CI: `cargo tree -p bloom` has no `uniffi` (the Mac build is unchanged); the `ios-build` job runs
  `dev/ios-core` and builds the app shell for the simulator; Swift bridge
  tests run.

**Done when** CI builds the XCFramework and the bridge tests pass,
including `Stale` (fails with the guard removed) and the panic.

---

## Phase 3: skeleton and browse (about 14–18 days)

Copied into core first: `Session` (with the epoch guard), the
hidden-content filter and the quality-tags read path.

1. **The test harness first**: the uitest bundle id and test mode
   (spec §10), the hook, the extended mock (clip, socket, large
   libraries, fault modes), the random walk on S5's input path, the
   snapshot target, CI running the flows from here on.
2. App, scenes, `AppModel`, `SessionScope`, `SecretStore` over the
   Keychain, device id in the Keychain, first-launch flag, file
   protection.
3. Sign-in (spec §8.1): servers, profiles, public users, Quick Connect
   both ways, multiple accounts, sign out, remove server; the local
   network sentence and denial; the offline page and the Downloads tab
   switch.
4. The fixed tabs, the account sheet, Settings as a sheet, toasts.
5. Home, Library tab and pages (infinite scroll, Random's fixed page),
   detail for every kind, search with the title index; the card menu's
   phase-3 entries; hidden content and quality tags applied.
6. Settings: Profile (picture, password, device name), Playback,
   Subtitles, Home, Display, About (with "Export diagnostics").
7. State restoration: the path per server and user; dropped when the
   session differs; a 404 item pops its page; never into the player;
   library place by item id.

**Checks**:
- The flows pass on the mock in CI; the snapshot matrix is recorded; the
  audit passes with its allow list.
- The random walk runs 30 minutes on the mock with no invariant broken.
- Scroll hitch metrics recorded on the iPhone Air for Home and the
  1,000-item library (the baseline).
- The network matrix of spec §12 on the device: browse and images for
  each address kind.
- The device checklist: Passwords key behaviour on the username and
  search fields with saved passwords, the local network prompt and a
  denial.

---

## Phase 4: the player (about 16–22 days, in four parts)

Copied into core first: `stream`, `adaptive`, `queue`.

### 4a: engine and shell

`PlayerEngine` + `EngineCapabilities`, `MpvEngine` on the sample-buffer
output (or the fallback), the `fullScreenCover`, controls (tap toggles,
double tap skips, scrub with trickplay, pinch fill), reporting, resume,
the life-cycle rows of spec §8.10, the audio session.

### 4b: tracks and quality

Audio, subtitles (image subtitles, timing, look in the spec's order),
audio delay, quality and Auto, speed.

### 4c: segments and the queue

Skip and auto-skip, Up next and autoplay, the queue, Previous, the
lost-episode re-find, the pause screen, playback info, the episode
picker.

### 4d: system

PiP, Now Playing (MediaPlayer), the AirPlay engine and its reload, the
player under VoiceOver and Switch Control, haptics, HDR from the device,
the memory cap, heat and Low Power rules.

**Checks** (each part its own): on the container with the sub clip and
the mock's clip: every subtitle kind, a transcode, a direct MKV, skip,
Up next, a resume position reported (the container, never Daniel's
server); the random walk extended into the player (the mock's clip); the
life-cycle tests; PiP in and out and the Lock Screen on the iPhone Air;
the AirPlay hand-off **only with Daniel present and asked first**.

---

## Phase 5: downloads and offline (about 6–8 days)

The shape S4 chose (spec §8.5); its core seam moves first. The Downloads
tab, buttons, storage and cellular settings, offline playback with
sidecars and segments, offline positions, the local notification.

**Checks**: suspended mid-file; terminated by the system (Xcode `kill`,
memory pressure); force quit (entries show Paused and resume); reboot
before the first unlock; a server file replaced mid-download; disk full;
airplane mode on and off mid-file; a cold launch in airplane mode opens
Downloads and plays with subtitles and skip; positions flush on
reconnect (the container).

---

## Phase 6: cast and SyncPlay (about 7–9 days)

Copied into core first: `cast/target`, `cast/remote`, `syncplay/session`.
Play on sheet; Chromecast with `NWBrowser` and the Rust session; casting
with the phone asleep (spec §8.4); remote control both ways with the
platform's commands; SyncPlay with `SyncPlayer` (mpv only).

**Checks**: everything on the cast mock and the container; the real
Chromecast discovered and its status read, nothing more; SyncPlay with
two simulators (or the simulator and `dev/run-second`) on the container,
drift within today's numbers (`dev/syncplay-skew`); an interruption in a
group; a suspension and rejoin.

---

## Phase 7: Enhanced, Seerr, hidden content, lists (about 5–7 days)

Copied into core first: the request blocks. Ratings, links, bookmarks,
hidden content's menus and settings, quality tags settings, pause screen
style, request more seasons (a confirm; never in a test), playlists and
collections editing, the card menu's remaining entries.

---

## Milestone M1: Daniel's phone switches

**Gate**: the parity table is green for the phone (every row done or
dropped with a reason); the device checklists of phases 3–7 passed; the
random walk ran an hour on the mock and 30 minutes on the container with
no invariant broken.

1. The native app takes `dev.bloom.ios`; the migration of spec §9 runs on
   Daniel's phone (with `config.gpui.json` kept until the first good
   session).
2. The iOS build of the GPUI app is retired: the `ios/` Rust crate,
   `ios/host/exports.txt`, `vendor/gpui-pre-ios` and the workspace's
   iOS-only dependency lines; `dev/ios-ui`'s a11y mode; `dev/ios` builds
   the native app; CLAUDE.md's iOS section rewritten (the GPUI notes
   moved to an archive file). `src/` is not edited: its
   `cfg(target_os = "ios")` code stays dormant, and cleaning it is a
   Mac-side decision for later. The Mac build is checked unchanged.

**Done when** Daniel uses the native app on his phone and a fresh clone
builds both apps with the documented commands.

---

## Phase 8: iPad (about 5–8 days)

Spec §6.4: sidebar, split views, regular-width layouts, the menu bar
commands and keys, hover, Stage Manager sizes, a hardware keyboard
driving the player.

---

## Phase 9: the dashboard (about 10–15 days)

Spec §8.8, the subset of Q8 first. Copied into core first: the admin
requests and field specs. **Checks**: writes on the container only; on
Daniel's server, read-only plus the save of an unchanged object (the
five-step rule), never Networking or Transcoding; no UI test or random
walk opens the dashboard on the live server.

---

## Phase 10: system integration (about 5–8 days)

Spec §8.9: widgets and App Intents (off by default, filtered by hidden
content), Spotlight, Handoff, deep links, quick actions, a Control Center
control. App Intents tests run on the container. Re-check BoltFFI (the
research's 2027 note) and the iOS 27 APIs if the minimum moved.

---

## Phase 11: polish for daily use (about 2–3 days)

Spec §12: the network matrix on the device again, the `.icon` and launch
screen, versioning, `dev/ios --device` documented as the install path,
"Export diagnostics" checked end to end.
