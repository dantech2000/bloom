# A native iOS client for Bloom: what Apple recommends in November 2026

Research date: 2026-11-13. Current SDK: iOS/iPadOS 27 (Xcode 27, Swift 6.4).

**Legend.** **[V]** means I read it in a fetched Apple source during this research (doc JSON, WWDC transcript, HIG, news page). **[M]** means it comes from my own knowledge (cutoff mid-2026) and I did not re-check it. Availability numbers like "iOS 26.0" come from the `platforms` metadata of each doc page.

Main sources:
- SwiftUI updates: https://developer.apple.com/documentation/updates/swiftui
- UIKit updates: https://developer.apple.com/documentation/updates/uikit
- WWDC26 SwiftUI guide: https://developer.apple.com/wwdc26/guides/swiftui/
- WWDC26 sessions 269, 272, 278, 321, 267, 262, 220, 312, 258, 260
- WWDC25 sessions 256, 323, 219, 208, 268, 245, 344, 361, 282

---

## 0. Six findings that change the plan

1. **The iOS 27 SDK requires the UIScene life cycle. Without it, an app built with that SDK does not launch.** [V] The UIKit updates page says so under June 2026, and session 278 says: "UIScene lifecycle is now required when building with the latest SDKs. Without it, your application will no longer launch." A SwiftUI `App` already uses scenes. A UIKit shell must have a `UIWindowSceneDelegate`. Sources: https://developer.apple.com/documentation/updates/uikit, https://developer.apple.com/documentation/uikit/transitioning-to-the-uikit-scene-based-life-cycle, https://developer.apple.com/videos/play/wwdc2026/278
2. **iPhone apps can be resized on iOS 27.** [V] They resize in iPhone Mirroring on macOS 27 and when an iPhone app runs on an iPad. Apple says the interface idiom is "no longer meaningful for any kind of layout decision". Supported orientations are "a preference" that a resizable environment ignores. Base the layout on size classes and the size of the view, not on the idiom, the orientation or `UIScreen.main`. `UIRequiresFullScreen` now gives "discrete resizing" on iPhone; it no longer opts the app out of resizing. Bloom's current iOS port ties the landscape player to `set_landscape`, so this item applies to it directly. Source: session 278.
3. **`AsyncImage` now caches.** [V] On the 2027 releases it uses standard HTTP caching by default, with no code change. Xcode 27 adds `asyncImageURLSession(_:)` (iOS 27.0) and `AsyncImage(request:…)` for a custom `URLCache` or cache policy. Sources: session 269, https://developer.apple.com/documentation/swiftui/view/asyncimageurlsession(_:)
4. **`@State` is now a macro.** [V] A class stored in `@State` is created only once per view lifetime, and Apple back-ported this behavior to iOS 17. This can break source: a default value combined with an assignment in `init` is now an error. The builders were also unified as `ContentBuilder`, which type-checks faster. Source: session 269.
5. **The September 2026 updates add hardware for a foldable "iPhone Duo".** [V] The new APIs are `ReservedRegion` (camera or hinge), `onHingeChange`, controls on the vertical axis (`toolbarVerticalEdge`, `toolbarVerticalBehavior(_:)`, `axisBehavior(_:)`) and `ArrangementView` (an adaptive primary/secondary layout). The UIKit equivalents are `UIArrangementViewController`, `UIView.ReservedRegion` and `UIHingeInteraction`. An iPhone-first app should stay away from reserved regions and let the system bars move. Sources: the two updates pages above.
6. **The SDK floor for uploads is Xcode 26 with the iOS 26 SDK (since April 28, 2026).** [V] The minimum deployment target that Apple accepts is iOS 13 (since September 9, 2026). The page says nothing yet about an Xcode 27 requirement. Source: https://developer.apple.com/news/upcoming-requirements/

---

## 1. SwiftUI or UIKit in 2026

**Apple's position [V].** The two frameworks are meant to be mixed, and Apple expects incremental adoption: "there are no expectations that an app needs to be entirely SwiftUI". Apple tells developers to "Start with SwiftUI for new scenes" and to "Consider SwiftUI when you implement a new component" (session 272, https://developer.apple.com/videos/play/wwdc2026/272). Apple's own controls now render through SwiftUI internally, and Liquid Glass "is also using SwiftUI to share large parts of the implementation". The new API work goes to SwiftUI first: documents, reordering, toolbar priority. UIKit gets parity APIs plus the adaptivity changes.

