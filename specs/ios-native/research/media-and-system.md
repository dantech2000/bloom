# Native iOS Bloom: what Apple recommends for media apps (as of 2026-11-13)

Research for a SwiftUI/UIKit rewrite of Bloom on iOS 26/27.

**Markers.** **[V]** means I read the claim in the cited page during this research (Apple docs were read through their `.md` endpoints, e.g. `developer.apple.com/tutorials/data/documentation/...md`). **[M]** means it comes from memory, which may be older than iOS 27, and should be checked. **[R]** means a press report, not Apple.

---

## The big news since iOS 26 (read this first)

Four iOS 27 additions change the plan. All four are new since my training data.

1. **The Now Playing framework (iOS 27).** This is a Swift framework that replaces `MPNowPlayingInfoCenter` + `MPRemoteCommandCenter` for new code. You give it an `@Observable` model that conforms to `MediaSessionRepresentable`. It has `MovieContent` and `TVShowContent` types and "remote sessions" for playback on other devices. Apple warns: "Don't mix the Now Playing framework with the MPNowPlayingInfoCenter and MPRemoteCommandCenter APIs … for local playback. Doing so results in undefined behavior." [V] https://developer.apple.com/documentation/nowplaying, https://developer.apple.com/documentation/nowplaying/publishing-media-sessions, WWDC26 312 https://developer.apple.com/videos/play/wwdc2026/312
2. **AVSystemRouting + MediaDevice (iOS 27).** These put third-party cast protocols in the system device picker (the AirPlay sheet). A *protocol provider* ships a Media Device extension in an app whose "sole purpose must be the delivery and installation of the media device extension". A *media app* such as Bloom observes `AVSystemRouteController` and starts an `AVSystemRouteSession(url:mode:)` on the chosen route. [V] https://developer.apple.com/documentation/avsystemrouting, https://developer.apple.com/documentation/mediadevice, https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.media-device-extension. The press reports that Google Cast will come this way, possibly EU-only (DMA). [R] https://9to5mac.com/2026/05/24/ios-27-google-cast-third-party-streaming-integration-eu/. I found no Apple statement on regions and no Google Cast extension.
3. **AVKit's `AVPlaybackUserInterfaceControllable` (iOS 27).** This protocol suite covers playback, timeline segments, media selection, volume and metadata. It is the control surface of a remote route (`AVSystemRouteMediaSession.playbackControl`). Apple frames it as a way to "build custom transport controls that work with third-party casting services". [V] https://developer.apple.com/documentation/avkit/third-party-casting-support. I found **no** API that hands such an object to `AVPlayerViewController`.
4. **CarPlay video apps (iOS 27).** CarPlay apps can browse video, but "CarPlay video apps need to support AirPlay video streaming". Video plays only when the car is parked. Otherwise "your video will be played as audio-only." Two entitlements (audio and video) let the app appear in every car. [V] WWDC26 212 https://developer.apple.com/videos/play/wwdc2026/212

Smaller changes: on-device **generated subtitles** (iOS 27, English from English audio, translation from English subtitles) need no code during playback. A **subtitle style preview** works through `AVPlayerLayer.setCaptionPreviewProfileID(_:position:text:)` or `AVLegibleMediaOptionsMenuController` (iOS 26.4+). [V] WWDC26 256 https://developer.apple.com/videos/play/wwdc2026/256, https://developer.apple.com/documentation/avkit/avlegiblemediaoptionsmenucontroller. A **Swift-first MetricKit** (`MetricManager`) is new as well, see §10.

---

## 1. Video playback UI

**What the system player gives.** `AVPlayerViewController` and SwiftUI `VideoPlayer` need an `AVPlayer`. Apple's overview: the player "connects to the system features that surround playback, from Now Playing to Picture in Picture and AirPlay … As the system player gains new behavior, your app picks it up." [V] https://developer.apple.com/documentation/avkit/avkit-playback-interfaces. Subclassing is not supported [V] https://developer.apple.com/documentation/avkit/avplayerviewcontroller. In iOS 26 the system controls take the Liquid Glass look on their own; I did not find an AVKit-specific iOS 26 change list [M].

