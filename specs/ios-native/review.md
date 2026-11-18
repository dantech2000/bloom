# Review of the native iOS spec and plan

Reviewer: Fable, 2026-11-13. Read: README.md (the spec), plan.md,
inventory.md, the three research notes, CLAUDE.md, DESIGN.md, PRODUCT.md,
and the parts of `core/` the claims rest on.

Markers: **[V]** I checked the claim today in an Apple page or in the repo
(the source is given). **[M]** from memory; spike it before the plan relies
on it.

Note: the spec's header and file table point to `review.md`. This file is
`review-fable.md`. Fix the link or rename the file.

## Verdict

The direction is right: SwiftUI first, the Rust core behind UniFFI, the
system's glass, mpv on a sample-buffer layer. The research is good.

The spec is not ready to build from. Twelve points need a fix first. The
largest are: the tests can reach Daniel's live server and the real cast
devices; the native build replaces his working phone app from the first
install; the bridge's threading model starves Swift's thread pool; the
download design has a check that cannot pass; and the app's behaviour when
it leaves the foreground is not written down at all.

The testing section does not yet answer Daniel's concern (touch, gesture
and animation bugs). The random walk cannot enter the player, and its main
idea (input inside a transition) is not possible with XCUITest as written.

---

## Must fix

### 1. The native build replaces Daniel's working app, and the migration cannot be undone

- **Where:** spec §4 ("Bundle id stays `dev.bloom.ios`"), §9, plan phase 3
  step 1.
- **What:** The first device install in phase 3 replaces the GPUI app on
  his phone. At that time the native app has no player. The migration then
  moves the tokens to the Keychain and writes `config.json` back without
  them. A GPUI build installed again finds no tokens.