**Interop that is current [V]:**
- `UIGestureRecognizerRepresentable` (iOS 18) attaches a UIKit recognizer to a SwiftUI view.
- `UIHostingConfiguration` (iOS 16) puts SwiftUI cells into a `UICollectionView`.
- `UIHostingSceneDelegate` (iOS 26) hosts SwiftUI scenes in a UIKit app.
- Observation tracking in UIKit is on by default from iOS 26. `layoutSubviews`, `updateProperties` and cell configuration re-run automatically when an `@Observable` value they read changes. On iOS 18, set the `UIObservationTrackingEnabled` key in Info.plist.
- iOS 27: closures of `UICollectionViewCompositionalLayoutSectionProvider` take part in observation tracking.

**Where UIKit is still the better choice [M, partly V]:**
- **Video surface.** mpv draws into a `CAMetalLayer` that a `UIView` hosts, wrapped in `UIViewRepresentable`. Use `AVPlayerViewController` only if you use AVFoundation playback. SwiftUI has no layer-backed video surface for a custom renderer.
- **Custom text input.** `UITextInput` and `UIKeyInput` exist only in UIKit. Bloom's GPUI port needed this; a native app with `TextField` does not.
- **Very large or complex collections.** These include compositional layouts with orthogonal sections, prefetching through `UICollectionViewDataSourcePrefetching`, and fine control over cell reuse. SwiftUI lazy stacks are adequate for poster rows; see section 4.
- **Low-level touch.** Examples are simultaneous recognizers on the video, such as a pan to seek plus a tap to show the controls. In SwiftUI, use `UIGestureRecognizerRepresentable`. iOS 27 adds `GestureInputKinds` initializers that filter direct touch, indirect touch, pencil or pointer. [V]

**Recommended split for Bloom:**
- SwiftUI for the app structure (`App`, `TabView`, `NavigationStack` and `NavigationSplitView`) and for every page: home, library, detail, search, settings and the admin dashboard.
- One `UIViewRepresentable`, or a `UIViewControllerRepresentable`, for the player surface (libmpv on `CAMetalLayer`). The player controls on top of it are SwiftUI.
- `@Observable` models shared across both.
- Drop to UIKit only where a profiler shows a SwiftUI list or grid as the bottleneck. Then use `UICollectionView` with `UIHostingConfiguration` cells.

---

## 2. What is new: iOS 26 (WWDC25) and iOS 27 (WWDC26)

### iOS 26: Liquid Glass and structure (all [V] for name and availability)

| API | Avail. | Note |
|---|---|---|
| `glassEffect(_:in:)` | 26.0 | Glass on a custom view. `Glass.regular`, `.clear`, `.tint(_:)`, `.interactive(_:)` |
| `GlassEffectContainer` | 26.0 | Groups shapes so that they blend and morph |
| `glassEffectID(_:in:)` | 26.0 | Morph between glass shapes with a `@Namespace` |
| `.buttonStyle(.glass)` / `.glassProminent` | 26.0 | |
| `ToolbarSpacer` | 26.0 | Fixed or flexible gap between groups of toolbar items |
| `tabBarMinimizeBehavior(_:)` | 26.0 | For example, `.onScrollDown` |
| `tabViewBottomAccessory(content:)` | 26.0 | For example, a mini-player. `isEnabled:` variant from 26.1. Read `TabViewBottomAccessoryPlacement` |
| `Tab(role: .search)` | role 18.0, new bar behavior 26 | On iPhone the search field replaces the tab bar (session 323) |
| `searchToolbarBehavior(_:)` | 26.0 | Forces the minimized search button |
| `scrollEdgeEffectStyle(_:for:)` | 26.0 | |
| `backgroundExtensionEffect()` | 26.0 | Mirrors and blurs an image under the sidebar or the safe area. Good for a hero backdrop |
| `WebView` / `WebPage` (WebKit) | 26.0 | Native SwiftUI web view |
| `TextEditor` + `AttributedString`, `AttributedTextSelection` | 26 | Rich text |
| `Animatable()` macro, `Slider` ticks, `draggable(containerItemID:…)` | 26 | |