What it has on iOS, from the class topics [V] (same URL):
- Subtitles and audio menus come from `AVMediaSelectionGroup`s of the item. You can limit them with `allowedSubtitleOptionLanguages` and `requiresFullSubtitles`. WWDC26 256 says it "fully implements subtitle selection and player controls" plus the style preview [V].
- Speeds (`speeds`, `selectSpeed`), skip behaviour (`isSkipForwardEnabled`, `skippingBehavior`), PiP (`allowsPictureInPicturePlayback`, `canStartPictureInPictureAutomaticallyFromInline`), `updatesNowPlayingInfoCenter`, `preferredDisplayDynamicRange`, `contentOverlayView`.
- **`contextualActions`** (a "Skip Intro" button): the doc says "such as showing a Skip Intro button during a title sequence … The view controller presents contextual actions only when the transport bar isn't visible." The docs now list iOS 8.0+ for it. That looks like inherited class availability. My memory is tvOS 15 + visionOS only [M]. **Test it on iOS 26/27 before you rely on it.** https://developer.apple.com/documentation/avkit/avplayerviewcontroller/contextualactions
- **`transportBarCustomMenuItems` is tvOS 15+ only** [V] https://developer.apple.com/documentation/avkit/avplayerviewcontroller/transportbarcustommenuitems. `customInfoViewControllers`, `infoViewActions`, content proposals and the Chapters panel are listed under "Customizing the tvOS player UI" and "visionOS" [V].
- Chapters: `AVNavigationMarkersGroup` + `AVPlayerItem.navigationMarkerGroups`. Interstitials: `AVInterstitialTimeRange` + `interstitialTimeRanges`. The class doc describes both as tvOS player features [V]. On iOS the default controls show no chapter panel, as far as I know [M].

**A custom renderer with system controls?** No. Every system-player entry point takes an `AVPlayer` (`AVPlayerViewController.player`, `VideoPlayer(player:)`, `AVLegibleMediaOptionsMenuController.init(player:)`) [V]. The iOS 27 `AVPlaybackUserInterfaceControllable` protocol looks like a step toward source-agnostic controls, but its documented consumers are remote routes only [V]. **Alternative:** custom SwiftUI controls (what Bloom has now), with these pieces from the system:
- `AVPictureInPictureController.pictureInPictureButtonStartImage`/`StopImage` for the PiP glyph [V] https://developer.apple.com/documentation/avkit/adopting-picture-in-picture-in-a-custom-player
- `AVRoutePickerView` for AirPlay (§4)
- `MACaptionAppearance*` (MediaAccessibility) to honour the user's subtitle style [V] (WWDC26 256)
- The HIG says: "If your app truly requires a custom video player, reference the behavior and interface of the system video player … A custom experience that diverges slightly … can cause frustration." It also asks for Space = play/pause on a hardware keyboard. [V] https://developer.apple.com/design/human-interface-guidelines/playing-video

## 2. Picture in Picture with a custom renderer

**Apple's path.** `AVPictureInPictureController(contentSource: .init(sampleBufferDisplayLayer:playbackDelegate:))` (iOS 15+) [V] https://developer.apple.com/documentation/avkit/avpictureinpicturecontroller/contentsource-swift.class. The delegate answers `setPlaying`, `timeRangeForPlayback`, `isPlaybackPaused`, `didTransitionToRenderSize`, `skipByInterval`, `shouldProhibitBackgroundAudioPlayback` [V] https://developer.apple.com/documentation/avkit/avpictureinpicturesamplebufferplaybackdelegate. Frames go into `AVSampleBufferDisplayLayer.sampleBufferRenderer` (an `AVSampleBufferVideoRenderer`, iOS 17+), which "allows you to safely enqueue sample buffers from a background thread" [V] https://developer.apple.com/documentation/avfoundation/avsamplebufferdisplaylayer/samplebufferrenderer. Drive timing with the layer's `controlTimebase` [M].

