# Bloom for iOS, native: the parity table

One row per item of [inventory.md](inventory.md), with its path in the new structure ([README.md](README.md) §6), its phase ([plan.md](plan.md)) and its status.
Status is "todo" or "dropped" now; each row moves to "done" as its phase lands, and M1 needs every phone row done or dropped.

Phases: S (spikes), 2, 3, 4a–4d, 5, 6, 7, then M1, then 8 (iPad), 9 (dashboard), 10, 11. "Spec §n" points into README.md. An item that shows in several inventory sections has one row, in the first section that names it. A row whose Notes start with "GAP:" has no path or no build decision in the spec, and its phase is "?" where the spec gives none.

## Gaps found

Nine gaps were found in draft 2; spec §16 settles each (draft 3). The rows below carry the decisions.

---

## 0. App skeleton

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §0 | Screens, pages and panels; video only | Tabs, pushed pages, sheets | 3 | done | Settings and Dashboard become sheets (spec §6.1); no music, books or Live TV |
| §0 | Back history with scroll place | System back button and edge swipe | 3 | done (native) | State restoration per server and user (plan 3.7) |
| §0 | Forward history; Mac back gestures (Cmd-[, trackpad, side buttons) | — | — | dropped | Mac-only; the phone has back only today (spec §6.2) |
| §0 | Toasts for every failure, one queue | Toast overlay | 3 | done | Own glass overlay, announced to VoiceOver (spec §6.3) |
| §0 | Session guard (epoch) | Core `Session`, `BloomError::Stale` | 2 | done (native) | Guard lives in core; a test fails without it (spec §5.1) |
| §0 | Page loaders in core; live updates over the socket | Through the bridge | 3 | done | `load_home` and `realtime::start` proven in S1 |

## 1. Screens and pages

### 1.1 Splash

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.1 | Bloom splash animation, 1.5 s, never a wait | At launch | 3 | done | Still under Reduce Motion (spec §8.1) |

### 1.2 Connect and sign-in

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.2 | Server discovery | — | — | dropped | Not today either; needs the multicast entitlement (Q5) |
| §1.2 | Server list, address field, Connect, address errors | Connect screen | 3 | done | URL keyboard and Go; local network sentence for private addresses; core codes, Swift sentences. Reached while signed in through Account › Servers & profiles… (spec §16.2) |
| §1.2 | Remove this server (revokes tokens) | Connect screen; sign-in page | 3 | done | Destructive `confirmationDialog`. Reached while signed in through Account › Servers & profiles… (spec §16.2) |
| §1.2 | Branding: server splash image, login disclaimer, titles | Sign-in page | 3 | done | |
| §1.2 | Saved profile tiles; Forget this profile | Sign-in page | 3 | done | Destructive confirm. Reached while signed in through Account › Servers & profiles… (spec §16.2) |
| §1.2 | Public users; sign in as another user; sign-in errors | Sign-in page | 3 | done | Username untyped, no autocorrection; `SecureField` `.password`; 401 → "Sign in again" code |
| §1.2 | Quick Connect sign-in (code, 3 s poll, expiry) | Sign-in page › Use Quick Connect | 3 | done | Poll stops in the background, polls once at return (spec §8.1) |
| §1.2 | Quick Connect authorize (approve a device) | Account sheet › Quick Connect; Settings › Profile › Enter code | 3 | done | `.oneTimeCode`, number pad |
| §1.2 | open_session steps (client, epoch, prefs, index, socket, settings) | — | 3 | done (native) | Core `ios::session` copied from `app/sessions.rs` (spec §5.2) |
| §1.2 | Link speed test at sign-in | — | 4b | done | Not on cellular or in Low Data Mode (spec §16.8) |
| §1.2 | Switch between servers and profiles | Account sheet › profiles by server | 3 | done | |
| §1.2 | "Servers & profiles…" with the "Back to {user}" pill | Account sheet › Servers & profiles… | 3 | done | Full-screen sheet (spec §16.2) |
| §1.2 | Sign out | Account sheet › Sign out; Settings › Profile › Sign out | 3 | done | `SessionScope` clears every store |
| §1.2 | iOS connect sizing; card above the keyboard | — | 3 | done (native) | System fields, Dynamic Type and keyboard avoidance replace them |

### 1.3 Offline state

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.3 | `classify`, probe, retry backoff, page auto-reload | — | 3 | done | Adds "local network denied"; `NWPathMonitor` feeds core (spec §8.4, §8.10) |
| §1.3 | Going offline: toast, Downloads opens, image fetches stop | Downloads tab selected | 3 | done (native) | The tab switch is phase 3; its content is phase 5; a cold launch offline opens Downloads |
| §1.3 | Offline page (reason, countdown, Retry, Open Downloads) | In place of the page | 3 | done | `ContentUnavailableView` |
| §1.3 | Top-bar offline chip (Retry, Downloads) | Bottom accessory | 3 | done (native) | Bottom accessory "{server} is offline" with Retry, ahead of a cast or a remote; the Downloads tab is selected and Home is the offline page (spec §16.9). A closed socket, a failed news refresh or a failed reload on the tabs goes offline; the socket opening again, the 30 s look or Retry comes back to the tab before. Test: `testOffline` (a relay in front of the mock) |
| §1.3 | Recovery: toast, socket restart, positions flushed, Home or reload | — | 3 | done (native) | The position flush lands in phase 5 |

### 1.4 Shell: bars and navigation

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.4 | Mac top bar tabs; Home tab | Fixed tabs Home, Library, Downloads, Search | 3 | dropped (Mac) | Spec §6.1 (Q9) |
| §1.4 | Favorites, per-library and Playlists tabs | Library tab › Favorites / library / Playlists | 3 | done | Pin a library as a tab after M1 |
| §1.4 | Cast chip | `tabViewBottomAccessory` | 6 | done (native) | Also shows a SyncPlay group |
| §1.4 | Downloads chip | Badge on the Downloads tab | 5 | done (native) | |
| §1.4 | Restart to update (chip and account entry) | — | — | dropped | Sparkle is Mac-only; dev-signed install (spec §2, §12) |
| §1.4 | Play on and SyncPlay buttons | Account sheet › Play on / SyncPlay; Player | 6 | done (native) | |
| §1.4 | Random item, Search button, account avatar | Account sheet › Random item; Search tab; toolbar avatar on each tab root | 3 | done (native) | |
| §1.4 | iPad glyph-only tabs under 900 pt | Sidebar | 8 | done (native) | `sidebarAdaptable` with libraries in a `TabSection` |
| §1.4 | Phone tab bar and its behaviour (lights, keyboard, centred label) | System `TabView` | 3 | done (native) | Settings and Dashboard are sheets, so the tab lights hold by structure |
| §1.4 | Phone top bar (back, title, account) | System navigation bar | 3 | done (native) | |
| §1.4 | Mac keys and hero keys (Cmd-[, Esc, /, Shift-H, R; ←/→, Space) | iPad › keyboard | 8 | dropped (Mac) | `.commands` (spec §6.4); hero keys not named there; M goes with the hero trailer |

### 1.5 Home

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.5 | Hero: 20 slides, size, title art, chips, genres, overview | Home tab | 3 | done (native) | Paging `ScrollView`, `backgroundExtensionEffect()`; Google Sans display title (Q2) |
| §1.5 | Hero auto-advance 7 s, 500 ms cross-fade | Home tab | 3 | done (native) | Stops while touched, under Reduce Motion, Low Power and Low Data Mode |
| §1.5 | Hero buttons (More info, Play, favourite), arrows, counter; swipe to step | Home tab | 3 | done (native) | Arrows → swipe on a phone; Play works from phase 4a |
| §1.5 | Trailer backdrop and its setting | — | — | dropped | Spec §8.2: not on iOS (MPVKit has no yt-dlp) |
| §1.5 | Rows Continue Watching, Next Up | Home tab | 3 | done | |
| §1.5 | Rows Recently Added, My Media | Home tab | 3 | done | |
| §1.5 | Rows in `homesection` order, lazily built, sideways scroll | Home tab | 3 | done | Nested `LazyHStack`s; iPad hover arrows in phase 8 |
| §1.5 | Empty state | Home tab | 3 | done | `ContentUnavailableView` |

### 1.6 Library

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.6 | Library grid; folders; a person's titles | Library tab › library; Detail › Cast & Crew › person | 3 | done | `LazyVGrid`; adaptive columns on iPad (8) |
| §1.6 | Collections | Library tab › Collections; library › View › Collections | 3 | done | |
| §1.6 | View, Sort, Filter menus | Library page › toolbar | 3 | done | |
| §1.6 | Letter rail | iPad › Library page | 8 | dropped (Mac) | Section index on iPad when sorted by name; none on a phone, as today |
| §1.6 | Pager, 100 a page | Library page | 3 | done (native) | Pager → infinite scroll, 100 at a time (Q6) |
| §1.6 | Random sort | Library page | 3 | done | One fixed page with "Shuffle again" |
| §1.6 | Play All / Shuffle | Library page | 4c | done | Needs the queue |
| §1.6 | Collection page More › Refresh | Collection page › More | 3 | done (native) | |
| §1.6 | Collection page More › Delete collection | Collection page › More | 7 | web | Destructive confirm |
| §1.6 | Skeleton, empty state, phone toolbar | Library page | 3 | done (native) | `redacted(.placeholder)`; toolbar menus replace the two-row toolbar |

### 1.7 Detail page

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.7 | Header: backdrop, title, season and show links, meta, overview, tags, credits | Detail (pushed) | 3 | done | Zoom transition from the poster |
| §1.7 | Enhanced lines (Seerr, Sonarr, Radarr links) | Detail | 7 | done | |
| §1.7 | Play / Resume, Play from the beginning; `play_series` | Detail | 4a | done | |
| §1.7 | Watch the trailer | Detail › SFSafariViewController | 3 | done | |
| §1.7 | Download button and its states | Detail | 5 | done | |
| §1.7 | Mark played, favourite | Detail; cards; hero | 3 | done | Haptic on toggle |
| §1.7 | More › Refresh, Open in Jellyfin Web | Detail › More | 3 | done | Plus `ShareLink` (spec §6.3) |
| §1.7 | More › Add to playlist / collection | Detail › More | 7 | done | With the lists request block (spec §16.1) |
| §1.7 | More › Hide… | Detail › More | 7 | done | |
| §1.7 | More › metadata entries (admin) | Detail › More | 9 | web | After M1 |
| §1.7 | Track rows Video, Audio, Subtitles; choice kept | Detail | 4b | done | `Menu` pickers; needs phase 4's track logic |
| §1.7 | Rows Next Up, Seasons, a season's episodes, More from {season} | Detail | 3 | done (native) | VoiceOver rotor of seasons and episodes |
| §1.7 | Rows Cast & Crew, Guest Stars, More Like This | Detail | 3 | done | |
| §1.7 | Bookmarks row | Detail | 7 | done | |
| §1.7 | Seerr rows, Request more seasons | Detail | 7 | done | Enhanced request block copied before phase 7 |
| §1.7 | Floating poster, two columns | iPad › Detail | 8 | done | The phone stays one column |

### 1.8 Search

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.8 | Local title index as you type | Search tab | 3 | done | The index stays in Rust: `Server::title_index` (the Mac's `catalog_query`) builds a `TitleIndex` over core's `search_index::Index` off the main thread after the sign-in, again on news (`AppModel.refresh`) and when older than 10 min; its `search` runs on the main thread at each key. The server's answer adds the titles the index lacks (episodes, new titles) below the local matches, which keep their places. Measured on the Mac (`titles::tests`, release build, the slowest of 7 texts, `MediaItem` conversion included; quiet machine): mock library 35 µs; 5,000 titles 0.65 ms (built in 0.65 ms); 20,000 titles 2.9 ms. With the Mac at a load average of 600 (median of 21 runs per text): 5,000 titles 1.1–2.5 ms. The test asserts the 8 ms budget (scaled under CI) for the mock and for 5,000 titles. UI test `Shots/testSearchIndex` (a typing error finds a film) |
| §1.8 | Server search, rows, empty and no-result states | Search tab | 3 | done | `.task(id: query)` debounce |
| §1.8 | Discover on Seerr row | Search tab | 7 | done | |
| §1.8 | Search field with predictions and Search key | Search tab | 3 | done | `Tab(role: .search)` |

### 1.9 Favorites

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.9 | Favorites page | Library tab › Favorites | 3 | done | Reachable with any number of libraries |

### 1.10 Playlists

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.10 | Playlists list, playlist page, rows, empty state | Library tab › Playlists › playlist | 3 | done | |
| §1.10 | Play, Shuffle, Play from here | Playlist page | 4c | done | |
| §1.10 | Rename, Delete playlist | Playlist page | 7 | done | Delete is a destructive confirm |
| §1.10 | Move up / down, Remove from the playlist | Playlist page | 7 | done | `reorderable()` and swipe actions |

### 1.11 Downloads page

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.11 | Entries by series or Movies; Play, Pause, Resume, Try again, Remove | Downloads tab | 5 | done | After a force quit entries show Paused with Resume (spec §8.5) |
| §1.11 | Try the server again, offline banner, empty state | Downloads tab | 5 | done (native) | |
| §1.11 | Remove all (confirm with size) | Downloads tab | 5 | done | |
| §1.11 | Show in Finder (page and Settings › Downloads) | — | — | dropped | Spec §8.6: the Files app does not see the folder |

### 1.12 Settings

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.12 | Section list, each pushed; controls with roles | Account sheet › Settings (large sheet) | 3 | done | System `Form`; `NavigationSplitView` on iPad (8) |
| §1.12 | Server-side prefs: whole-document save, revert on failure, flush | — | 3 | done | A pending save flushes when the scene goes to the background |
| §1.12 | Profile header and picture | Settings › Profile | 3 | done | `PhotosPicker`; adds a device name field (spec §8.1) |
| §1.12 | Change password | Settings › Profile | 3 | done | `.newPassword` with the strong-password offer (Q18) |
| §1.12 | Audio language, default track, remember audio | Settings › Playback | 3 | done | |
| §1.12 | Maximum bitrate; lower on a slow connection | Settings › Playback | 3 | done | Applied in phase 4b; adds a cellular limit (Q11) |
| — | Cellular streaming limit (Q11) | Settings › Playback › Maximum quality on cellular | 4b | done | iOS only. Default Original (no limit of its own); on cellular (`NWPath` uses cellular or is expensive) the lower of it and the user's cap goes out (`request_cap` in ffi/src/link.rs). A move to cellular mid-stream loads again at the place when the stream is over the limit (`cellular_step`, toast "Cellular: switched to X"); back on Wi-Fi the limit lifts at the next load |
| §1.12 | Autoplay, Up next card, skip lengths | Settings › Playback | 3 | done | |
| §1.12 | SyncPlay settings; allow remote control | Settings › Playback | 6 | done | |
| §1.12 | Subtitle language, mode, remember | Settings › Subtitles | 3 | done | |
| §1.12 | Subtitle appearance (size, colour, background, position) | Settings › Subtitles | 3 | done | Applied in phase 4b |
| §1.12 | Home sections 1–10 and swap | Settings › Home | 3 | done | |
| §1.12 | Trailers in the hero; Liquid glass switch | — | — | dropped | No hero trailers (spec §8.2); no glass switch, the glass is the system's (spec §8.6) |
| §1.12 | Quality tags switch, Enhanced per-feature switches | Settings › Display | 7 | done | `EnhancedSettingsPage`: quality tags (own value over the server's) and a switch per Enhanced feature (`enhancedOff`), the custom subtitle style among them (`subtitle_style`: off, the look comes from the own setting and the system's caption style alone; applies at the next playback) |
| §1.12 | Downloads: storage limit, 1 or 2 at a time, space, Open Downloads, denied message | Settings › Downloads | 5 | done | 1 or 2 → `httpMaximumConnectionsPerHost`; adds "Download over cellular" (Q11) |
| §1.12 | Hidden content list | Settings › Hidden content | 7 | done | |
| §1.12 | About: logo, version, licence, credits | Settings › About; Account sheet › About | 3 | done | Adds mpv/FFmpeg LGPL and "Export diagnostics" |
| §1.12 | About: Updates card | — | — | dropped | Sparkle is Mac-only (spec §12) |

### 1.13 Server dashboard (after M1)

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §1.13 | Entry, gated by is_admin | Account sheet › Dashboard (large sheet); key GoToDashboard on iPad | 9 | done | Q8 subset first: Dashboard, Users, Tasks, Logs |
| §1.13 | Section list | Dashboard sheet | 9 | done | `NavigationSplitView` on iPad |
| §1.13 | Skeleton, failed load with cause, action toasts | Every section | 9 | done | Swift owns the sentences |
| §1.13 | Live reload every 5 s | Dashboard, Tasks, Libraries | 9 | done | Stops in the background (spec §8.10) |
| §1.13 | Confirms, prompts, Secret dialog, text editor; find field | Every section | 9 | web | Red confirms (spec §8.8); `.searchable` |
| §1.13 | JSON editors with `Draft`; save bars; leave guards | Editors | 9 | web | Never typed (spec §8.8) |
| §1.13 | Server card, Scan All Libraries | Dashboard › Dashboard | 9 | done | |
| §1.13 | Restart, Shut Down | Dashboard › Dashboard | 9 | done | Red confirm; never in a test |
| §1.13 | Count tiles, running tasks, active devices, activity, alerts, storage, paths | Dashboard › Dashboard | 9 | done | Paths in Geist Mono |
| §1.13 | Users list, counts, New user | Dashboard › Users | 9 | done | `.newPassword` (Q18) |
| §1.13 | User page: facts, Change password, Enable/Disable | Dashboard › Users › user | 9 | done | |
| §1.13 | Delete user | Dashboard › Users › user | 9 | done | Red confirm; never in a test |
| §1.13 | Access editor | Dashboard › Users › user › Access | 9 | done | |
| §1.13 | Libraries: cards, Scan | Dashboard › Libraries | 9 | done | |
| §1.13 | Config pages: General, Library Display, Metadata, NFO, Resume, Streaming, Trickplay | Dashboard › section | 9 | web | |
| §1.13 | Transcoding and Networking (danger confirm on save) | Dashboard › section | 9 | web | Never saved on the live server |
| §1.13 | Branding: disclaimer, splash, CSS, Upload image, Remove | Dashboard › Branding | 9 | done | Upload from Photos (`photosPicker`, HEIC goes as JPEG) or Files (`fileImporter`: PNG, JPEG, WebP, GIF, 10 MB); Remove is a red confirm; preview of `/Branding/Splashscreen`. testDashboard uploads and removes on the container (`BLOOM_TEST_SPLASH`) |
| §1.13 | Devices: list, Delete | Dashboard › Devices | 9 | done | Red confirm; never in a test |
| §1.13 | Activity | Dashboard › Activity | 9 | done | |
| §1.13 | Scheduled Tasks: list, Run, Stop, triggers editor | Dashboard › Scheduled Tasks › task | 9 | done | |
| §1.13 | Plugins: cards, menu, settings editor | Dashboard › Plugins | 9 | done | |
| §1.13 | Logs: list, viewer | Dashboard › Logs › file | 9 | done | Geist Mono |
| §1.13 | API Keys: list, Copy, Revoke, New | Dashboard › API Keys | 9 | done | Revoke is a red confirm |
| §1.13 | Metadata editor: Details, Images, Identify, Refresh | Card menu / Detail › More › Edit metadata… | 9 | web | iPad first (spec §8.8) |

## 2. Popups, menus, panels, dialogs

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §2 | Menu kit (radio, check, detail, icon, submenu, phone sheet) | System `Menu` and `Picker` | 3 | done (native) | |
| §2 | Card menu by long press, release does not open | `.contextMenu` with a poster preview | 3 | done | Right click and hover More on iPad (8) |
| §2 | Card menu › Play, played, favourite, remove from a home row | Card menu | 3 | done | Play works from phase 4a |
| §2 | Card menu › Add to playlist / collection, Remove from collection; the Add to dialog | Card menu | 7 | done | With the lists request block (spec §16.1) |
| §2 | Card menu › group and session queue entries | Card menu | 6 | done | |
| §2 | Card menu › download entries | Card menu | 5 | done | |
| §2 | Card menu › Hide entries | Card menu | 7 | done | |
| §2 | Card menu › admin metadata entries | Card menu | 9 | web | |
| §2 | Card menu › Open in Jellyfin Web | Card menu | 7 | done | Plan phase 7: "the card menu's remaining entries"; plus `ShareLink` |
| §2 | Card hover overlay | iPad › hover | 8 | dropped (Mac) | `.hoverEffect` (spec §6.4) |
| §2 | Card badges: watched, unplayed, progress, quality tags | Cards | 3 | done | VoiceOver value per spec §7.3 |
| §2 | Seerr card menu (status, Request, Open on TMDB) | Seerr card | 7 | done | A request needs an explicit confirm; never in a test |
| §2 | Account menu › profiles, Random item, Settings, Quick Connect, About, Sign out | Account sheet | 3 | done | |
| §2 | Account menu › Dashboard | Account sheet | 9 | done | |
| §2 | Account menu › Play on, SyncPlay | Account sheet | 6 | done | |
| §2 | Account menu › Downloads | Downloads tab | 5 | done (native) | A tab replaces the entry |
| §2 | Account menu › Trailer backdrops, Quality tags (Mac) | — | — | dropped | Mac-only; quality tags in Settings › Display |
| §2 | Account menu › Enhanced submenu (feature toggles) | Settings › Display | 7 | done | Not in the sheet (spec §6.1) |
| §2 | Panels; one popup at a time | Sheets with detents | 3 | done (native) | One presented value per scene (spec §6.3); each panel lands with its feature |
| §2 | Dialog shell (scrim, keyboard inset, entrance) | — | 3 | done (native) | System sheets, alerts and keyboard avoidance |
| §2 | Downloads queued at once become one toast | Toast | 5 | done | |

## 3. Player

### 3.1 Engine and platform

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.1 | libmpv engine | Player (`fullScreenCover`) | 4a | done | `MpvEngine` in Swift on the sample-buffer layer; Metal fallback (S2) |
| §3.1 | Video under the UI; glass over the video (frame copy) | Player | 4a | done (native) | The video is a normal layer the controls blur; no `screenshot-sw` copy |
| §3.1 | Mac render thread, GL surfaces, display clock; fullscreen | — | — | dropped | Mac-only; the player is a full-screen cover |
| §3.1 | Keep-awake assertion | — | 4a | done | Idle timer off while video plays (spec §8.3) |
| §3.1 | Mac PiP (window shrunk); small-window button | Small player over the pages; system Picture in Picture outside Bloom | 4d | done | Daniel's choice (2026-11-14): both; MPVUI sample-buffer output |
| §3.1 | Nothing on the mpv thread waits for the network; picture choices; TLS and token rules | — | 4a | done (native) | Rules of `MpvEngine`; S2 measures 4K |

### 3.2 Layout and controls

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.2 | Full-bleed video, header, floating glass panel | Player | 4a | done | Bar glass from S3 (Q13) |
| §3.2 | Hide after 2.5 s; a tap toggles the controls or closes a panel | Player | 4a | done | They stay under VoiceOver; a double tap on a side third skips (new) |
| §3.2 | Mac pointer shows controls, click pauses | — | — | dropped | Mac-only; iPad hover (8) |
| §3.2 | Spinner, Back (stops the player), title | Player | 4a | done | Back stops (Q12) |
| §3.2 | Seek row; drag moves the preview, release seeks | Player | 4a | done | `.adjustable`; haptic on a chapter crossing |
| §3.2 | Rewind, Play/Pause, Forward | Player | 4a | done | Magic Tap plays and pauses |
| §3.2 | Previous (restart, or previous item in the first 5 s), Next | Player | 4c | done | |
| §3.2 | Rating chip and community rating ★ | Player | 7 | done | Phone: no; iPad regular width: yes (spec §16.4) |
| §3.2 | Ends at, favourite button | Player | 4a | done | Phone: no; iPad regular width: yes (spec §16.4) |
| §3.2 | Mute and volume slider | Player | — | dropped | The hardware buttons are the volume; a remote "mute" mutes the player (spec §16.4) |
| §3.2 | Subtitles button, Settings gear | Player › Subtitles / gear | 4b | done | |
| §3.2 | Bookmarks button | Player › Bookmarks | 7 | done | Phone: no (bookmarks panel from the gear); iPad regular width: yes (spec §16.4) |
| §3.2 | Episodes button | Player › Episodes | 4c | done | |
| §3.2 | SyncPlay chip and button; Play on button | Player | 6 | done | AirPlay route picker in phase 4d |
| §3.2 | Phone trims, 44 pt controls, safe area | Player | 4a | done (native) | The trim list of spec §16.4 |
| §3.2 | Landscape on open on a phone; the iPad keeps its orientation | Player | 4a | done | `requestGeometryUpdate`; S3 checks the iPad is not windowed |
| §3.2 | Player keyboard (Space/K, arrows, J/L, M, E, Shift-N/P, Esc, Z, Ctrl-−/=) | iPad › keyboard; Playback commands | 8 | dropped (Mac) | ↑/↓ volume and F fullscreen have no iOS target |

### 3.3 Timeline

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.3 | Chapter ticks, trickplay preview, text box, preload near resume | Seek bar | 4a | done | No preload in serious thermal state or Low Power Mode (4d) |
| §3.3 | Bookmark diamonds | Seek bar | 7 | done | |

### 3.4 Media segments

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.4 | Skip Intro / Credits / Recap / Preview button; yields to a panel | Player | 4c | done | Announced to VoiceOver; haptic |
| §3.4 | Auto-skip once per segment, toast; its Enhanced flag | Player | 4c | done | Never in a SyncPlay group |
| §3.4 | Segments stored for offline | — | 5 | done | |

### 3.5 Up next, autoplay, queue

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.5 | Up next card, autoplay, close at the end | Player | 4c | done | Announced to VoiceOver |
| §3.5 | Queue (episodes after, Play All, Shuffle, playlists, series expand) | Player | 4c | done | Copied from `queue.rs` before phase 4 |
| §3.5 | Lost episode re-find on 404 | Player | 4c | done | |
| §3.5 | A full-screen window ends with the player (Mac) | — | — | dropped | Mac-only |

### 3.6 Resume

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.6 | Position reported so other devices resume | — | 4a | done | Resume buttons are under §1.7 |

### 3.7 Pause screen and info

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.7 | Pause screen after 5 s | Player | 4c | done | Style settings in phase 7 (plan) |
| §3.7 | Playback info card; mpv stats overlay | Player › gear › Playback info | 4c | done | The spec names only "playback info" |

### 3.8 Audio

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.8 | Audio track menu; language preference in a transcode | Player › gear › Audio | 4b | done | Default also from the system's Audio Descriptions setting |
| §3.8 | Audio delay submenu | Player › gear › Audio delay | 4b | done | Hidden on the AirPlay engine |

### 3.9 Subtitles

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.9 | Subtitle menu (Off, tracks, Burned in, Loading…) | Player › Subtitles | 4b | done | Default also from the system's SDH setting |
| §3.9 | Image subtitles as files, fetched when chosen, id map, burn-in once | Player › Subtitles | 4b | done | Burned in on the AirPlay engine |
| §3.9 | Text subtitles of a transcode fetched apart | — | 4b | done | |
| §3.9 | Timing submenu, per-item delay, toast | Player › Subtitles › Timing | 4b | done | Hidden on the AirPlay engine |
| §3.9 | Search for subtitles… panel | Player › Subtitles › Search for subtitles… (sheet) | 7 | done | Writes to the server; on the deny list (spec §16.3) |
| §3.9 | Look mapped to mpv `sub-*`; Enhanced custom style | Settings › Subtitles › Appearance | 4b | done | Order: own setting, system caption style, Enhanced (spec §7.3), per value (core-side `subtitle_properties`, the plugin's presets in `enhanced.rs`). A system value counts when its style overrides the video (`useValue`) |

### 3.10 Quality, bitrate, transcoding

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.10 | Quality menu, ladder, 1.5× codecs, choice persists | Player › gear › Quality | 4b | done | Core `bitrate` |
| §3.10 | Adaptive Auto; BitrateTest on reconnect | — | 4b | done | Stalls step Auto down a rung (`StallWatch` over core's `Stepper`; the cap holds until a new measurement); a new network path (`NWPathMonitor`) drops the old value and measures again once nothing plays; no test on cellular or Low Data Mode (spec §16.8). Step back up while the item plays by the Mac's rule (`StallWatch.step_up`, asked every 10 s): ten minutes without a stall (a hiccup counts) since the step, the hold-off over, a measurement newer than the step, and the link deciding a higher cap; toast "Better connection: switched to X". The measurement runs only while nothing plays (a pause or the end of an item after the step down), as on the Mac. Never in a SyncPlay group, under a manual quality or on a downloaded file |
| §3.10 | PlaybackInfo with the device profile; remux is a transcode | — | 4a | done | HDR ranges from the device (4d) |
| §3.10 | Transcode ends on stop and on quit | — | 4a | done | No quit on iOS: the next launch sends Stopped (spec §8.10) |
| §3.10 | Speed 0.5–2× | Player › gear › Speed | 4b | done | Locked in SyncPlay (6) |

### 3.11 Playback reporting

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.11 | Start, Progress, Stopped, in order | — | 4a | done | Background report inside `beginBackgroundTask` |
| §3.11 | Quitting during playback sends Stopped | — | 4a | done | Next launch sends Stopped; Stopped after 10 min paused in the background |
| §3.11 | Offline positions, flushed on reconnect | — | 5 | done | |

### 3.12 Episode picker

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.12 | Season chips, rows, Now playing, opens at the current one, tap plays | Player › Episodes | 4c | done | |
| §3.12 | Phone sheet at the right; iPad card over the bar | Player › Episodes | 4c | done (native) | Trailing sheet in landscape (spec §6.3); iPad layout in phase 8 |

### 3.13 Jellyfin Enhanced

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.13 | Feature flags read from the plugin | — | 7 | done | Switches under §1.12 |
| §3.13 | Pause when hidden / resume on return | — | 4a | dropped (Mac) | Replaced by the background rule of spec §8.10 |
| §3.13 | Bookmarks: plugin storage, panel (label, Add, rows, jump) | Player › Bookmarks (sheet) | 7 | done | |
| §3.13 | Keys: aspect, playback info, cycle tracks, speed, skip, bookmark | iPad › keyboard | 8 | dropped (Mac) | Pinch fit/fill on touch (4a) |
| §3.13 | Keys: frame step | iPad › keyboard | 8 | dropped (Mac) | mpv engine only |
| §3.13 | Keys: jump to last position, jump to percentage; page keys | iPad › keyboard | 8 | dropped (Mac) | |
| §3.13 | Help panel "Keyboard shortcuts…" | — | — | dropped | iPad: the menu bar and hold-⌘ list the keys (spec §16.5) |

### 3.15 Now Playing and media keys

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §3.15 | Now Playing info | Lock Screen, Control Center, Dynamic Island | 4d | done | iOS 27 Now Playing framework, never mixed with `MPNowPlayingInfoCenter` |
| §3.15 | Remote commands (play, pause, next, previous, skip, seek) | Lock Screen, headphones, Watch | 4d | done | |
| §3.15 | Commands go to the cast target | — | — | dropped | Spec §8.4: Lock Screen control of a cast is not in v1 |
| §3.15 | Audio session `.playback` / `.moviePlayback`; background audio | — | 4a | done | Interruptions and route loss per spec §8.10 |

## 4. Remote and cast

### 4.1 Play on panel

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §4.1 | Sections This device, Jellyfin devices, Chromecast | Account sheet › Play on; Player › Play on (sheet) | 6 | done | |
| §4.1 | AirPlay row | Play on sheet | 6 | done | `AVRoutePickerView`; the engine is phase 4d |
| §4.1 | With a target: state, seek bar, transport, volume, tracks | Play on sheet; bottom accessory | 6 | done | |
| §4.1 | Play here instead | Play on sheet | 6 | done | |
| §4.1 | Every play goes to the target; toast | — | 6 | done | |
| §4.1 | `cast_off` on every session change | — | 6 | done | `SessionScope` |

### 4.2 Controlling another Jellyfin client

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §4.2 | Controllable sessions, from the socket or a poll | Play on sheet | 6 | done | Polled only while the Play on sheet is open or a session is the target, foreground only (spec §16.7) |
| §4.2 | Play, Playstate and General commands; optimistic state | Play on sheet | 6 | done | |

### 4.3 Being controlled

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §4.3 | Capabilities on socket open; mute, track index, DisplayMessage | — | 6 | done (mock) | `post_capabilities` (ffi/src/socket.rs): Mute, Unmute, ToggleMute, DisplayMessage; track index waits for the server-to-mpv map |
| §4.3 | Volume and ToggleFullscreen commands | — | — | dropped | Spec §8.4: not system volume or full screen |
| §4.3 | Play messages, Rewind / FastForward, DisplayMessage toast, facade | — | 6 | done | PlayNow and the Playstate commands work (testRemoteControl); PlayNext/PlayLast go to the player's queue (PlayNow when nothing plays); Previous, SetAudioStreamIndex, SetSubtitleStreamIndex reload at the place; SetVolume is the user's on iOS; a play in a group goes to the group |

### 4.4 Chromecast

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §4.4 | Bonjour discovery while the sheet is open | — | 6 | done | `NWBrowser` handed to core; Info.plist keys (spec §8.4) |
| §4.4 | TLS session, pinning, reconnects, seek coalescing | — | 6 | done | The Rust session stays |
| §4.4 | Jellyfin receiver and Default Media Receiver; track change | Play on sheet | 6 | done | |
| §4.4 | Mock device | — | 3 | done | Forced in test mode (spec §10) |
| §4.4 | Local network denied; casting with the phone asleep | Play on sheet | 6 | done | Never "offline": the list and a chosen device that does not answer say "Bloom may not use the local network" with Open Settings (`LocalNetworkCheck`, `CastControls`); a failure that is not the permission reads "Not connected: …". Away: background time, then at return `refresh()` reads the state and a dead socket reconnects (`CastLife`). testLocalNetworkDenied; testChromecast drops the fake device's socket at the background (`BLOOM_CAST_DROP_AWAY`, `cast_mock_drop_connections`) and checks after the return that the device is connected again and a mute reaches it (the fake Jellyfin app reports its item only on a command, so the transport comes back with the real app's own reports, not in the test). The Default Media Receiver's reports stop while away (known loss) |

### 4.5 AirPlay

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §4.5 | AVPlayer over the server's HLS; reports; track change re-sends | Player (AirPlay engine) | 4d | done | Reload at the position when a route is picked |
| §4.5 | Route picker | Player › AirPlay; Play on sheet › AirPlay | 4d | done | Hidden when `AVRouteDetector` finds no route |
| §4.5 | Hardening (deferred commands, seeks, last half second, mute, failed route) | — | 4d | done | MPVUI's external playback keeps the play/pause asked while it prepares, restarts or replaces seeks (the last one wins), and hands the receiver the player's volume and mute at each start; a failed route returns the picture to mpv. New: the failure is told as a toast (`AirPlayNotice`, Cast.swift), not only in the AirPlay view that is gone by then. Differs: MPVUI calls a pause in the last 100 ms an end (the Mac's engine: a user pause in the last half second is not); not changed, as that is MPVUI code. No UI test: the simulator has no AirPlay route |

### 4.6 SyncPlay

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §4.6 | Access from the policy; panel only with a socket | — | 6 | done | |
| §4.6 | Not in a group: groups, join, New group | Account sheet › SyncPlay; Player › SyncPlay (sheet) | 6 | done | |
| §4.6 | In a group: state, queue, shuffle, repeat, this player, leave, stats | SyncPlay sheet; bottom accessory | 6 | done | Queue edits are on the deny list |
| §4.6 | Toasts | Toast | 6 | done | |
| §4.6 | Core engine, drift, server clock; leaving gives back speed | — | 6 | done | Behind `SyncPlayer`; mpv only, an AirPlay route is refused |
| §4.6 | Interruption and suspension in a group | — | 6 | done | New on iOS: "this player waits", rejoin at return (spec §8.4) |

### 4.7 Server socket

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §4.7 | WebSocket, KeepAlive, backoff, dispatch | — | 3 | done (mock) | Closes on suspension, reconnects at return (spec §8.10) |
| §4.7 | News reloads Home, Library, the index | — | 3 | done (mock) | Home, the libraries and an open library grid reload (3 s quiet, 15 s at most, never under the player); the grid loads its items again in one request in place and keeps its scroll position; a random order keeps its page. Test: `testLibraryNews` |

## 5. Other features

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §5.1 | Servers and profiles in config; token in the Keychain | — | 3 | done | `SecretStore`; device id in the Keychain too |
| §5.2 | Liquid glass with the Mac's tuning; frosted fallback | — | — | dropped | Spec §7.2: system Liquid Glass; the tuning stays on the Mac |
| §5.2 | Phone popups frost fully | — | S | done (native) | Left to the system; S3 shows a sheet over posters |
| §5.4 | Automation events socket, bloom-lights, Settings › Automation | — | — | dropped | Mac-only: a Unix socket and a lamp helper (spec §16.6) |
| §5.5 | Debug channel and its switch | Test hook; Safe mode (debug builds) | 3 | done | Simulator socket or a 1 pt element (spec §10, §11) |
| §5.6 | What downloads; gated by policy | Detail; card menu | 5 | done | Original file only |
| §5.6 | Per-server store, `.part` resume, stalls, disk full, changed file | — | 5 | done | Shape from S4 (background `URLSession`); backup excluded |
| §5.6 | States Queued to Done | Downloads tab | 5 | done | |
| §5.6 | Offline playback with sidecars and segments; offline detail page | Downloads tab › item | 5 | done | `ImageLoader` reads local files |
| §5.6 | Other server refused; index recovery; ids checked as paths | — | 5 | done | |
| §5.7 | Hidden file read and write; when active | — | 7 | done | |
| §5.7 | Surfaces filtered | Home, Library, Search | 3 | done | `HiddenTitles.visible(_, on:)` on the library grid, Search, the detail page's seasons and similar titles; Next Up drops a hidden series |
| §5.7 | Scopes; Hide entries; a hidden series hides its episodes | Card menu › Hide; Detail › More › Hide | 7 | done | |
| §5.8 | Seerr card status, tap opens | Seerr card | 7 | done | |
| §5.8 | Request more seasons dialog | Detail › Request more seasons (sheet) | 7 | done | Confirm; never in a test |
| §5.9 | Ratings: critic %, official rating chip | Detail; Player | 7 | done | The official rating chip on the detail page is phase 3 |
| §5.10 | Collections permission; delete guard | — | 7 | done | |
| §5.11 | Mac menu bar: Bloom, File, Window, Help | — | — | dropped | Mac-only (inv §5.11) |
| §5.11 | Mac menu bar: Edit | — | 3 | dropped (Mac) | System text editing and edit menu |
| §5.11 | Mac menu bar: View, Playback | iPad › menu bar | 8 | dropped (Mac) | `.commands`; PiP in Playback |
| §5.11 | Trackpad swipe, window frame memory | — | — | dropped | Mac-only; the system edge swipe |
| §5.11 | Layered app icon | — | 11 | partial | Icon Composer `.icon` |

## 6. iOS behaviour already designed

Most items of inventory §6 already have a row above. These are the ones that do not.

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §6 | Three posters across, gap 14 | Grids and rows | 3 | done (native) | `containerRelativeFrame` |
| §6 | Detail one column, actions row, 96 pt cast tiles, wide episode stills | Detail | 3 | done (native) | |
| §6 | 44 pt targets; 50 pt fields with 17 pt text; Dynamic Type | — | 3 | done (native) | Text styles replace fixed sizes (Q2) |
| §6 | Player popups open upward, above the bar | Player menus | 4b | done (native) | System `Menu` placement |
| §6 | Each field names its keyboard (URL, username, password, code, search, name, prose); dark keyboard | — | 3 | done | `textContentType`, `keyboardType`, `submitLabel` |
| §6 | Edit menu on long press; predictions and autocorrect work | — | 3 | done (native) | System fields; a password is never copied |
| §6 | The Passwords bar does not leak onto search | — | 3 | done (native) | Device checklist: both walks |
| §6 | VoiceOver roles and labels | — | 3 | done | Every phase; stable `accessibilityIdentifier`s |
| §6 | Motion: push slides, Back fades, sheets rise | — | S | done (native) | System motion replaces it (Q14, S3) |
| §6 | iPad: `UIRequiresFullScreen` true, no geometry request | iPad | 8 | done | Changed: resizable, no `UIRequiresFullScreen` (spec §4) |
| §6 | No phone build before a clean simulator walk | — | 3 | done (native) | Rule of every phase |

## 7. Design rules

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §7 | Dark only, page #101010; no gradients, no shadows | — | 3 | done (native) | `UIUserInterfaceStyle = Dark` |
| §7 | One ink; colour only for state; one white primary | — | S | done (native) | Ink as tint checked in S3 |
| §7 | Smoked glass on every popup, sheet and bar; popups share one look | — | — | dropped | Spec §7.2: system glass, menus and sheets; custom glass on the player controls and the toast only |
| §7 | Type: Google Sans, Geist Mono | — | 3 | done | SF text styles; Google Sans for display titles (Q2); Geist Mono for the code, logs, paths |
| §7 | Shapes, spacing, terminology, copy | — | 3 | done (native) | String Catalogs; "this device" |
| §7 | Motion: one curve, 150/200 ms, Arrival and Quiet Return rules, no spring | — | S | done (native) | System motion for push, pop and sheets (Q14); own motion keeps the curve |
| §7 | Performance budget | — | 3 | done (tooling); device numbers pending | Scroll hitch metrics on the iPhone Air replace `frame_cpu`: `Performance` (Home, a row, the library grid, search results: hitch time ratio under 5 ms/s; cold launch to the first frame 400 ms; detail page and player watched). `dev/ios-next perf` (simulator: no hitch metric, numbers printed only), `dev/ios-next perf-device` (the phone, against the mock on the Mac's address; checks the budget from the result bundle). README §18, "Performance budget" |

## 8. Safety rules for tests

| Inv § | Item | iOS path | Phase | Status | Notes |
|---|---|---|---|---|---|
| §8 | Writes only with revert; the never-in-a-test list | — | 3 | done | Writes on the container; the random walk's deny list (spec §10) |
| §8 | Playback muted, no reports, socket off; never print the token | — | 3 | done | Test mode enforced in core |
| §8 | Cast devices: discover and read only | — | 6 | done | The mock is forced in test mode |
| §8 | Never send a Seerr request | — | 7 | done | |
| §8 | No synthetic input on the Mac; a test instance does not take the focus | — | S | dropped (Mac) | S5 picks the simulator's input path; the focus rule is Mac-only |
| §8 | Test account, server choice, facts to verify | — | 3 | done (native) | uitest bundle id, mock or container only |