Sheets [V, session 323]: partial-height sheets are inset with a glass background by default. At full height the background becomes opaque. Remove custom `presentationBackground`. To morph a sheet out of a toolbar button, mark the button as a source with `matchedTransitionSource(id:in:)` and the sheet with `.navigationTransition(.zoom(sourceID:in:))`. Both exist since iOS 18 [V]. The same zoom gives poster → detail pushes in a `NavigationStack`.

Design rules [V, session 323 and HIG Materials]:
- "use standard app structures, toolbars, search placements, and controls" to get the new design.
- Do not put Liquid Glass in the content layer. Use it sparingly on custom controls.
- Use the `clear` variant only over rich media, such as video. Add a 35% dimming layer if the content behind is bright.
- Use tint to give meaning, not as decoration.
- Remove background colours behind bars and sheets.

Source: https://developer.apple.com/design/human-interface-guidelines/materials

### iOS 27: WWDC26 (all [V] unless marked)

- **Look:** the existing Liquid Glass gets a refined appearance with no code change. Users get a Liquid Glass slider that adjusts the tint. The iPad dims inactive windows; read `appearsActive`. Menu bars show fewer icons by default; use `.labelStyle(.titleAndIcon)` or `preferredImageVisibility` (UIKit) to force an icon. (Sessions 269 and 278.)
- **Tabs:** `TabRole.prominent` (27.0) puts a tab at the trailing end, separated from the others. In UIKit, use `UITabBarController.prominentTabIdentifier`. iPhone apps can opt into a sidebar through `sidebar.preferredPlacement = .sidebar` (UIKit, session 278).
- **Toolbars:** `visibilityPriority(_:)`, `ToolbarOverflowMenu` (27.0), `topBarPinnedTrailing` placement, and `toolbarMinimizationBehavior(_:for:)` (27.0; the session text calls it "toolbarMinimizeBehavior … onScrollDown for the navigationBar placement"). In UIKit, use `UINavigationItem` `barMinimizationBehavior` and `barMinimizationSafeAreaAdjustment`.
- **Scroll edge:** `.automatic` has its own look now. Re-evaluate any override to `.soft` (session 278).
- **Containers:** `reorderable()` + `reorderContainer(for:isEnabled:move:)` work in `List`, `LazyVGrid` and stacks. `swipeActions(…)` + `swipeActionsContainer()` (27.0) work outside `List`. These fit "Up next" queues and playlists.
- **Presentations:** `alert(_:item:actions:)`, `alert(error:actions:)` and `confirmationDialog(_:item:…)` work like `sheet(item:)`. A sheet can use the `crossFade` transition.
- **Data flow and performance:** the `@State` macro, `ContentBuilder` and `AsyncImage` caching (section 0).
- **Documents:** `ReadableDocument`, `WritableDocument`, `DocumentReader` and `DocumentWriter`. Not relevant for Bloom.
- **Media:** the new **NowPlaying framework**: an `@Observable` model conforms to `MediaSessionRepresentable` and is connected through `MediaSession`. The system then keeps the Lock Screen, Control Center, the Dynamic Island and CarPlay up to date. Content types include `MovieContent`. Session 312: https://developer.apple.com/videos/play/wwdc2026/312. I did not check its exact availability, and `MPNowPlayingInfoCenter` remains the API for iOS 26 [M]. Also relevant: session 256, "Discover generated subtitles and subtitle styles".
- **Siri:** menus get an "Ask Siri" item. The View Annotations API maps views to `AppEntity` (session 278).
- **Tooling:** Xcode 27 ships agent skills ("SwiftUI Specialist", "What's New in SwiftUI", app modernization), which `xcrun agent skills export` exports. Device Hub replaces the Simulator window and has a resize mode. Previews have resize handles (sessions 269, 258 and 260).

---

## 3. State and data flow, concurrency

**Observation [M, consistent with V].** Use `@Observable` classes for models. Store them in `@State` at the owner (lazy now, see section 0), pass them down as plain properties or through `@Environment(Type.self)`, and use `@Bindable` for bindings into an observable. Leave `ObservableObject` and `@StateObject` for older code. Session 321 adds rules for lazy stacks [V]:
- Keep important state out of row-level `@State`, because rows that scroll away are freed.
- Filter at the data level, not with `if` in a row's body.
- Set the row up in `init`, not in `onAppear`, so that prefetching does not go to waste.