**Requirements** [V] https://developer.apple.com/documentation/avfoundation/configuring-your-app-for-media-playback:
- `AVAudioSession` category `.playback` (mode `.moviePlayback` for films). Activate it when playback starts, not at launch.
- Background mode "Audio, AirPlay, and Picture in Picture" (`UIBackgroundModes` = `audio`).
- "Only begin PiP playback in response to user interaction and never programmatically. The App Store review team rejects apps that fail to follow this requirement." Automatic start from inline on backgrounding is the allowed exception [V] (custom-player article above).
- Implement `restoreUserInterfaceForPictureInPictureStop…`. Otherwise, by default, returning from PiP ends playback [V].

**A CAMetalLayer cannot go into PiP.** mpv's `gpu-next` → MoltenVK → `CAMetalLayer` output, Bloom's current iOS path, cannot be a PiP source. A PiP-capable mpv build must either output sample buffers or copy each rendered frame into a `CVPixelBuffer` and enqueue it.

**How others do it:**
- **Swiftfin** (official Jellyfin Swift client, v1.7.0 released 2026-11-11, iOS 18+/tvOS 26+, SwiftUI, 900 Swift files) [V via GitHub API]. It dropped VLCKit. **MPV is now the default player and AVPlayer the alternative.** Their doc: "MPV is the default and is the recommended choice for the broadest compatibility". PiP works on both. "AirPlay video hand-off" works only with AVPlayer ("MPV … screen mirroring only"). MKV/WebM/TS play only in MPV. DTS/TrueHD/Opus need a transcode under AVPlayer. PGS/DVD subtitles need server burn-in under AVPlayer. DV profile 7.6 is not supported in MPV and only partly in AVPlayer. [V] https://github.com/jellyfin/Swiftfin/blob/main/Documentation/players.md
- Swiftfin's mpv wrapper is **MPVUI** (github.com/LePips/MPVUI, MIT, active Nov 2026). It has two Apple outputs. **Metal:** mpv → libplacebo → MoltenVK → `CAMetalLayer`, for shaders and tone mapping. **Sample buffer:** "mpv → AVFoundation → `AVSampleBufferDisplayLayer`", for "native presentation, supported Dolby Vision metadata, and iOS PiP". "Sample-buffer output is the default … iOS picture in picture requires sample-buffer output." [V] https://github.com/LePips/MPVUI/blob/main/Build/RENDERING.md. The AVFoundation VO is an mpv patch (`0005-avfoundation-native-video`: "sample-buffer timing, seeking, and subtitle composition"). Other patches add AC-3/E-AC-3 passthrough via AVPlayer, DV P7→8.1 conversion, Spatial Audio, and plain-text subtitle interception so the app can draw subtitles over PiP [V] https://github.com/LePips/MPVUI/blob/main/Build/PATCHES.md. **This is the closest prior art for Bloom. Read it before you write your own.**
- **VLCKit 4.0** NEWS lists PiP on iOS and macOS, done through a libVLC Apple vout module [V] https://code.videolan.org/videolan/VLCKit/-/raw/master/NEWS, https://mailman.videolan.org/pipermail/vlc-commits/2024-September/070418.html.
- **KSPlayer** is "based on AVPlayer and FFmpeg", lists PiP as free, and is GPL by default (LGPL or commercial for a fee) [V] https://github.com/kingslay/KSPlayer. I found no public source for Infuse's internals.

## 3. Hybrid: AVPlayer where it can, mpv for the rest