- **Why:** The household uses this app (PRODUCT.md: "The household
  first"). The native app reaches parity at phase 7 at the earliest.
- **Fix:**
  1. Give the native app its own bundle id until parity (`dev.bloom.ios.next`),
     with its own container and its own device id.
  2. Move the migration to a "cut-over" step after the parity gate. Keep a
     copy of the old `config.json` until the first successful session.
  3. Write the cut-over as a plan phase with its own check (see finding 12).

### 2. A test can reach the live server, the real Chromecast and the Apple TV

- **Where:** spec §10, §11; plan "The random-walk tester"; plan phase 9.
- **What, in five parts:**
  1. **Random-point taps skip the deny list.** The deny list filters
     elements by label. A tap on a random point can land on "Delete",
     "Shut Down" or the confirm button of a dialog.
  2. **The deny list is too short.** Missing writes: Mark played, Favorite,
     Hide, Run and Stop (tasks), Scan All Libraries, Save, Enable/Disable,
     Forget this profile, Use this match, Upload image, Download to the
     library (subtitles), New API key, Change password, New user, Play
     next/Add to queue on a remote session, SyncPlay queue edits.
  3. **Nothing forces the mock.** "Against the mock server only" is a
     sentence, not a guard. A simulator that got Daniel's servers
     (`dev/ios-account --own` does that today) shows his profile tile. One
     random tap opens his session.
  4. **Cast devices.** The simulator shares the Mac's network and does not
     enforce local network privacy (research, TN3179). `NWBrowser` finds
     the "Living Room" box. Inventory §8: "A tap that lands on a cast
     device row connects to it." The next Play then loads media on the TV.
     The AirPlay row opens the system route picker, which lists the real
     Apple TV; a random tap can pick it.
  5. **The switches do not exist on a device.** `BLOOM_NO_REPORT` and
     `BLOOM_MUTED` are launch environment. An app that Daniel starts from
     its icon has no environment. The phase 4 device checks (PiP, Lock
     Screen) then play with reports on, possibly on his own session.
- **Why:** CLAUDE.md's safety rules; the 2026-10-03 incident.
- **Fix:**
  1. A **test mode compiled into non-release configurations** and enforced
     at the transport: core refuses every host that is not loopback or the
     named container, forces the Chromecast mock, starts no real discovery,
     replaces the AirPlay row with a stub, sends no report, plays muted.
     The guard lives in core (`Client` and the cast session), so no Swift
     path can go around it.
  2. The UI test and monkey targets use a **separate bundle id and a fresh
     container**. The run asserts at its start that the config holds only
     the mock or the container, and stops if not.
  3. In monkey runs, remove random-point taps inside presented dialogs,
     and make the deny list an allow list per page ("the monkey may tap
     these roles") for pages with writes.
  4. For device builds, a persisted "safe mode" setting (debug builds
     only) that survives a launch from the icon, shown as a visible badge.
  5. Plan phase 9: no UI test opens the dashboard on the live server. The
     "save of an unchanged object" check excludes Networking and
     Transcoding on the live server; those run on the container only.
  6. Plan phase 10: App Intents tests ("Play next episode") run on the
     container. Siri on the phone plays on the real session.

### 3. The download design has a check that cannot pass and a seam that does not fit

- **Where:** spec §8.5; plan phase 1 step 7 and phase 5.
- **What:**
  1. Phase 5 check: "a download survives the app … being ended (kill from
     the switcher between two files)". Apple: "If the user terminates the
     app from the multitasking screen, the system cancels all of the
     session's background transfers. In addition, the system does not
     automatically relaunch apps that were force quit by the user." **[V]**
     https://developer.apple.com/documentation/foundation/urlsessionconfiguration/background(withidentifier:)
  2. The core engine and a background session disagree on who owns what
     **[V repo: `core/src/downloads/engine/job.rs`]**:
     - Core resumes a `.part` file with `Range` + `If-Range` and a stored
       validator. A download task writes to a system temp file and resumes
       from opaque resume data.
     - Core runs 1 or 2 jobs and starts the next when one ends. Apple
       delays each new task that the app starts after a background
       relaunch and says to "start many download tasks at once" **[V]**
       https://developer.apple.com/documentation/foundation/downloading-files-in-the-background.
       A season of 10 episodes started one by one slows down more with
       each file.
     - Core detects a changed server file by size and re-reads the item.
       Core counts 6 attempts and detects stalls. The system retries by
       its own rules and reports only the end.
     - Sidecars, images and `meta.json` are small fetches by `ureq`. They
       stop when the app suspends.
  3. On a background relaunch the app must build the core, open the
     engine and take the event with no UI and possibly before the first
     unlock (finding 5).
- **Why:** Phase 1 designs the `Transport` trait before any of this is
  known. A wrong seam costs the Mac engine a second refactor.
- **Fix:**
  1. Add **spike S4 (2 days)**: one background session against
     `dev/jellyfin-local`. Answer: does `/Items/{id}/Download` send an
     `ETag` or `Last-Modified` (resume data needs one **[M]**)? Does
     resume data survive a pause, a suspension, a system termination, a
     reboot? What arrives after a force quit? How long is the delay for
     task 2, 3, 4 started from the background?
  2. Decide from S4 between two shapes and write the choice down: (a) the
     `Transport` trait; or (b) on iOS, core keeps the index, the metadata
     and the rules, and a Swift `DownloadCoordinator` owns the transfers
     and reports state changes into core. I expect (b).
  3. Enqueue every file of a season at once and limit with
     `httpMaximumConnectionsPerHost`. Fetch `meta.json`, the sidecars and
     the images before the big file, in the foreground.
  4. Rewrite the phase 5 checks: suspended; terminated by the system
     (`kill` from Xcode or memory pressure, not the switcher); force quit
     (the entry shows Paused or Failed with "Try again", and resumes);
     reboot before first unlock; server file replaced mid-download; disk
     full; airplane mode on and off mid-file; cold launch in airplane
     mode opens Downloads.
  5. Move the seam from phase 1 to the start of phase 5.

### 4. The bridge's threading model is wrong in three places

- **Where:** spec §5.1 ("exported sync and called from Swift in
  `@concurrent` functions"), §5.1 "Callbacks into Swift", §8.4 ("the
  `SyncPlayer` trait implemented by the Swift `PlayerModel`").
- **What:**
  1. **Blocking calls on Swift's cooperative pool.** A `@concurrent`
     function runs on the cooperative pool, which has about one thread per
     core **[M]**. A `ureq` call blocks its thread for the whole request.
     A library page, 40 posters' metadata and a slow server block every
     thread. Then no other Swift task runs, the UniFFI `next()` polls
     included. Apple's rule is that code on the pool must not block
     **[M]**.
  2. **A foreign trait on a MainActor class.** A UniFFI foreign trait
     must be `Sendable` and is called from a Rust thread (research,
     rust-swift-bridge §1). `PlayerModel` is MainActor by the project's
     default isolation. A sync method that returns a value
     (`SyncPlayer::status`) cannot hop to the main actor. If it blocks on
     the main thread while the main thread waits in a Rust call, the app
     deadlocks.
  3. **Panics on Rust's own threads.** UniFFI turns a panic into a Swift
     error only at a call boundary. Core has 28 thread spawns
     (research §3). A panic on the socket thread kills that thread with
     no error in Swift, and poisons its mutexes.
- **Fix:**
  1. Take the research's other option: Rust `async fn` wrappers over a
     small Rust thread pool with a one-shot `async_channel`. Swift sees
     `async throws` and blocks nothing. Cap the pool (8 threads).
  2. Implement `SyncPlayer` in a small `final class`, not isolated, that
     holds a lock-protected snapshot of the player state and a command
     queue. The engines write the snapshot; commands hop to the engine's
     queue. Write the rule in §5.1: no foreign-trait method waits for the
     main actor.
  3. A panic hook in `bloom-ffi`: log to `os_log`, store the message in a
     file, send a `Fatal` event on a subscription that Swift shows as an
     error with "Restart Bloom". Add a bridge test for it.
  4. Add to spike S1: 60 parallel calls with a 2 s server delay while a
     subscription delivers events; Thread Sanitizer on; a callback during
     a main-thread call.
  5. The session guard: put the epoch check in core's `Session` (a stale
     call returns `Stale`), not only in Swift task groups. A cancelled
     Swift task does not stop the Rust call, and its answer can still
     arrive. CLAUDE.md's rule applies: a test that fails with the guard
     removed.

### 5. "The token never crosses the bridge" cannot hold; the credential design has holes

- **Where:** spec §5.1 (last bullet), §8.1, §9.
- **What:**
  1. Core signs in (`authenticate_by_name` runs in Rust) and core sends
     the token with every request. Swift owns the Keychain. So the token
     crosses twice. AVPlayer, the background session and `MpvEngine` (all
     Swift) also need it for their headers. The sentence in §5.1 is
     false as written.
  2. **The device id is in `config.json`** **[V repo: `core/src/config.rs`]**.
     A restore from a backup to a new phone copies the config. The tokens
     stay behind (`ThisDeviceOnly`). Two phones then share one device id
     and no token. Jellyfin ties a session to the device id **[M]**.
  3. **The Keychain outlives the app.** Delete and reinstall leaves
     tokens with no config **[M]**.
  4. **Before the first unlock** after a reboot, a background launch
     reads nothing from the Keychain (`errSecInteractionNotAllowed`)
     **[M]**. Code that reads this as "signed out" forgets the profile.
- **Fix:**
  1. Replace the sentence: "The token is never logged, never in a UniFFI
     record that Swift keeps, never in a URL but the AirPlay and Cast
     ones." Give core a foreign trait `SecretStore { get, set, delete }`
     that Swift implements over the Keychain. Core calls it; `Config`
     holds no token on iOS.
  2. Keep the device id in the Keychain (`ThisDeviceOnly`). A config with
     no matching Keychain id gets a new id.
  3. A first-launch flag in `UserDefaults`: when absent, delete the app's
     Keychain items.
  4. A profile with no token shows "Sign in again", keeps its place, and
     is never removed by itself. A Keychain read that fails with
     "interaction not allowed" is "locked", not "absent".
  5. State the file protection class of `config.json` and the download
     index (`completeUntilFirstUserAuthentication`).

### 6. The network permissions are specified wrong

- **Where:** spec §8.4 (local network prompt), §12 (ATS).
- **What:**
  1. **ATS.** §12 sets `NSAllowsArbitraryLoads` "plus
     `NSAllowsLocalNetworking`". Apple: "the value of the
     NSAllowsArbitraryLoads key is ignored—and the default value of NO
     used instead—if any of the following keys are present:
     NSAllowsArbitraryLoadsForMedia, NSAllowsArbitraryLoadsInWebContent,
     NSAllowsLocalNetworking." **[V]**
     https://developer.apple.com/documentation/bundleresources/information-property-list/nsapptransportsecurity/nsallowsarbitraryloads
     With both keys, plain `http://` to a public host fails for images,
     AVPlayer and downloads.
  2. **ATS covers only Apple's stack.** Core's `ureq` and `rustls` are
     not under ATS. Browse works on `http://`; images (URLSession),
     AVPlayer and the background session fail without the exception. A
     half-working app is the worst failure to debug.
  3. **The local network prompt comes at sign-in, not at "Play on".** The
     first connect to a LAN address (`http://192.168.254.95:8096`, the
     direct address in CLAUDE.md) asks for it (research, swiftui-uikit
     §10, **[M]**). Most home users meet the prompt on the connect
     screen, before any explanation.
  4. **A denial looks like an offline server.** A denied BSD connect
     fails with "no route to host" **[M]**. `connection::classify` reads
     that as `Unreachable(Connect)`, and the app says "{server} is
     offline".
- **Fix:**
  1. Choose one ATS shape and test it: `NSAllowsArbitraryLoads` alone
     (with the review note), or `NSAllowsLocalNetworking` plus
     `NSAllowsArbitraryLoadsForMedia` if images over public `http://` are
     dropped. I recommend the first.
  2. A test matrix on a device, in phase 3: `http://` LAN IP, `http://`
     `.local` name, `http://` public host, `https://` public host,
     `https://` with a private CA (see Should fix 4). Each for browse,
     images, AVPlayer, mpv, a download.
  3. Show the sentence about the local network on the connect screen when
     the address is private or `.local`, before the first connect.
  4. Detect the denial (`NWBrowser` `.waiting` with `PolicyDenied`, or a
     probe through `NWConnection`) and show "Bloom may not use the local
     network. Allow it in Settings > Privacy & Security > Local Network",
     with a button to Settings.
  5. `NSLocalNetworkUsageDescription` names both uses: the server and the
     cast devices.

### 7. The app's behaviour outside the foreground is not specified

- **Where:** spec §8.3 "System integration" (three lines); research
  swiftui-uikit §8 has more than the spec.
- **What is missing:**
  1. **Background during playback.** Three behaviours compete: automatic
     PiP (§8.3), background audio (the background mode), and Jellyfin
     Enhanced's "Pause when the window is hidden" (inventory §3.13). The
     spec picks none.
  2. **mpv video in the background.** With audio going on and no PiP,
     mpv must stop decoding video. VideoToolbox sessions die when the app
     leaves the foreground, and an `AVSampleBufferDisplayLayer` can need
     a flush on return (`requiresFlushToResumeDecoding`) **[M]**.
  3. **The Stopped report.** Inventory §3.11: "Quitting during playback
     sends Stopped." iOS has no quit. An app killed while suspended sends
     nothing; the server keeps the session and the transcode. The spec
     needs: a progress report and a `beginBackgroundTask` flush when the
     scene goes to the background; a stop after N minutes paused in the
     background; at the next launch, a Stopped for a session that never
     ended.
  4. **A transcode that died.** The server ends an idle transcode. A
     resume after a long pause gets 404s from the playlist. The player
     must ask for PlaybackInfo again at the position.
  5. **Interruptions.** A call, Siri, an alarm: resume only when the
     system says `shouldResume`. A SyncPlay group must hear "this player
     waits" and get a rejoin after.
  6. **Polling and timers.** Quick Connect polls every 3 s. The user
     leaves the app to approve the code on the same phone; the poll must
     run once at return. The dashboard's 5 s reload and the hero's timer
     stop in the background. A pending settings save flushes ("flushed
     when the window closes" on the Mac).
  7. **The socket.** §8.4 covers SyncPlay. The same holds for "being
     controlled", server news and the offline detection: reconnect at
     `.active`, then reload what the news would have reloaded.
  8. **Network changes.** Wi-Fi to cellular mid-play; `NWPathMonitor`
     should feed `core::connection` so the app does not wait for a probe
     to time out.
- **Fix:** Add spec §8.10 "Life cycle" with a table: event (to
  background while playing / while paused / while casting / while
  downloading / in a SyncPlay group; return; interruption begins / ends;
  route lost; path change; memory warning; termination) × what each part
  does (player, reports, socket, SyncPlay, cast, downloads, polls,
  saves). Add each row as a test (finding 10). Add to S2: background and
  return, lock and unlock, an interruption, all with the mpv
  sample-buffer output.

### 8. Two engines doubles the player, and the spike is too thin to carry that choice

- **Where:** spec §8.3, Q3, Q4; plan S2 (3 days) and phase 4 (12–16 days).
- **What:**
  1. Every player feature must work twice and across a switch: segments,
     trickplay, chapters, audio and subtitle menus, timing, speed,
     quality reload, reporting, Now Playing, PiP, SyncPlay, the pause
     screen, the info overlay. The estimate does not show this.
  2. **AVPlayer cannot do several things the menus offer:** audio delay,
     subtitle delay, frame step, external `.srt`/`.ass` on a direct-play
     MP4, PGS and VobSub, a second audio track that is not in an
     alternate group **[M]**. The spec has no capability flags, and no
     rule for a subtitle chosen mid-play that the engine cannot draw
     (switch engines? burn in?).
  3. **The engine rule is circular.** "The engine is chosen per item from
     the PlaybackInfo answer (core's device profile gets an
     AVPlayer-capable variant)". The request needs a profile before the
     engine is known.
  4. **MPVUI's sample-buffer output is two days old in production**
     (Swiftfin 1.7, 2026-11-11, per the research) and has one maintainer.
     The spec has no fallback if it drops frames or loses HDR.
  5. S2 does not measure: 4K HEVC 10-bit frame drops and heat over
     30 minutes on the iPhone Air; hardware decode in this path; ASS with
     heavy typesetting and PGS (who draws them into the buffer, at what
     cost); subtitles in PiP for image tracks; 23.976 fps cadence on a
     120 Hz screen; A/V sync over AirPods; seek latency; HDR and Dolby
     Vision ("if a sample is at hand" is not a check); the life-cycle
     cases of finding 7.
- **Fix:**
  1. Narrow v1: **mpv plays everything local; AVPlayer only when the
     route is AirPlay** (over the server's HLS, as `src/airplay` does
     today). Direct play on AVPlayer (Dolby Vision 5, Atmos) is a later
     phase, added when S2's numbers show mpv's path lacks them. This
     matches "Play the file as it is" and Swiftfin's default.
  2. If Daniel keeps two engines for local play: one PlaybackInfo request
     with the mpv profile, then a local check of the chosen source
     (container, codecs, chosen subtitle) picks the engine. A second
     request only for AirPlay. Add `EngineCapabilities` to the
     `PlayerEngine` protocol; a menu entry the engine lacks is hidden.
  3. S2 grows to 5–6 days with the list above and written pass numbers
     (dropped frames under 0.5% on the 4K sample; no thermal state above
     `.fair` in 30 minutes; PiP shows the PGS track or the spec says it
     does not).
  4. Name the fallback: the Metal output without PiP.
  5. Split phase 4: 4a engine, controls, reporting, resume, life cycle;
     4b tracks, subtitles, quality; 4c segments, Up next, queue; 4d PiP,
     Now Playing, the AirPlay hand-off. Each with its own check.

### 9. Some pages have no way in

- **Where:** spec §6.1.
- **What:**
  1. **Playlists.** The Mac has a Playlists tab. The phone's tab set
     (Home, three libraries, Favorites when room, Search) has none, and
     the account sheet has no entry.
  2. **Favorites** with three libraries or more: no tab, no entry.
  3. **Libraries after the third:** "under Home's My Media row". Settings
     › Home lets the user remove My Media (inventory §1.12). Then the
     fourth library cannot be reached.
  4. **Downloads** is "pushed from the account sheet". Offline mode opens
     Downloads by itself (inventory §1.3). The spec does not say where a
     page that lives behind a sheet appears when no sheet is open, nor
     at a cold launch with no network.
  5. **Settings and Dashboard "pushed from the account sheet":** onto the
     sheet's own stack, or onto the tab's? The rule "the tab bar lights
     nothing on Settings" holds only for the first.
  6. The tab set changes with the server and the profile. HIG: "Don't
     disable or hide tab bar buttons" (research, swiftui-uikit §7).
- **Fix:** Decide one fixed structure and spike it in S3. My proposal:
  **Home, Library, Downloads, Search.** Library is a page with every
  library, Favorites, Playlists and Collections. Downloads is a real tab
  (it is the offline home). On the iPad the sidebar lists the same, with
  the libraries in a `TabSection`. Let the user pin libraries as tabs
  with `TabViewCustomization` later. S3 must also show what
  `.sidebarAdaptable` does with a section of six libraries on an iPhone
  (a "More" tab **[M]**). Add this as a question for Daniel (Q9 below).

### 10. The testing plan does not find the bugs Daniel asked about

- **Where:** spec §11; plan "The random-walk tester"; plan phase 3
  step 6 and its checks.
- **What:**
  1. **Input inside a transition.** XCUITest waits for the app to be
     idle (animations included) before each event **[M]**. I know no
     public switch. "Every other action goes without waiting" needs a
     way around that: a swizzle of the quiescence wait in the test
     runner, or events through idb as `dev/ios-ui` does today. Neither
     is in the plan.
  2. **The test hook contradicts itself.** "An accessibility element
     hidden from VoiceOver": an element hidden from accessibility is
     hidden from XCUITest too, which reads the same tree **[M]**. And a
     UI test on a device cannot read a file in the app's container.
  3. **The monkey never plays anything.** It runs "against the mock
     server only". The mock has no media and answers `/socket` with 404
     **[V repo: `dev/mock-jellyfin`]**. The player has the most gestures
     and the most state. It gets no random input at all.
  4. **Two invariants fire on correct screens.** "Every hittable element
     inside the window and the safe area": scroll content and the hero
     lie under the bars on purpose. "No hittable element under
     44 × 44 pt": the system's back button, keyboard keys, menu rows and
     links fail it. The check will be turned off within a week.
  5. **A seed does not replay a timing bug.** The shrink step assumes a
     failure repeats. Mid-transition failures depend on wall time.
  6. **The tester is the last step of phase 3**, after every page is
     written. CI runs no UI test until phase 12.
  7. **Phase 3's checks cannot be shown.** "No hitch … on the
     simulator" measures the Mac. "A 1,000-item library" does not exist
     on the mock. "The audit has no issues" fails on contrast over
     artwork and glass, with no rule for what may be ignored.
  8. Snapshot tests are "optional". No interruption tests, no fault
     injection, no small phone, no sanitizer runs.
- **Fix:**
  1. **Spike S5 (2 days): the monkey's input.** Prove one path that
     sends a tap 80 ms after a push starts. Record the choice.
  2. **Test hook:** a debug socket as today (`/tmp/bloom-ios-test.sock`
     works in the simulator) plus, for devices, a 1 pt element with an
     identifier that is not hidden, compiled out of release.
  3. **Extend the mock:** serve one short clip (`dev/make-sub-clip`'s)
     with segments, chapters and trickplay; a WebSocket on `/socket`;
     a 1,000-item and a 10,000-item library behind a flag; **fault
     modes** (delay per request, a 401 mid-session, a 404 for one item,
     a 503, a body cut short, a socket that drops). Then the monkey and
     the flows cover the player and the error states with no real
     server.
  4. **Invariants that hold:** the app runs; no crash log; the hook's
     main-thread watchdog saw no hang over 250 ms; the hook's count of
     runtime faults is zero (SwiftUI's "already presenting", "Modifying
     state during view update", "AttributeGraph: cycle", unsatisfiable
     constraints, read from `OSLogStore` in the debug build); the
     focused field is above the keyboard; after settle, no spinner
     older than 5 s on the mock; Back returns to the page before; a tap
     on a button that presents something changed the hook's state; the
     player is never under a sheet from the page behind it; memory
     (`phys_footprint` through the hook) stays under a bound after 500
     actions. Keep the 44 pt rule for the app's own controls by
     identifier prefix.
  5. **Replay with time:** log each action with its offset in ms, replay
     with the same offsets, run a failure 5 times, and report the rate.
     Keep the video of the run. Shrink only failures that repeat.
  6. **Randomise the environment per run:** text size (`-UIPreferredContentSizeCategoryName`),
     Reduce Motion, Increase Contrast, Bold Text, orientation, the
     smallest phone iOS 26 runs on (4.7 inch), an iPad in three window
     sizes, the mock's fault mode.
  7. **Interruption actions in the walk:** Home and return, the app
     switcher, lock and unlock, Control Center, a rotation during a
     push, a memory warning, the mock's socket drop, offline and back.
  8. **Snapshots are required** for a fixed matrix: 12 screens × 3
     widths × 3 text sizes, mock data, stub images. This is the only
     layer that catches "the label sits 5 points too high".
  9. **Hitches by number, on the device:** `XCTOSSignpostMetric`
     scroll metrics in `measure` on the iPhone Air, a release-optimised
     "Testing" configuration with the hook. A baseline per page; a
     regression fails.
  10. **Sanitizers:** one UI flow run with Thread Sanitizer and one with
      the Main Thread Checker, because Rust threads call into Swift.
  11. **A written device checklist per phase** for what no simulator
      shows: the Passwords key (the simulator has no saved passwords, so
      the two keyboard walks prove nothing there, as CLAUDE.md already
      says), local network prompt and denial, PiP, AirPods, a call,
      haptics, heat, HDR.
  12. Build the harness, the hook and the monkey as **step 1 of phase
      3**, and run the flows in CI from that step.
  13. An audit allow list in the repo, each entry with its reason.

### 11. Three of Daniel's design decisions are dropped without a word

- **Where:** spec §7.2, §6.2.
- **What:**
  1. **The dimming under the player's glass.** §7.2: "`.clear` with the
     35% dimming Apple asks for over bright content". Daniel rejected "a
     dark scrim over glass ('dark foreign blob')" and said the phone's
     bar "is glass over the picture, not a dark slab" (2026-10-23).
     DESIGN.md: "The Glass Shows Through Rule".
  2. **The Quiet Return Rule and the motion limits.** DESIGN.md: "Going
     back fades; only going forward slides", "no bounce, no spring",
     "nothing over 200 ms". The system's pop slides, the zoom
     transition and the sheets spring and run longer. §7.2 says the
     system's motion "replaces `ui::motion`" and calls zoom "the Arrival
     Rule done the system's way". It does not say the other rules end.
  3. **One ink as the tint.** A near-white tint changes system controls:
     a prominent button becomes white (good: the one white primary), but
     so does every one of them on a page; a selected sidebar row on the
     iPad gets a white fill under white text **[M]**; switches stay
     green (a state, acceptable); links and swipe actions lose their
     colour. The spec does not list which system controls need a style
     of their own.
  4. Smaller: Geist Mono (the Quick Connect code, logs, paths) has no
     line in §7.2. "Popups frost over on a phone" (Daniel: menus "hard
     to see over background") is left to the system; a medium-detent
     sheet over a bright poster grid is the case to look at.
- **Fix:**
  1. S3 (or S2) shows Daniel the player bar over a bright frame three
     ways: `.regular`, `.clear`, `.clear` with dimming. He picks.
  2. Write the motion change as a decision ("On iOS the system's motion
     replaces the Quiet Return Rule and the 200 ms limit") in §13, and
     amend DESIGN.md when he agrees.
  3. S3's screenshots include: a `Form` with switches and pickers, a
     prominent and a plain button, a destructive dialog, the iPad
     sidebar with a selection, a context menu, a medium sheet over
     posters. Each compared with the One Ink Rule.

### 12. The plan's order hides the risk and has no cut line

- **Where:** plan phases 1, 2, 12 and the whole sequence.
- **What:**
  1. **Phase 1 moves 8–10k lines before any consumer exists.** The
     seams (`SyncPlayer`, `Transport`, `Session`, the cast target) are
     designed for a Swift caller nobody has written. The research says
     the same about the player ("Decide this before the SyncPlay seam").
  2. **No milestone says "Daniel's phone switches".** The phases sum to
     78–114 working days before phase 12. The GPUI iOS app stays alive
     for all of them, and nothing says whether it still gets fixes.
  3. **Phase 3 ships pages without their filters.** Hidden content
     (phase 7) filters the library, the hero, search and the home rows
     (inventory §5.7). Until phase 7 the native app shows titles a
     profile has hidden.
  4. **The card menu spans five phases** (Download 5, cast and SyncPlay
     entries 6, Hide 7, metadata 9) but is listed once, in phase 3.
  5. **The iPad runs the app from phase 3** (one target, resizable), but
     its first check is phase 8.
  6. Phase 0 says the capabilities are registered; it does not list
     them. Needed: App Groups, Keychain sharing (if a widget ever needs
     it), background modes `audio`, the Live Activities and CarPlay
     entitlements only if those features stay.
- **Fix:**
  1. Move each core module **directly before the phase that uses it**:
     `Session` before phase 3; `stream`, `adaptive`, `queue` before 4;
     the download seam before 5; cast and SyncPlay before 6; the request
     blocks before 7. Each seam is then proven by its Swift caller in
     the same week. Phase 2 becomes "the bridge's rules and CI", and the
     surface grows per phase.
  2. Add **milestone M1 "the phone switches"** after phase 7: the parity
     table (below) is green for the phone, the device checklist passed,
     the migration runs, the bundle id changes back. Freeze the GPUI
     iOS code at the start of phase 3 (crash fixes only) and remove it
     at M1, not at the end. iPad, dashboard and system integration come
     after M1.
  3. Add a **parity table** file: one row per inventory item, with its
     phase, its status and "dropped on purpose" with the reason. The
     inventory is a list today, not a checklist.
  4. Put the hidden-content filter and the quality tags in phase 3 (they
     are read paths in core), and keep only their settings and menus in
     phase 7.
  5. Each phase's checks include one pass on the iPad simulator and one
     resized window, from phase 3 on.

---

## Should fix

### 1. The licence question is wider than one message

- **Where:** spec §2.
- **What:** The credits list jellyfin-web and jellyfin-chromecast
  (GPL-2.0) and IINA (GPL-3.0) as "Studied for protocols and behaviour"
  **[V repo: `src/credits.rs`]**. If any core code is a translation of
  that code (the SyncPlay engine is the candidate), an exception from
  Sarat Chandra does not cover it. Also: TestFlight is under the same
  terms as the store (the spec says so, but option 1's text does not);
  and option 4 changes §4 and §12 (no store means no reason to carry
  iOS 26).
- **Fix:** Add an audit step to phase 0: for `core/src/syncplay`,
  `core/src/cast`, `core/src/chromecast` and `core/src/playback.rs`,
  record whether the code was written from the protocol or from the
  source. Confirm that the MPVKit build is the LGPL one and that MPVUI's
  mpv patches are LGPL. Run `cargo deny` for the licences of the crates
  in `bloom-ffi`'s tree.

### 2. Carry one OS, or say what the second one costs

- **Where:** spec §4.
- **What:** Each `if #available(iOS 27, *)` is a second path to test:
  Now Playing (two APIs with different models), `AsyncImage` versus
  `ImageLoader`, `reorderable()` versus Move up/down, `MetricManager`.
  The research itself says "An iOS 27-only target is reasonable for a
  release in spring 2027 or later", and the plan ends later than that.
- **Fix:** Either set the minimum to iOS 27, or keep iOS 26 and use the
  iOS 26 API alone where both work (MediaPlayer for Now Playing, the
  `ImageLoader`, the Move menu) until the minimum moves. Add the
  question (Q10).

### 3. No policy for cellular, Low Data Mode or expensive networks

- **Where:** spec §8.3, §8.5, §8.6.
- **What:** Core's sockets ignore `allowsExpensiveNetworkAccess` and
  `allowsConstrainedNetworkAccess`; only the URLSession parts can honour
  them. A 60 GB remux over cellular starts without a question. The link
  speed test at sign-in (`/Playback/BitrateTest`) spends data on every
  socket reconnect.
- **Fix:** Settings › Downloads: "Download over cellular" (off by
  default; the session sets `allowsCellularAccess`). Settings › Playback:
  a second bitrate limit for cellular, default "Original" to keep the
  product principle, so the user must lower it. `NWPathMonitor`'s
  `isExpensive` and `isConstrained` go into core's bitrate decision as
  inputs. In Low Data Mode: no bitrate test, no hero auto-advance
  images ahead, no trickplay preload. Ask Daniel (Q11), because "Quality
  is lowered only when the user asks" is his principle.

### 4. TLS trust and IPv6-only networks

- **What:** Core trusts `webpki-roots` only **[V repo:
  `core/src/realtime.rs`, `core/Cargo.toml`]**. A server behind a
  private CA that the user installed on the phone works in Safari, in
  AVPlayer and in the image loader, and fails in core. On an IPv6-only
  network with NAT64 (carriers, and App Review tests on one), a
  hostname works; an IPv4 literal typed as the address does not when
  the code parses it without `getaddrinfo` **[M]**.
- **Fix:** Use `rustls-platform-verifier` on Apple targets (it asks the
  system's trust store). Resolve literals through `getaddrinfo`. Add
  both to the device matrix of Must fix 6.

### 5. "Localization-ready" is not true while core writes the sentences

- **Where:** spec §12 (String Catalogs), §5.1 (errors).
- **What:** The user-facing text of errors (`explain`, `explain_load`),
  toasts, the trigger descriptions, `control_label`, the language
  names, sizes and "2 min ago" comes from Rust in English.
- **Fix:** Rule in §5.1: core returns codes and values; Swift owns
  every sentence, through String Catalogs with plural rules, and
  formats dates, durations and sizes with `FormatStyle`. Check
  right-to-left once in S3 (the seek bar and the skip glyphs do not
  mirror; the layout does).

### 6. No plan for diagnostics

- **What:** `MetricManager` is iOS 27 and is about performance. Nothing
  covers: crash reports on iOS 26, Rust's `log` output, symbols for the
  Rust dylib (`strip = true` in the research's size advice removes
  them), or how a household member reports a problem.
- **Fix:** Bridge `log` to `os.Logger` with the same redaction rules.
  Build the XCFramework with a dSYM and keep `strip` for the binary
  only. Subscribe to MetricKit's crash and hang diagnostics on both OS
  versions. Settings › About: "Export diagnostics" writes the last
  hour of the app's log (`OSLogStore`), the last panic file and the
  playback info to a file for the share sheet. No analytics, and the
  privacy manifest stays as written.

### 7. A profile switch or a sign-out must clear every system surface

- **Where:** spec §8.9, §5.3 (the image cache, `@SceneStorage`).
- **What:** Spotlight entities, the widget's App Group cache, Handoff
  activities, Now Playing, the `URLCache` of posters and the stored
  navigation path all outlive a session. A family shares the device
  (PRODUCT.md). Hidden titles must never reach Spotlight or a widget.
- **Fix:** One `SessionScope` in Swift that every such store registers
  with; `cast_off`'s rule ("runs at every change of session") extended
  to these. Index only what the hidden-content filter passes. A test
  per store.

### 8. State restoration needs rules

- **Where:** spec §5.3 (`@SceneStorage`).
- **Fix:** Store the path per server and user. At restore, drop it when
  the session differs. An item that answers 404 (ids do not last,
  CLAUDE.md) pops its page with no error. Never restore into the
  player or start playback. Restore the tab and the scroll place of a
  library with infinite scroll by item id, not by offset.

### 9. The player's accessibility is missing

- **Where:** spec §7.3.
- **Fix:** With VoiceOver or Switch Control on, the controls do not
  hide. The video surface is one element, "Video", with the hint "Double
  tap to show the controls". Magic Tap (two-finger double tap) plays
  and pauses. The escape gesture closes a panel, then the player. Skip
  intro and Up next are announced when they appear. The system's
  "Closed Captions + SDH" and "Audio Descriptions" settings feed the
  default track choice, after the server's subtitle mode; write the
  order. Write the order of the three subtitle looks too: the system's
  caption style, Bloom's setting, the Enhanced plugin's style (today
  Bloom's setting overrides the plugin's; the spec adds the system's on
  top with no rule). ASS styling and image tracks ignore all three.

### 10. SyncPlay on the second engine and in the background

- **Where:** spec §8.4; plan S2 step 4.
- **What:** S2 checks AVPlayer's position at a speed that is not 1. The
  engine also needs a start at a server time, an exact seek and a ±5%
  rate. On HLS a seek lands on a segment and takes seconds. On an
  AirPlay route none of this is possible.
- **Fix:** With Must fix 8's narrow scope the question goes away for
  v1: SyncPlay runs on mpv only, and an AirPlay route in a group is
  refused with a sentence. If two engines stay, S2 measures
  `setRate(_:time:atHostTime:)` and the seek latency, and the check of
  phase 6 names both engines.

### 11. Casting with the phone asleep

- **What:** While a Chromecast or another Jellyfin client plays, the
  phone plays no audio, so iOS suspends the app. The Cast session's
  5 s ping stops, the TLS socket dies, progress reports for the Default
  Media Receiver stop, and the Lock Screen shows nothing (Now Playing
  needs local audio **[M]**). The iOS 27 remote sessions that phase 10
  plans need "app extension + APNs" (research, media §5): a push
  server, which a self-hosted client does not have. Inventory §3.15
  ("Commands go to the cast target") cannot carry over as it is.
- **Fix:** State the v1 behaviour: the cast goes on, the phone
  reconnects and reads the receiver's state at return, and the Jellyfin
  receiver app reports its own progress. Mark the Default Media
  Receiver's missing reports as a known loss. Remove "Now Playing remote
  sessions" from phase 10 or name who runs the push server.

### 12. Remote commands that do not exist on iOS

- **What:** `SUPPORTED_COMMANDS` includes SetVolume, VolumeUp,
  VolumeDown, Mute and ToggleFullscreen (inventory §4.3). An iOS app
  cannot set the system volume, and has no full screen.
- **Fix:** Core takes the command list from the platform. iOS
  advertises mute (the player's own) and drops the others, or maps
  volume to the player's own gain; choose one.

### 13. Storage and memory

- **What:** `free_space` reads `statvfs` **[V repo:
  `core/src/downloads/engine/space.rs`]**. On iOS that number leaves out
  the space the system can purge; `volumeAvailableCapacityForImportantUsage`
  is the number Apple gives for this **[M]**. mpv's demuxer cache
  defaults to 150 MB forward and 50 MB back **[M]**, which is a large
  share of what iOS lets an app hold.
- **Fix:** A foreign function for free space on iOS. Caps on mpv's
  cache for a phone, set from `os_proc_available_memory`. On a memory
  warning: drop the decoded image cache and the trickplay sheets. The
  download page explains a "disk full" stop with the amount needed.

### 14. HDR, external screens, heat, Low Power Mode

- **What:** The spec names Dolby Vision once. Missing: the device
  profile must take the HDR ranges from the device
  (`AVPlayer.eligibleForHDRPlayback`), not from a table; a wired
  screen (USB-C to HDMI) mirrors the phone unless the app gives it a
  scene; under a serious thermal state or Low Power Mode the app
  should stop the hero's auto-advance and the trickplay preload.
- **Fix:** A short §8.3 paragraph for each, and one line in S2 (HDR on
  the sample-buffer layer: does the system tone-map, or does the frame
  clip?). An external-screen scene can wait; say so.

### 15. Small API points to settle in the spec

1. **How the player is presented** is not written: a `fullScreenCover`,
   or a window-level overlay. It decides how PiP restores, how the
   rotation request works (SwiftUI needs the app delegate's
   `supportedInterfaceOrientationsFor` plus `requestGeometryUpdate`
   **[M]**) and how the home indicator hides
   (`persistentSystemOverlays(.hidden)`). Add it to S3, on an iPhone
   and an iPad, because the geometry request "windowed" the iPad app
   in the GPUI port.
2. **What Back does to local playback.** Today Back stops the player.
   The bottom accessory shows the cast target, SyncPlay or downloads,
   but not the local film. iOS users expect swipe-down to keep it
   playing in PiP. Decide (Q12).
3. **Double tap delays single tap** by about 300 ms (the single tap
   waits for the double to fail). Daniel's rule is "a tap toggles the
   controls"; he should feel the delay in S2 before it is fixed.
4. **New-password fields** (Change password, New user): `.newPassword`
   brings the strong-password sheet. Daniel disliked the Passwords key
   on the username field; ask before typing these.
5. **Profile picture and branding image upload** need `PhotosPicker`
   and `fileImporter`. Neither is in the spec. No permission prompt is
   needed for `PhotosPicker`.
6. **Random sort with infinite scroll** returns repeats: the server
   shuffles again for each page **[M]**. Load Random as one fixed page,
   or keep the pager for it.
7. **`UIApplicationSupportsMultipleScenes`**: set it to false for v1.
   With it on, the iPad can open two windows and two players on one
   audio session; §6.4 lists several windows as "later".
8. **Dark only** needs `UIUserInterfaceStyle = Dark` in Info.plist, or
   the launch screen, system alerts, the Safari sheet and the share
   sheet follow the system. Widgets render in the system's modes
   (tinted, clear); "dark only" does not hold there, so say so.
9. **The deployment target in the build script**: the research keeps
   `IPHONEOS_DEPLOYMENT_TARGET=17.0` from `dev/ios`. Set 26.0 for the
   Rust build too.
10. **The XCFramework must hold a real `.framework`** (Info.plist,
    `MinimumOSVersion`, signed), not a bare dylib; the store refuses
    loose dylibs **[M]**. S1 should build the store shape.
11. **The device's name**: `UIDevice.current.name` is "iPhone" without
    an entitlement since iOS 16 **[M]**. The dashboard's device list
    then shows several "iPhone". Let the user name the device in
    Settings › Profile.
12. **Haptics** are not in the spec. Use `sensoryFeedback` for: a
    favourite or played toggle, a scrub that crosses a chapter, a
    download that completes, a failed action, a skip.

### 16. App Store points the list in §12 lacks

1. **The name.** Store names are unique. "Bloom" is taken many times
   over **[M]**. A name with "Jellyfin" in it needs a look at
   Jellyfin's trademark rules.
2. **A privacy policy URL** is required for every app, also one that
   collects nothing **[M]**.
3. **The demo server** must be reachable by the reviewer for the whole
   review, from the internet. "The container" on the Mac is not. Host
   one, or name Jellyfin's public demo server as the backup.
4. **The age rating**: user-chosen servers and web content in the
   Safari sheet usually mean 17+ **[M]**. Decide before the
   questionnaire.
5. **Seerr** in review notes and screenshots: leave it out; it reads
   as a download tool.
6. **Export compliance**: `rustls` and `ring` are not the system's
   crypto. The spec flags it; the research note says the Chromecast
   pinning "uses the system TLS stack", which is wrong **[V repo]**.
   Correct the note.
7. **Versioning and the release path**: one marketing version from the
   Cargo workspace; a build number that only rises; `dev/release-ios`
   (archive, export, upload); who holds the distribution certificate;
   TestFlight builds end after 90 days **[M]**, which matters if
   TestFlight is the household's channel.

---

## Consider

- **Quick actions** on the app icon (Search, Continue watching) and a
  **Control Center control** for play/pause cost a day and read as
  native.
- A **local notification** when a download ends. It needs the
  notification prompt; ask only at the first download.
- **`ShareLink`** for an item (the Jellyfin Web URL), since "Open in
  Jellyfin Web" stays.
- **A sleep timer** is the most common request on media apps. It is
  not on the Mac, so it is out by the non-goals; note it for after
  parity.
- **Hold to play at 2×** and **swipe down to close the player**: both
  common on iOS; neither is in the system player on a phone. Leave them
  out unless Daniel asks.
- **The Dynamic Island** comes with Now Playing. A Live Activity for a
  SyncPlay group is possible later; not for v1.
- **Proxies, per-app VPN and on-demand VPN**: sockets from Rust do not
  start an on-demand VPN **[M]**. Daniel reaches the server over
  Tailscale at times; test "Tailscale off, on-demand rule on" once.
- **UniFFI cost per call**: an `Item` has about 50 fields; a page of
  100 is 5,000 fields through a buffer. Measure once in S1 with a
  10,000-item title index. If the index stays in Rust and Swift asks
  with a query, this is no problem.
- **A leak test for subscriptions**: a Swift task that awaits `next()`
  keeps its model alive. Each subscription needs `close()`, called when
  the session ends.
- **BoltFFI** is the research's "re-check in 2027". The plan runs into
  2027; put the re-check in phase 11.
- **Stage Manager and the menu bar** are covered in phase 8. Add
  drag and drop of a poster into a playlist as a later idea only.
- **CI**: `ios-build` "has not run yet" (CLAUDE.md). Check that the
  `macos-26` runner has Xcode 27 before phase 2 depends on it.

---

## Missing from the inventory or from parity

Items in inventory.md with no line in the spec or no phase in the plan:

| Inventory | Item | State in the spec |
|---|---|---|
| §1.4, §1.10 | Playlists: how a phone reaches them | No way in (Must fix 9) |
| §1.9 | Favorites with three libraries or more | No way in |
| §1.3 | Offline mode opens Downloads by itself | Downloads is behind a sheet |
| §1.12 Profile | Profile picture add, change, remove | Not named; needs `PhotosPicker` |
| §1.12 Profile | Change password | Not named; field types open |
| §1.12 Downloads | "At the same time: 1 or 2" | Conflicts with the background session |
| §1.12 Downloads | Location "Show in Finder" | Dropped without a word; decide whether the Files app sees the folder (no) |
| §1.12 Display | Quality tags switch, per-feature Enhanced switches | By reference only |
| §1.13 Branding | Upload image | Needs `fileImporter` |
| §2 | "Remove from Continue Watching / Next Up" | By reference only |
| §2 | Seerr card menu | Phase 7 by reference |
| §3.2 | Previous: restart, or previous item in the first 5 s | By reference only |
| §3.8, §3.9 | Audio delay, subtitle timing | Not possible on AVPlayer |
| §3.9 | "Search for subtitles…" writes to the server | Not in the deny list |
| §3.10 | The transcode ends "on stop and on quit" | No quit on iOS (Must fix 7) |
| §3.11 | "Quitting during playback sends Stopped" | Same |
| §3.13 | Pause when hidden, resume on return | Conflicts with PiP and background audio |
| §3.13 | Frame step, aspect cycle, jump to percentage | Keyboard only; mpv only |
| §3.15 | Media keys go to the cast target | Not possible on iOS 26 (Should fix 11) |
| §4.2 | Session list poll every 2 s or 15 s | Stops in the background; not written |
| §4.3 | Volume and full-screen commands | Not possible (Should fix 12) |
| §4.7 | News reloads Home and Library | With infinite scroll, a reload must keep the place |
| §1.2 | Link speed test at sign-in | Data cost on cellular |
| §1.2 | The server's splash image and login disclaimer on sign-in | "As today" only |
| §5.6 | Offline detail page and images from the download folder | The `ImageLoader` must read local files |
| §5.2 | "Popups frost over fully on a phone" | Left to the system; check in S3 |
| §7 | Geist Mono for the code, logs and paths | No line |
| §7 | The Quiet Return Rule, no spring, 200 ms | Dropped silently (Must fix 11) |

Platform expectations with no line anywhere: scene phases (Must fix 7),
cellular and Low Data Mode, TLS trust, storage pressure and memory
warnings, HDR and external screens, thermal state and Low Power Mode,
haptics, the player under VoiceOver, diagnostics and crash reports,
localization of core's text, versioning and the release path, the
store name and the privacy policy.

---

## Questions the spec should add for Daniel

| # | Question | My recommendation |
|---|---|---|
| Q9 | Tabs: per-library tabs that change with the server, or a fixed set (Home, Library, Downloads, Search)? | Fixed set; pin libraries later |
| Q10 | Minimum iOS 26 with forks, or iOS 27 alone? | 27 alone if the store is not in scope before mid-2027; else 26 with the iOS 26 API used on both |
| Q11 | Cellular: a separate bitrate limit and "Download over cellular"? Your principle says quality is lowered only when the user asks. | Both settings; streaming default stays Original, downloads default to Wi-Fi only |
| Q12 | The app goes to the background while a film plays: PiP, sound only, or pause? And what does swipe-down or Back do in the player: stop, or keep playing in PiP? | Automatic PiP when the system allows it, else pause; Back stops, as today |
| Q13 | The player bar over a bright picture: clear glass with Apple's dimming, or no dimming? You rejected a dark scrim over glass on the Mac. | Decide from three screenshots in the spike |
| Q14 | On iOS the system's motion replaces "going back fades", "no spring" and the 200 ms limit. Accept? | Yes; amend DESIGN.md |
| Q15 | When does your phone switch to the native app: at parity for the phone (after phase 7), or later? Does the GPUI iOS app get fixes until then? | At parity; crash fixes only |
| Q16 | For v1, is AVPlayer only for AirPlay (mpv plays everything else), or also for direct play of MP4 with Dolby Vision and Atmos? | AirPlay only in v1 |
| Q17 | The licence audit: was the SyncPlay and Cast code written from the protocol or from jellyfin-web's source? | Record it before the message to Sarat Chandra |
| Q18 | New-password fields: allow the system's strong-password offer? | Yes for New user and Change password |
| Q19 | May the app index titles in Spotlight and show them in a widget on a phone the family shares? | Off by default; a switch in Settings |
| Q20 | The store name, since "Bloom" alone is taken | Decide with Q1 |