**Concurrency [V, sessions 268 and 245 of WWDC25]:**
- "Your apps should start by running all of their code on the main thread, and you can get really far with single-threaded code."
- For the app module, Apple recommends two build settings: **Approachable Concurrency** and **Default Actor Isolation = MainActor**. Both are "enabled by default for new app projects created with Xcode 26".
- Move heavy work off the main actor with `@concurrent` on a function, for example image decoding, JSON parsing of large library pages or index building.
- Use `nonisolated` for library-style code, so that the caller decides where it runs.
- Introduce an `actor` only when main-actor contention shows up, for example in a download manager or an image cache.
- "UI-facing classes should stay on the main actor."

So a `@MainActor` view model (implicit under default isolation) is the recommended shape, not something to avoid. Put the networking client in a separate module, such as a Swift package, that is `nonisolated` with `Sendable` value types. Swift 6.3 and 6.4 (WWDC26 session 262) add no change to these defaults that I found. Community reports say Xcode 27 keeps them [unverified].

**Structured concurrency in views [M]:**
- Use `.task { }` for loads tied to a view's lifetime, which cancels automatically.
- Use `.task(id: query)` to restart when an input changes, for example search-as-you-type with `try await Task.sleep` as a debounce.
- Do not use `Task {}` in `onAppear` for this.

---

## 4. Poster lists and grids, scrolling, images