**What AVFoundation plays** (from the HLS authoring spec, the best formal list) [V] https://developer.apple.com/documentation/http-live-streaming/hls-authoring-specification-for-apple-devices:
- Video: H.264 (≤ High 6.0 on iOS), HEVC (≤ Main10 L5.1 High tier, fMP4 only), AV1 (≤ L6.2, fMP4), Dolby Vision. "HDR HEVC video MUST be HDR10, HLG, or Dolby Vision". DV on iOS: Profile 5 or Profile 10 (AV1) ≤ L9 as the main codec. Profiles 8.1/8.4/10.1/10.4 go in as `SUPPLEMENTAL-CODECS` over an HDR10/HLG base.
- Audio: AAC, AC-3, E-AC-3, E-AC-3 JOC (Atmos, `CHANNELS="16/JOC"`), ALAC, FLAC, APAC. **No DTS, no TrueHD.**
- Subtitles in HLS: WebVTT or IMSC1 (text only), CEA-608/708. **No PGS, VobSub or ASS styling.**
- Containers: fMP4 or MPEG-TS (H.264 only) for HLS. Files: MP4/MOV. **MKV is not supported** [M, matches Swiftfin's table V].

So the split Swiftfin made is a sound one. Put HLS transcodes and direct MP4/MOV with H.264/HEVC/AV1 + AAC/(E-)AC-3 on AVPlayer: you get AirPlay video, system PiP, system subtitles and Atmos. Keep MKV, PGS/VobSub, ASS, DTS/TrueHD and DV P7 on mpv. Jellyfin's device profile decides per item. A caveat: in Swiftfin's table, Dolby Vision playback through mpv's sample-buffer output works only with MPVUI's patches [V].

**Lock screen / Control Center:**
- New code on iOS 27: Now Playing framework (`MediaSession` + `MovieContent`/`TVShowContent`, `MediaPlaybackSnapshot` with elapsed time + timestamp, commands such as `.seekToPosition`). Call `requestToBecomeSystemPrimary()` in the foreground. [V] (links above). Apple's docs do not say how this framework coexists with `AVPlayerViewController.updatesNowPlayingInfoCenter`. Pick one owner per playback.
- iOS 26 and older: `MPNowPlayingInfoCenter` + `MPRemoteCommandCenter` [V] https://developer.apple.com/documentation/mediaplayer/mpnowplayinginfocenter. `AVPlayerViewController.updatesNowPlayingInfoCenter` (default true) publishes for an AVPlayer automatically [V class topics; default value M]. Since iOS 16, `MPNowPlayingSession` ties players to a session. The custom-PiP article tells tvOS apps to "migrate away from `MPNowPlayingInfoCenter.default()`" when they use sessions [V].
- mpv has no automatic path. Publish the metadata yourself.
- `AVAudioSession`: `.playback` + `.moviePlayback` [V]. Watch `interruptionNotification` and `routeChangeNotification` (pause on `.oldDeviceUnavailable` when headphones are pulled) [M]. Watch `silenceSecondaryAudioHintNotification` so audio does not mix across PiP [V HIG].
- Screen sleep: `AVPlayer.preventsDisplaySleepDuringVideoPlayback` defaults to true on iOS [V] https://developer.apple.com/documentation/avfoundation/avplayer/preventsdisplaysleepduringvideoplayback. With mpv, set `UIApplication.shared.isIdleTimerDisabled = true` while video plays and reset it after [M].

## 4. AirPlay

- **Picker:** `AVRoutePickerView` (UIKit; Apple's own sample wraps it in `UIViewRepresentable`; there is no native SwiftUI view). Set `prioritizesVideoDevices = true` for a video app. Use `AVRouteDetector.multipleRoutesDetected` to hide the button when nothing is nearby. [V] https://developer.apple.com/documentation/avkit/avroutepickerview, https://developer.apple.com/documentation/avfoundation/avroutedetector
- **AVPlayer:** `allowsExternalPlayback` defaults to true [V] https://developer.apple.com/documentation/avfoundation/avplayer/allowsexternalplayback. The Apple TV fetches the URL itself (true AirPlay video), and the phone becomes a remote. The background audio mode enables this [V].
- **Custom renderer (mpv):** no AirPlay video hand-off. The user can only mirror the screen, or the audio goes over AirPlay. Swiftfin documents the same limit [V]. A practical answer: when the user picks an AirPlay route, switch to AVPlayer over an HLS transcode URL from Jellyfin.
- **HLS rules that touch AirPlay** [V spec]: subtitles MUST be WebVTT. If several video streams are present (H.264, HEVC, HDR), "each stream MUST provide all anticipated bandwidths". When you AirPlay downloaded HLS, "the AirPlay receiver will attempt to load the original Multivariant Playlist URL". AirPlay of a *download* therefore needs the server online.
- AirPlay as a service needs no local-network permission ("AirPlay: no") [V] TN3179.

## 5. Google Cast on iOS

- **Your own Cast v2 implementation vs Google's SDK.** The Cast iOS SDK handles discovery, sessions and the `GCKUICastButton`. Swiftfin uses it (`GoogleCast`, `kGCKDefaultMediaReceiverApplicationID`) [V Swiftfin source]. Your own implementation keeps one code path with Rust core and skips the SDK, but you must do discovery and TLS yourself.
- **Permissions** [V] https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy:
  - `NSLocalNetworkUsageDescription`, plus `NSBonjourServices` with `_googlecast._tcp` (Google also asks for `_<APPID>._googlecast._tcp`; Swiftfin lists `_CC1AD845._googlecast._tcp`) [V] https://developers.google.com/cast/docs/ios_sender/permissions_and_discovery.
  - Browsing and resolving a **listed** Bonjour type needs local-network access but **not** the multicast entitlement. `com.apple.developer.networking.multicast` is required only to send or receive raw UDP multicast/broadcast, to work "with arbitrary Bonjour service types", or to browse all types. Google adds that Apple "will not be granting multicast entitlements solely for the purpose of supporting Cast."
  - The TCP/TLS connection to the Chromecast (port 8009) is a local-network operation and needs the permission.
  - In the background with an undetermined state, the system denies without a prompt. The simulator "doesn't support local network privacy". Test on a device.
- **Discovery API:** `NWBrowser(for: .bonjour(type: "_googlecast._tcp", domain: nil), …)`. Check `.waiting(.dns(kDNSServiceErr_PolicyDenied))` for denial [V TN3179]. Bloom's `dns_sd` C calls are covered by the same rules [V TN3179: "dnssd, Network framework"].
- **iOS 27 alternative:** Chromecasts may appear in the system picker through a Google-shipped Media Device extension. Bloom would then use `AVSystemRouteSession(url:mode: .player)` and `playbackControl`. That is Apple-blessed, but it depends on Google shipping the extension, possibly in the EU only [V API, R availability]. Keep your own implementation.
- Lock-screen control of a Chromecast: Now Playing **remote sessions** (`RemoteMediaSession`, app extension + APNs) [V], or `MPNowPlayingInfoCenter` per the AVSystemRouting article [V].

## 6. Downloads

- **Background `URLSession`** [V] https://developer.apple.com/documentation/foundation/downloading-files-in-the-background:
  - Use one session from `URLSessionConfiguration.background(withIdentifier:)`, with `sessionSendsLaunchEvents = true`.
  - Recreate it with the same identifier at launch, store the handler from `application(_:handleEventsForBackgroundURLSession:completionHandler:)`, and call it in `urlSessionDidFinishEvents`. SwiftUI has `.backgroundTask(.urlSession(...))` [M].
  - Move the file inside `didFinishDownloadingTo` before it returns.
  - Limits: a delegate is required, HTTP(S) only, redirects are always followed. A rate limiter grows the delay each time the app is relaunched to queue the *next* download, so "start many download tasks at once". Tasks started in the background are treated as discretionary.
  - Set `countOfBytesClientExpectsToReceive`.
- **Background Assets is the wrong tool.** "Use the Background Assets framework only to download additional assets for your app; don't use it for any other purposes." It targets app asset packs (Apple-hosted or self-hosted manifests). [V] https://developer.apple.com/documentation/backgroundassets
- **`BGContinuedProcessingTask` (iOS 26)** must start in the foreground from a user action. The system shows it as a Live Activity with progress and Cancel. It may use the network, but Apple's examples are exports and processing. Closing the app from the switcher cancels it. [V] https://developer.apple.com/documentation/backgroundtasks/performing-long-running-tasks-on-ios-and-ipados. For plain file downloads, a background `URLSession` survives app termination and is the better fit. A continued-processing task suits a *client-side* step, e.g. remuxing a download. Forum threads report launch-handler and lock-screen pauses on 26.0 [V threads, unconfirmed by Apple] https://developer.apple.com/forums/thread/801126.
- **Live Activity for download progress:** acceptable under the HIG ("tasks and events that have a defined beginning and end … that don't exceed eight hours"; end it right away; no ads) [V] https://developer.apple.com/design/human-interface-guidelines/live-activities. Your app must update it while suspended, and a background `URLSession` only wakes the app at completion. A progress bar would stall. Options: update it at task completion only, or use push updates [M].
- **Storage:** put media in `Application Support` (or `Library/Caches` if the system may purge it) and set `URLResourceValues.isExcludedFromBackup = true` [M]. For downloaded HLS use `AVAssetDownloadURLSession` (system-managed location) [M].

## 7. Background behaviour

- With `audio` background mode and an active `.playback` session, the process keeps running while media plays. That covers background audio and PiP [V configuring-your-app]. When playback stops, the app suspends within seconds [M].
- **SyncPlay websocket:** it stays up while the app plays in the background or in PiP, because the process runs [M]. Once paused or suspended, sockets close. iOS gives no mode for keeping a socket alive. Reconnect and rejoin on foreground. Do not count on `BGAppRefreshTask` (minutes-scale, opportunistic) [M].
- `BGContinuedProcessingTask` is for user-started finite work with progress, not for sockets [V].

## 8. System integration

- **App Intents / Siri / Shortcuts:** WWDC26 345 adds `LongRunningIntent` (past 30 s, Live Activity progress), `EntityCollection`, `SyncableEntity` (stable server IDs across devices: Jellyfin item IDs qualify) and `RelevantEntities` [V] https://developer.apple.com/videos/play/wwdc2026/345. WWDC26 240 names App Schema domains for messages, mail and photos. I found no video domain [V] https://developer.apple.com/videos/play/wwdc2026/240. Good intents: "Play next episode", "Resume <show>", "Search library".
- **Spotlight:** expose items as `IndexedEntity` (App Intents) or `CSSearchableItem`. WWDC26 345 calls Spotlight indexing "the primary way Siri retrieves your content" [V]. Swiftfin indexes only the app itself [V source].
- **Widgets:** a WidgetKit "Continue watching" widget fed from an App Group cache. WWDC26 277 covers the basics [M content].
- **Live Activities:** fine for downloads and maybe a SyncPlay session. Avoid playback progress: Now Playing already covers it.
- **Handoff:** `NSUserActivity` with the item ID and position. Associated domains also back Handoff [V] https://developer.apple.com/documentation/xcode/supporting-associated-domains.
- **TV app:** gated by the Video Partner Program, aimed at "subscription premium video entertainment" apps with catalog feeds. **Not open to a self-hosted client.** [V summary] https://developer.apple.com/programs/video-partner/
- **Universal Links:** each user's server is a different host, so `applinks` applies only to a domain you own (e.g. a redirect/share domain). Custom URL schemes (Swiftfin registers `jellyfin://`) [V Swiftfin plist].
- **Focus filters** (`SetFocusFilterIntent`): optional, low value [M].
- **CarPlay:** audio-only before iOS 27. iOS 27 adds video apps for parked cars, which **require AirPlay video**, so only the AVPlayer path qualifies [V WWDC26 212].
- **Apple Watch remote:** the Now Playing card on Watch comes free once Now Playing is published [V NowPlaying overview].

## 9. Credentials

- **Keychain:** store access tokens as `kSecClassGenericPassword` with `kSecAttrAccessibleAfterFirstUnlock` ("recommended for items that need to be accessed by background applications"; migrates in encrypted backups) [V] https://developer.apple.com/documentation/security/ksecattraccessibleafterfirstunlock. Add `…ThisDeviceOnly` to keep it off other devices [M]. Background downloads and the reporter need the token while the phone is locked, so do not use `WhenUnlocked`.
- **Password AutoFill / shared web credentials:** needs `webcredentials:<domain>` in the associated-domains entitlement and an AASA file served over HTTPS with "no redirects", via Apple's CDN [V] https://developer.apple.com/documentation/xcode/supporting-associated-domains. For arbitrary self-hosted servers this cannot work generally. Type the fields with `.textContentType(.username/.password)` so iCloud Keychain can still save and offer them [M]. This matches the field-typing lessons in Bloom's CLAUDE.md.
- **Quick Connect:** the new device shows a 6-character code. A signed-in device authorizes it. On by default on the server. Clients must keep the code screen open. [V] https://jellyfin.org/docs/general/server/quick-connect/. On iOS, offer it as the first option on the sign-in card. Also add an "Authorize a device" screen in Settings: Swiftfin tvOS cannot authorize, so an iPhone app that can is useful [V same page].

## 10. Performance

- **SwiftUI instrument (Instruments 26):** lanes for Update Groups, Long View Body Updates, Long Representable Updates and Other Long Updates, plus a Cause & Effect Graph. Advice: keep `body` cheap (no formatter creation), give views narrow `@Observable` dependencies, keep changing values out of the Environment. [V] https://developer.apple.com/videos/play/wwdc2025/306/
- **Instruments 27 (WWDC26 268):** Top Functions mode, Run Comparisons with `os_signpost` intervals, Swift Executors instrument (finds work stuck on the Main Actor), System Trace for low-CPU hangs. "Profile release builds." Move work off the main actor with `@concurrent`. [V] https://developer.apple.com/videos/play/wwdc2026/268
- **MetricKit (iOS 27):** a new `MetricManager` with `for await` streams `metricReports` / `diagnosticReports` (crash, hang, memory-exception). Per-state hitch rates via StateReporting (e.g. per tab). "All the new features are exclusive to the new MetricManager API." [V] https://developer.apple.com/videos/play/wwdc2026/222
- For the video path: the SwiftUI layer must stay out of the frame loop. Render video in its own layer (`AVSampleBufferDisplayLayer` or `CAMetalLayer`). Let SwiftUI draw only the controls.

---

## Suggested shape for Bloom iOS (my synthesis)

1. Use two engines behind one `Player` protocol. **AVPlayer** for anything the device profile marks as direct-play-able in MP4/HLS, and always while AirPlay or CarPlay video is the route. **mpv** for MKV, PGS/ASS, DTS/TrueHD and DV P7.
2. Move mpv on iOS from `gpu-next`/MoltenVK/`CAMetalLayer` to a **sample-buffer output** (study MPVUI's `0005-avfoundation-native-video` patch, LGPL credits included). This is what makes PiP possible, and it removes the glass-backdrop `screenshot-sw` hack: the video becomes a normal layer.
3. Use custom SwiftUI controls for both engines, so the player looks the same whichever engine plays. Take the PiP glyphs, `AVRoutePickerView` and MediaAccessibility caption styles from the system.
4. Use the Now Playing framework on iOS 27, with MediaPlayer as the fallback for 26.
5. Keep the Rust Cast v2 implementation. Discover through `NWBrowser` with `NSBonjourServices`. Do not request the multicast entitlement. Revisit AVSystemRouting when a Google extension ships.
6. Downloads: one background `URLSession`, files excluded from backup. A Live Activity only if updates come from completion events.