**Lazy stacks [V, session 321, https://developer.apple.com/videos/play/wwdc2026/321]:**
- Content above the visible area is *estimated*. "Avoid using the absolute content size or content offset with lazy stacks." Prefer `onScrollTargetVisibilityChange(idType:threshold:_:)` and `onScrollVisibilityChange(threshold:_:)` (both iOS 18) over `onScrollGeometryChange` offsets.
- A nested `LazyHStack` in a horizontal `ScrollView` inside a `LazyVStack` is the endorsed pattern for home rows. The row height comes from the *first* item, so give each card a fixed height (line limits, reserved title space).
- Each `ForEach` element must resolve to exactly one subview, with no conditional content in leaf views. Otherwise programmatic scrolling and memory suffer.
- `onAppear` on a trailing `ProgressView` is fine for infinite paging. It replaces Bloom's Previous/Next pager on a phone.
- Do not change a row's layout after it appears (the `onGeometryChange` → `@State` → relayout loop). Use a custom `Layout` instead.
- `scrollTransition` must not push views into the visible rect from outside.
- iOS 27: "lazily loaded subviews that prefetch content" (iOS 27 guide). Start a loader in `init` so that prefetching starts the fetch early.

**Scroll APIs [V availability]:**
- `scrollPosition(_:anchor:)` with `ScrollPosition` (18.0)
- `scrollTargetBehavior(.viewAligned)` + `.scrollTargetLayout()` (17.0) for rows that snap to cards. `.paging` for a hero carousel. `limitBehavior` `.alwaysByOne` (18).
- `containerRelativeFrame(_:count:span:spacing:)` (17.0) to size "3 posters across" from the container. This replaces Bloom's `cards::Metrics` arithmetic and works in a resizable window.
- `LazyVGrid(columns: [GridItem(.adaptive(minimum:))])` for library grids. `List` only for row-style lists, such as settings, users, tasks and logs.

**Images:**
- iOS 27 `AsyncImage` with HTTP caching, plus `asyncImageURLSession` with a large `URLCache`, is now viable [V].
- On iOS 26 it does not cache in memory [V: "up until now, AsyncImage hasn't kept images in memory"].
- Jellyfin image URLs carry a `tag` and are effectively immutable, so a shared `URLSession` with a large `URLCache` works. Add a small `NSCache` of decoded, downsampled `UIImage` (`preparingThumbnail(of:)` or ImageIO, decoded off the main actor with `@concurrent`) [M].
- Third-party options such as Nuke and Kingfisher remain common [M]. With an iOS 27 floor you may not need them.
- Request the server-side size (`maxWidth`, `fillHeight`) at `displayScale`, as Bloom already does.

---

## 5. Text input and sign-in [M unless noted]

- Use `TextField` + `.textContentType(.username)`, `SecureField` + `.textContentType(.password)`, `.textContentType(.oneTimeCode)` for a Quick Connect or OTP code, and `.textContentType(.URL)` + `.keyboardType(.URL)` for the server address. Add `.textInputAutocapitalization(.never)` and `.autocorrectionDisabled()` for username and URL.
- `.submitLabel(.next / .go)` (15.0 [V]) + `.onSubmit`, and `@FocusState` with `.focused(_:equals:)` (15.0 [V]) to step between fields.
- Keyboard avoidance is automatic for SwiftUI scroll views. Use `.scrollDismissesKeyboard(.interactively)`. Do not hand-roll the keyboard inset that Bloom's GPUI port needed.
- AutoFill offers saved passwords from the content types. Full "save password" association needs `webcredentials:` Associated Domains on a domain you control. For self-hosted servers this does not work per server; the heuristic save prompt still appears.
- Passkeys (`ASAuthorizationPlatformPublicKeyCredentialProvider`, iOS 15 [V]) do not apply, because Jellyfin offers no WebAuthn. For an SSO plugin flow, use `ASWebAuthenticationSession`.
- Store the access token in the Keychain, never in `UserDefaults`.

---

## 6. Accessibility

[V, HIG Accessibility, https://developer.apple.com/design/human-interface-guidelines/accessibility]:
- Minimum control size on iOS and iPadOS is **44×44 pt** (28×28 pt is the absolute minimum).
- Pad about 12 pt around bezeled elements and about 24 pt around elements without a bezel.
- Support text enlargement of at least 200%, preferably through Dynamic Type.
- Meet WCAG AA contrast. Provide a higher-contrast scheme under Increase Contrast.
- Under Reduce Motion, reduce automatic and repetitive animation, including zooming and scaling.

Liquid Glass and settings [V, session 219 of WWDC25]:
- "Reduced Transparency makes Liquid Glass frostier".
- "Increased contrast makes elements predominantly black or white … with a contrasting border".
- "Reduced Motion … disables any elastic properties".

System glass does all of this for free. Custom glass from `glassEffect` follows too. Hand-made translucency does not. Read `accessibilityReduceTransparency` and `accessibilityReduceMotion` (both [V], iOS 13) for custom surfaces and for `navigationTransition(.zoom)`.

VoiceOver [V, WWDC26 session 220, https://developer.apple.com/videos/play/wwdc2026/220]:
- Give each custom control a label, a value, actions and announcements.
- Use the `.adjustable` trait for single-axis controls, such as the seek bar and volume.
- Use custom actions (`accessibilityAction(named:_:)` [V]) for anything multi-directional or hidden behind gestures. Typical cases: the long-press card menu, "Mark played", "Add to favourites".
- Use `allowsDirectInteraction` only where gestures are essential.
- `accessibilityRotor` [V] can expose seasons and episodes on a detail page.

Use semantic fonts (`.body`, `.headline`) or `@ScaledMetric` so that Dynamic Type works. Bloom's GPUI port never applied `text_scale`.

---

## 7. iPad

- Start with a tab bar that can become a sidebar: `.tabViewStyle(.sidebarAdaptable)` (18.0 [V]) with `TabSection` for libraries. HIG Tab bars [V]: "Prefer a tab bar for navigation", "Don't disable or hide tab bar buttons". Session 208 of WWDC25 [V]: "If you're still not sure which navigation is best for your app, start with a tab bar."
- `NavigationSplitView` fits inside a tab with deep hierarchy, for example dashboard sections or settings. Do not let a sidebar selection change the tab [V, HIG].
- iPadOS 26 windowing [V, session 208]:
  - Windows resize freely, with window controls at the top leading corner. "Wrap your toolbar around window controls" instead of reserving a safe area.
  - Give windows descriptive names. Open each document in its own window. For Bloom, that means a detail page or the player in a new window through `openWindow` [M].
- **Bloom's `UIRequiresFullScreen = true` goes against this.** It was set to avoid the iPadOS 26 scaled-window problem in the GPUI port. A native SwiftUI app should drop it.
- Menu bar [V]: "every app on iPad is getting its own menu bar". Put the tabs and the sidebar toggle in the View menu, with keyboard shortcuts. Keep menus static. In SwiftUI, add `.commands { CommandMenu / CommandGroup }` on the `WindowGroup` and `.keyboardShortcut(_:modifiers:)` [V, 14.0]. Use `focusedValue` to route the commands to the active window. In UIKit, the equivalent is `UIMainMenuSystem` (26.0 [V]).
- Pointer [V]: the new pointer has a glass hover highlight and no magnetism. Test custom hit areas. Use `.hoverEffect` on custom cards [M].
- Size classes, not the idiom, drive the layout. This matters on both iPad and iPhone in iOS 27 (section 0).

---

## 8. App life cycle and architecture [M unless noted]

- Use a SwiftUI `App` with a `WindowGroup`. Add an `@UIApplicationDelegateAdaptor` only for background `URLSession` events (downloads) and remote notifications. The SwiftUI life cycle meets the iOS 27 scene requirement [V for the requirement].
- `@Environment(\.scenePhase)`: pause polling and the socket in `.background`, and resume in `.active`. Report playback progress from the player model, not from the phase.
- Store per-window navigation state in `@SceneStorage` (14.0 [V]) as an encoded `NavigationPath`, using `Codable` route values. The selected tab can go there too. Account and server configuration go in the Keychain and `@AppStorage`.
- Deep links:
  - Use `.onOpenURL` (14.0 [V]) for a custom scheme (`bloom://item/<id>`) and for Universal Links. Universal Links need an `apple-app-site-association` file on a domain you own; self-hosted servers cannot provide one.
  - Use `handlesExternalEvents(preferring:allowing:)` [V] to route a link to an existing window on iPad.
  - Open the trailer with `@Environment(\.openURL)`, or an in-app `WebView` [V, 26] / `SFSafariViewController`.
- Background playback: set `AVAudioSession` category `.playback`, the "audio" background mode, Now Playing (section 2) and Picture in Picture. With mpv, PiP needs an `AVSampleBufferDisplayLayer` route; this is hard [M].

---

## 9. Testing

- **Swift Testing vs XCTest [V, WWDC26 session 267, https://developer.apple.com/videos/play/wwdc2026/267]:** write new unit tests with `@Test`, `#expect` and `#require`, parameterized tests and traits. "UI automation and performance testing APIs are only available in XCTest." Xcode 27 adds *test framework interoperability*: an XCTest assertion called from a Swift Testing test reports an issue. The modes are limited (warnings; the default for test plans from before Xcode 27), complete (errors; the default for new projects), strict and none. The override is `SWIFT_TESTING_XCTEST_INTEROP_MODE`. Exit tests are macOS, Linux, FreeBSD and Windows only. Swift 6.4 adds non-fatal issues and dynamic skips (session 262).
- **XCUITest [V, WWDC25 session 344]:**
  - UI recording in Xcode 26 writes Swift code from your taps.
  - Test plans replay across languages and devices, with video for each run (kept for failures by default) and an element overlay at the moment of failure.
  - Use `.accessibilityIdentifier`s that are unique, static and not localized. Use `XCUIApplication.open(url)` for deep links.
  - `performAccessibilityAudit(for:_:)` (XCUIAutomation, iOS 17 [V]) runs Accessibility Inspector's checks inside a UI test.
- **App Intents:** `AppIntentsTesting` (WWDC26 session 295) tests intents without UI automation [V, iOS 27 guide].
- **Snapshot testing:** Apple ships none. Point-Free's `swift-snapshot-testing` is the usual choice [M]. Xcode Previews with resize handles (Xcode 27 [V]) and Device Hub's resize mode cover layout checks by hand.
- **Running the app on the simulator:** Device Hub runs simulator and device windows, sets accessibility and text-size overrides, and simulates conditions such as location [V, session 260].

---

## 10. App Store readiness

- **Privacy manifest** (`PrivacyInfo.xcprivacy`) [V]: required since May 1, 2024 for required-reason APIs, including those in third-party SDKs. Likely categories for Bloom:
  - `NSPrivacyAccessedAPICategoryUserDefaults` (reason `CA92.1`)
  - `NSPrivacyAccessedAPICategoryDiskSpace` (download space check: `E174.1`; showing space to the user: `85F4.1`)
  - `NSPrivacyAccessedAPICategoryFileTimestamp` if you read file dates (`C617.1` for files inside the container)

  The category keys and reason codes exist in the doc JSON [V]; the meaning of each reason is [M]. Declare `NSPrivacyTracking = false` and collected data types: no tracking, so no ATT prompt and no `NSUserTrackingUsageDescription` [M]. Sources: https://developer.apple.com/documentation/bundleresources/privacy-manifest-files, https://developer.apple.com/documentation/bundleresources/describing-use-of-required-reason-api
- **Local Network** [V]: "Any app that uses the local network, directly or indirectly, should include" `NSLocalNetworkUsageDescription` (iOS 14). This covers Bonjour and unicast to local hosts, so a LAN Jellyfin server at `192.168.x.x` also triggers the prompt [M]. List every browsed type in `NSBonjourServices`: `_googlecast._tcp` for Chromecast and `_airplay._tcp` / `_raop._tcp` for AirPlay [M]. Jellyfin's own discovery is UDP broadcast on port 7359. Multicast or broadcast needs the `com.apple.developer.networking.multicast` entitlement, which you request from Apple [M].
- **ATS** [V]: `NSAllowsLocalNetworking` allows plain HTTP to unqualified names, `.local` names and IP addresses. Since iOS 17, connections to IP addresses are no longer allowed by default without it. A self-hosted client also needs plain `http://` to arbitrary public hostnames. Many Jellyfin clients set `NSAllowsArbitraryLoads` and justify it in review [M]. Decide this explicitly. Source: https://developer.apple.com/documentation/bundleresources/information-property-list/nsapptransportsecurity/nsallowslocalnetworking
- **Export compliance** [M]: an app that uses only OS-provided HTTPS/TLS is exempt. Set `ITSAppUsesNonExemptEncryption = NO`. Correction (review, 2026-11-13): the Rust core does its TLS with `rustls` and `ring`, not the system's stack, for the server and the Chromecast; check the export rules for that before submitting.
- **Age rating** [V]: the questionnaire was updated by January 31, 2026. **EU trader status** [V] is needed if the app is sold in the EU.
- **Deployment target:**
  - Apple's June 7, 2026 figures [V]: iOS 26 runs on 86% of iPhones introduced in the last four years and on 79% of all iPhones. iPadOS 26 runs on 79% and 68% of iPads. Source: https://developer.apple.com/support/app-store/
  - **Recommendation: iOS 26 minimum, built with the iOS 27 SDK.** This gives Liquid Glass APIs without `#available` forks, `Tab(role: .search)` bar behavior, `WebView`, `tabViewBottomAccessory` and default Observation tracking in UIKit. Gate iOS 27 extras with `if #available(iOS 27, *)`: prominent tab, toolbar priority, reorderable, `AsyncImage` cache APIs and NowPlaying. iOS 18 support would double the design work, because pre-glass bars look and behave differently. An iOS 27-only target is reasonable for a release in spring 2027 or later.
- **App icon** [V, WWDC25 session 361]: build a layered `.icon` file in Icon Composer (1024 px canvas shared by iPhone, iPad and Mac). It renders Default, Dark, Clear light/dark and Tinted light/dark. Drag it into Xcode and choose it in the Project Editor. The existing `assets/icon/bloom.png` would need splitting into SVG or PNG layers.

---

## Short checklist for Bloom-on-SwiftUI

1. Build a SwiftUI `App` with a `TabView` using the `Tab` API: Home, libraries, Favourites, Search (`role: .search`). Add `.tabViewStyle(.sidebarAdaptable)`, `tabBarMinimizeBehavior(.onScrollDown)` and the mini-player in `tabViewBottomAccessory`.
2. Put a `NavigationStack` in each tab with `Hashable` routes. Push poster → detail with `matchedTransitionSource` + `.navigationTransition(.zoom)`.
3. Use system bars, sheets and buttons for the glass, and `glassEffect` only on player controls over video (`.clear`, with dimming).
4. Use `@Observable` models, default MainActor isolation, a networking package that is `nonisolated`/`Sendable`, `.task(id:)` loads and `@concurrent` decoding.
5. Use nested `LazyHStack` rows with fixed card heights and `containerRelativeFrame` widths, and `LazyVGrid` with paging through `onAppear`.
6. Host the player surface in `UIViewRepresentable` (mpv on `CAMetalLayer`). Use the NowPlaying framework on iOS 27 and `MPNowPlayingInfoCenter` on iOS 26.
7. Make the layout use size classes only, with no idiom or orientation checks. Do not set `UIRequiresFullScreen`.
8. Use Swift Testing for core logic and XCUITest with identifiers + `performAccessibilityAudit` for flows.
9. Ship with `PrivacyInfo.xcprivacy`, the local network keys, an explicit ATS decision and an `.icon`.
