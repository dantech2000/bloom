# Bloom feature inventory (for the native iOS rebuild)

Snapshot of `main` at 4e08d29 (2026-11-13). Read-only survey; nothing in the repo changed.
Paths are relative to the repository root (`/Users/drodriguez/github.com/bloom`).
"Phone" = compact window (`shell::compact`, width under 700 pt) for layout, and `Bloom::phone()` (short side under 500 pt) for player trims.
"Mac/iPad" = the desktop layout. The iPad has had no layout pass of its own yet.
"core" = `bloom-core` (no window, reusable from Swift). "src" = the GPUI app.

Sources: CLAUDE.md, DESIGN.md, PRODUCT.md, README.md, docs/releases/0.1.1–0.1.12, specs/graphical-subtitles.md, specs/outbound-events.md, and the code.

---

## 0. App skeleton

- Root view `Bloom` (src/app.rs).
  - `Screen {Connect, Main}`.
  - `Page {Home, Library, Detail, Search, Admin, Settings, Playlist, Downloads}`.
  - `Panel {Episodes, Help, SyncPlay, Cast, Subtitles, Bookmarks, Seasons}`.
  - `PlayerUi` holds the player's UI state: `playback.open` is the player overlay, which covers every page.
- Navigation (`Navigation`, src/app/navigation.rs):
  - Back and Forward history, each entry with its scroll place.
  - Mac: Cmd-[ / Cmd-], a two-finger trackpad swipe, three-finger swipe and mouse side buttons (src/swipe.rs).
  - Phone: the top-bar back arrow.
- Toasts (src/ui/toast.rs). Every failure goes to a toast with a title and a plain-words description.
- Page loaders live in core (`core/src/pages.rs`: `load_home`, `load_detail`, `load_search`, `library_query`, `catalog_query`). Each runs its requests in parallel.
- Session guards: `SessionEpoch` / `fetch_in_session` drop an answer that arrives after a sign-out or an account switch. A native app needs the same guard.
- Live updates come over the server socket (§5.3).
- Media types: video only. `library_views` drops the `music`, `books`, `livetv` and `playlists` collection types. Home "Recently Added" rows cover movies, tvshows, homevideos and mixed. No music, books or Live TV UI exists.

---

## 1. Screens and pages

### 1.1 Splash — src/views/splash.rs
- The logo blooms with a glow and a ring swell (700 ms / 1000 ms). The word "Bloom" fades in at 350 ms over 450 ms, holds, then the whole layer fades out (300 ms) over the page that loaded behind it. Total 1.5 s from the first frame.
- It never adds a wait: the page under it (Connect or Home) loads at the same time.
- Reduce Motion makes it still. `BLOOM_NO_SPLASH=1` skips it.

### 1.2 Connect / sign-in — src/views/connect.rs, src/app/sessions.rs
- **No server discovery.** The user types the address; there is no UDP 7359 and no Bonjour.
- **Server list** ("Connect to a server" / "Enter the address of your Jellyfin server."):
  - "Your servers" rows: name · N profiles. "Remove this server" needs two taps ("Click again to remove"). The removal revokes every profile's token on the server.
  - "Server address" field (URL keyboard) and the button Connect / "Connecting…".
  - `normalize_url` (default `http://`, trailing slash trimmed), then `GET /System/Info/Public`, then `upsert_server`.
  - Errors from `explain()`:
    - the proxy says the server is offline (502/503/504/52x/530);
    - no answer (DNS, connect, timeout, TLS);
    - "answered, but not as a Jellyfin server";
    - 403 firewall; 404 "No Jellyfin server was found"; 5xx; a generic message.
- **select_server** fetches in parallel: branding (login disclaimer, splash screen enabled), whether Quick Connect is on, and the public users.
- **Sign-in page**:
  - Title "Who is watching?" when the server has saved profiles, else "Please sign in". Under it, the server name or host.
  - The background is the server's splash-screen image when its branding enables one. The login disclaimer text shows.
  - Saved profiles are avatar tiles; one tap opens the session. Each tile has an X, "Forget this profile" (two taps), which logs the token out on the server.
  - "Users on this server": public users not yet saved. A tap fills the username, then focuses the password field, or signs in at once when the user has no password.
  - "Sign in as another user": Username and Password, then "Sign In" / "Signing in…" (`AuthenticateByName`).
  - Errors: "Enter a username."; "Wrong username or password." (any 401); or the network explanation.
  - "Use Quick Connect" shows only when the server enables it.
  - "Change server" and "Remove server" (two taps).
- **Quick Connect sign-in**:
  - Shows the 6-digit code with a wider gap in the middle, and the text "On a device where you are signed in, open Quick Connect in the user menu and enter this code."
  - "Waiting for the approval…" with Cancel. Polls every 3 s and expires after 600 s ("The code expired…"). A 404 also means expired.
  - Other messages: "Quick Connect is off on this server."; a failed sign-in after the approval says so.
- **Quick Connect authorize** (approve another device): `open_authorize`.
  - Reached from the account menu "Quick Connect" and Settings › Profile "Enter code".
  - A code field with the check "The code has 6 digits."
  - Errors: 404 "The server does not know this code…"; 401/403 "did not accept"; unreachable. Success toast "Device signed in".
- **open_session** (sessions.rs:178) on success:
  1. Upsert the profile and build the client (token + device id).
  2. Stop any cast and bump the epoch.
  3. Start the download engine and load the user prefs.
  4. Load the search title index after 2 s.
  5. Open the socket and measure the link speed.
  6. Load the playlists, `user_settings` (is_admin, audio language, SyncPlay access), the Jellyfin Enhanced settings and the quality-tag prefs.
  7. Save `active`, then go Home.
- **Multiple servers and accounts**: the account menu lists every profile of every server under a server label; a tap switches. "Servers & profiles…" opens the connect screen with a "Back to {user}" pill (on iOS it sits under the status bar).
- **Sign out**: stops SyncPlay and cast, logs out on the server, forgets the profile, clears history, and shows the connect screen of that server.
- **iOS sizing** (`connect::metrics`): fields and buttons 50 pt tall with 17 pt text, group label 13 pt, quiet link 15 pt on a 44 pt target, card padding 24. The Mac keeps 44 pt fields.
  - The card centres in the room above the keyboard and scrolls to its end while the keyboard slides.
- iOS note from CLAUDE.md: the app shows "Wrong username or password" for any 401. The phone's 401 on 2026-10-22 was Jellyfin with IPv6 off behind Cloudflare, not the app. Keep the classification in mind for error text.

### 1.3 Offline state — core/src/connection.rs, src/connection.rs, src/views/shell.rs
- `classify(err)` → `Unreachable(Dns | Connect | Timeout | Tls | Gateway(502/503/504/520–527/530))`. A 401 means the session is invalid, not that the server is offline. Any other HTTP answer counts as online.
- A failed page request probes the server (5 s). Retry backoff is 5, 10, 30, then 60 s. A failed page reloads by itself at most every 30 s.
- Going offline:
  - toast "{server} is offline", plus "Your downloads play from this device." when there are downloads;
  - opens Downloads automatically when downloads exist;
  - image fetches stop.
- **Offline page**: "Checking the connection to {server}…" or "{server} is offline", the reason, a countdown, and the buttons Retry and Open Downloads.
- **Top-bar chip**: an amber dot, "{server} is offline", Retry, Downloads.
- Recovery:
  - toast "{server} is back";
  - restarts the socket and flushes the offline positions;
  - goes Home if offline mode had opened Downloads, else reloads the page.

### 1.4 Shell: bars and navigation — src/views/shell.rs
- **Mac top bar** (64 pt, transparent over the page):
  - Tabs in a group that gets a glass pill once the page scrolls: Home (the server name with the logo glyph), Favorites, one tab per library (glyph + name), and Playlists (only when the user has a playlist).
  - Right side: the cast chip and the downloads chip when active; "Restart to update"; round buttons Play on (Cast; lit while casting), SyncPlay (lit in a group), Random item (R), Search (/); then the account avatar, which opens the account menu.
  - The iPad under 900 pt shows the tabs as glyphs alone.
- **Phone tab bar** (`render_tabbar`):
  - A 62 pt glass capsule with 24 pt corners, 10 pt above the home indicator.
  - Entries: Home, up to 3 libraries, Favorites (only when there are 2 libraries or fewer), Search.
  - Glyph over a 12 pt label; the label's line box is 12 pt so the pair centres (Daniel, 2026-10-23: "slightly top justified").
  - Lights only Search on the search page, and nothing on Settings, Dashboard or Downloads.
  - Steps aside while the keyboard is up.
- **Phone top bar**: back arrow, page title (`page_title`, or `subpage_title` for a user or a log), account disc.
- **Keys** (Mac): Cmd-[ back; Esc closes the SyncPlay or Cast panel, else goes back; Backspace back; `/` search; Shift-H home; R random. On the hero: ←/→ step, Space/P pause the slideshow, M mute the trailer. Plus the Jellyfin Enhanced page keys.
- **Random item**: one random Movie or Series (`open_random`).

### 1.5 Home — src/views/home.rs, src/views/hero.rs, core/src/pages.rs
- **Hero** (`Hero`, after the "Media Bar Enhanced" plugin and the Abyss theme):
  - Up to 20 slides (`HERO_ITEMS`) of items that have a backdrop. 7 s per slide; a new slide cross-fades over 500 ms.
  - Covers 90% of the window height; the rows start at 68%.
  - Content: full-bleed backdrop with a 55% black shade from the bottom; title art (or a bold display title); chips for rating ★, year, age rating, "N Seasons" and "Ends at"; the genres ("No Genre Available"); the overview (ellipsis on a phone).
  - Buttons: More info (glass disc), the white Play, and Add/Remove favorites. Previous and Next slide arrows. A position counter with a thin progress bar at the right.
  - **Trailer backdrop** (Mac only): a second mpv plays the best YouTube trailer muted behind the slide (yt-dlp). The slide waits up to 5 s for the trailer, then lasts until it ends. Mute/Unmute trailer (M) and pause/resume the slideshow (Space).
    - Setting "Trailers in the hero" / "Trailer backdrops" (`config.hero_video`, default on).
    - Not on iOS: MPVKit has no Lua/yt-dlp.
  - Open on the phone (CLAUDE.md 2026-10-23): a swipe to step the hero.
- **Rows**, in the order of the server's `CustomPrefs.homesection0..9` (web defaults when unset):
  - "Continue Watching": wide cards with progress (`client.resume(12)`).
  - "Next Up": wide cards (`next_up(24)`).
  - "Recently Added in {library}": posters, 16 per library.
  - "My Media": library tiles.
  - Rows outside the view (±200 pt) are not built.
- Empty state: "Nothing here yet" / "Start watching something and it will show up on your home screen."
- Rows scroll sideways: Mac arrows at the right of the heading glide nearly one view width; a phone pans with a finger (no arrows).

### 1.6 Library — src/views/library.rs, core/src/pages.rs
- Also used for Favorites (the synthetic view `favorites`), a person's titles (`person`), Playlists (the list of playlists), collections (BoxSet) and folders.
- **Views** (`LibraryShow`), in the title menu on the Mac and as a "View" pill on the phone:
  - movies: All / Favorites / Collections;
  - tvshows: All / Favorites / Episodes.
- **Sort** menu: Name, Random, Community Rating, Critic Rating, Date Added, Date Played, Release Date, Runtime; then Ascending / Descending. The button lights when the sort is not the default.
- **Filter** menu: All, Unplayed, Played, Favorites, Resumable. It lights when set.
- **Letter rail** at the right edge: '#' plus A–Z. Shown only when sorted by Name, on the Mac, and with a window at least 610 pt tall. Not on a phone.
- **Pager**: 100 items a page (`PAGE_SIZE`).
  - Previous and Next arrows plus the range ("1–100 of 236") in the toolbar, and again under the grid.
  - A new page lifts in.
- **Play All / Shuffle** pill (`play_all_pill`): queues the first 30 of the current query. A series in the queue expands to its episodes.
- **Collection page** "More" menu: Refresh, and "Delete collection" when `can_collect`.
- Grid of posters (`cards::grid`): 3 across on a phone; on the Mac the count follows the width (5 under 1200 pt, up to 9 at 1920).
- Skeleton while loading. Empty: "No items found" / "Change the filter or the letter to see more."
- Phone toolbar on two rows; tools are 44 pt (`tool_size`).

### 1.7 Detail page — src/views/detail.rs, core/src/pages.rs `load_detail`
- One page for Movie, Series, Season, Episode, Video and the like. A BoxSet, Folder or Playlist opens as a Library or Playlist page instead (`open_item`). A Person opens as a Library of their titles (`open_person`).
- **Header**:
  - Backdrop, title art or name.
  - Episode: a link to its season ("Open the season") and its show ("Open the show").
  - Meta line: year, runtime, rating, "Ends at", and so on.
  - Overview, "Tags: …", external links, credits (Creator(s), Director(s), Writer(s), Studio(s), Genre(s)). The studio column flexes so a list wraps.
  - Jellyfin Enhanced lines (Seerr, Sonarr and Radarr links).
- **Actions**:
  - Play or Resume (white primary), and "Play from the beginning" when resumable.
  - "Watch the trailer": opens the remote trailer URL. On iOS that is an SFSafariViewController sheet (`gpui_ios::present_web`); its X is at top left.
  - Download (§5.6).
  - Mark played / unplayed. Add / remove favorites.
  - More (…) menu: Refresh, Open in Jellyfin Web, Add to playlist / collection, Hide…, and the admin metadata entries.
  - Phone: the actions sit on a row of their own under the name.
- **Tracks** (playable items): rows "Video" (text), "Audio" (menu) and "Subtitles" (menu with Off).
  - The choice applies when playback starts and survives a reload of the page and Back/Forward.
  - Phone: a box wider than the room goes under its label, full width.
- **Rows**:
  - "Next Up" (series).
  - "Seasons" (series).
  - Season: its episodes as a virtual list of rows. Each row has a wide still, "N. Name", runtime, overview and a Play button.
  - Episode: "More from {season}" with the current episode ringed.
  - Bookmarks (Enhanced).
  - "Cast & Crew" and "Guest Stars": person cards, roles merged ("as X / Director"). Phone tiles are 96 pt.
  - "More Like This" (movie, series; 12 items).
  - Enhanced rows "Recommended on Seerr" / "Similar on Seerr".
  - "Request more seasons" button (Seerr, §5.8).
- **Mac**: the floating poster at the left (fixed while the text scrolls). **Phone**: one column, no poster, the hero text the width of the phone.

### 1.8 Search — src/views/search.rs, core/src/search_index.rs
- A local **title index** of every Movie and Series (`catalog_query`, including the original title) answers as the user types, with no request. It reloads after a library change.
- The server search (Movie, Series, Episode; limit 100) runs on Enter or after a debounce. The answer is cached per text.
- Result rows: "Movies", "Shows", "Episodes", then "Discover on Seerr" (when the user has Seerr).
- Empty: "Search your libraries" / "Type a title and press Enter." No result: "Nothing matched “q”."
- The search field has a search keyboard with predictions and a Search return key. The tab bar's Search tab focuses it.

### 1.9 Favorites
- The Library page with the synthetic view `favorites` (Movie + Series, filter IsFavorite). A tab on the Mac; a tab on the phone when there are 2 libraries or fewer.

### 1.10 Playlists — src/lists.rs, src/views/playlist.rs
- The "Playlists" tab shows only when one exists, and lists them as a library page.
- **Playlist page**:
  - Header: "N items", Play, Shuffle, "Rename playlist", "Delete playlist".
  - Rows: place, picture, title, runtime, and the buttons "Play from here", "Move up", "Move down", "Remove from the playlist".
  - Empty: "This playlist is empty" / "Use Add to playlist in the menu of a title."

### 1.11 Downloads page — src/downloads/ui.rs
- Entries grouped by series, or "Movies". Each row: artwork, title, state, size, progress bar, and Play ("from this device"), Pause, Resume / "Try again", Remove.
- Header: "Try the server again" (while offline), "Remove all" (confirm with the total size), "Show in Finder" (Mac only).
- Offline banner: "The server cannot be reached. Your downloads play from this device."
- Empty: "Nothing is downloaded. Use the download button on a movie, an episode or a season."

### 1.12 Settings (user) — src/settings/mod.rs, src/settings/sections.rs
- Sections (`Section::ALL`): Profile, Playback, Subtitles, Home, Display, Downloads, Hidden content (only when on), Automation, About.
- Mac: a 264 pt sidebar, and pills under 860 pt. Phone: a list page, each section pushed with its name in the top bar.
- Server-side prefs (`Prefs`):
  - `/Users/Me` Configuration, saved with `set_user_config`.
  - DisplayPreferences "usersettings" `CustomPrefs`, saved with `set_custom_prefs`.
  - A save writes the whole document, keeps keys the app does not know, and runs one at a time per document.
  - A failed save is reverted with the toast "The setting was not saved".
  - Quick changes are coalesced; a pending save is flushed when the window closes.
- **Profile**:
  - Avatar (96 pt), name, server · "Administrator" · "Signed in {ago}".
  - Profile picture: "Add/Change picture" and "Remove" (PNG/JPEG/WebP/GIF up to 10 MB; `upload_user_image`, `delete_user_image`).
  - Password: "Change password" with Current (only if one is set) and New ("Other devices stay signed in.").
  - Quick Connect "Enter code".
  - Sign out ("Ends this session on the server and forgets it on this device.").
- **Playback**:
  - Preferred audio language (`AudioLanguagePreference`; "Any language" plus the languages table).
  - Play default audio track regardless of language (`PlayDefaultAudioTrack`).
  - Remember audio selections.
  - Maximum streaming bitrate (local `max_bitrate`: Auto or a ladder rung up to 120 Mbps).
  - Lower the quality on a slow connection (local `adaptive_quality`).
  - Play next episode automatically (`EnableNextEpisodeAutoPlay`).
  - Show the Up next card (`enableNextVideoInfoOverlay`).
  - Skip back / Skip forward length (5–30 s; `skipBackLength` / `skipForwardLength` in ms).
  - SyncPlay: Keep in step with the group (`sync_correction`); Delay of this player (−250…+250 ms in 50 ms steps).
  - Remote control: Allow other devices to control this app (`remote_control`).
- **Subtitles**:
  - Preferred subtitle language.
  - Subtitle mode (Default / Smart / Only forced / Always / None, each with help text).
  - Remember subtitle selections.
  - "Appearance in this app" (local `subtitle_look`; each has "From Jellyfin Enhanced"):
    - Text size: Small 0.8, Normal 1, Large 1.3, Extra large 1.6.
    - Colour: White, Yellow, Green, Cyan.
    - Background: Dark box / No box.
    - Position: Bottom 100, Raised 90, High 80.
- **Home**:
  - Section 1–10 → `homesection0..9`.
  - Options: None, Recently Added, My Media, My Media (small), Continue Watching, Next Up. Marked "web only": Continue Listening, Continue Reading, Live TV, Active Recordings.
  - Up/down arrows swap neighbouring slots in one save.
- **Display**:
  - Trailers in the hero.
  - Liquid glass (off = frosted).
  - Quality tags (unset follows the plugin).
  - Jellyfin Enhanced: one switch per feature the server enables (§3.13).
  - No Reduce Motion switch: it follows the system (or `theme.motion.reduced`). `config.dark` exists but has no row (dark only).
- **Downloads**:
  - Location "Show in Finder" (Mac only).
  - Storage limit: No limit, 10, 25, 50, 100, 250 or 500 GB; 5 GB always stays free.
  - At the same time: 1 or 2.
  - Space used / free.
  - "Open the Downloads page". When the server denies downloads: "The server does not allow this account to download files."
- **Hidden content**: the list of hidden titles (§5.7).
- **Automation** (Mac in practice): §5.4.
- **About** (src/credits.rs, src/updates.rs):
  - Logo, "Jellyfin client · version X".
  - AGPL licence "Read"; "Not an official Jellyfin app".
  - Credits groups with links: Built on, Inside libmpv, Look/icons/fonts, Server plugins (Jellyfin Enhanced, Intro Skipper), Studied for protocols, Rust crates.
  - Updates card: Sparkle, Mac only; on iOS it shows the off-reason text.
  - "Debug channel" switch (dev only).
- Controls carry accessibility roles: a choice is a ComboBox named by its value, a checkbox a CheckBox with its label from the key (`settings::control_label`).
- Phone: a wide control goes under its label (`settings::field`, `control_max_w`).

### 1.13 Server dashboard (admin only) — src/admin/
Entry: the account menu "Dashboard" ("Users, libraries, tasks and logs of the server"), or the Enhanced key GoToDashboard. Gated by `session.is_admin`.

- **Sections** (`admin::Section`, sidebar groups):
  - Server: Dashboard, General, Branding, Users.
  - Libraries: Libraries, Display, Metadata, NFO Settings.
  - Playback: Transcoding, Resume, Streaming, Trickplay.
  - Devices: Devices, Activity.
  - Plugins: Plugins.
  - Advanced: Networking, Scheduled Tasks, Logs, API Keys.
- **Shared behaviour**:
  - Loading draws a skeleton.
  - A failed load shows "{Section} could not load", the cause in plain words (`explain_load`: proxy offline / network / sign in again / not an administrator / not on this server version / try later / not understood), and "Try again".
  - Live reload every 5 s for Dashboard, Tasks and Libraries. It skips while playing, offline or busy, and redraws only on change (or once a minute for the "ago" texts).
  - `admin_action` shows a done toast, or "The server refused the change".
  - Confirm dialogs (`ask_confirm`, red when dangerous); value prompts (`ask_value`); multi-field prompts (`Prompt`, Tab moves fields, required fields block); a show-once `Secret` dialog (Copy / Done); a large text editor (`TextEditor`).
  - Dialogs are dropped when the page or session changes.
  - Find field (`find_field`) from 10 entries (Users: more than 6); `nothing_found`.
  - Editors keep server objects as `serde_json::Value` and send the whole object back with only the changed keys merged (`Draft`, `with_changes`). **Do not type them**; the same holds in Swift.
  - Mac: a 236 pt sidebar with a 26 pt title; the section fades in.
  - Phone: the list of sections is a page; a section pushes in; the top bar shows its name. The back arrow closes, in order: the access editor (if unchanged), the user page, the open log, then returns to the list (`admin_back_to_list`).
  - Save bars sit over the tab bar on a phone (`admin_bar_edges`).
- **Dashboard** (dashboard.rs):
  - Server card:
    - badge Shutting down / Restart needed / Update available / Online;
    - chips: product version, OS, architecture, local address;
    - "Scan All Libraries" (becomes "Scanning… N%" while a scan runs);
    - **Restart** (only if `can_self_restart`; danger confirm) and **Shut Down** (danger confirm).
  - Count tiles: Movies, Series, Episodes, Active Streams; plus Collections, Albums, Songs, Music Videos, Books, Trailers when non-zero. 2 per row on a phone.
  - Running Tasks panel (progress).
  - "Active Devices" (last 16 min), badge "N playing":
    - a playing card shows backdrop, avatar, client · device, Paused, Direct Play / Direct Stream / Remux / Transcode, title, transcode details (codecs, Mbps, fps, hardware acceleration, % done, reasons) and progress;
    - a tap opens the item; there is no remote control from here;
    - an idle card shows "Idle · last seen · IP".
  - Activity (last 7 user plays, start and stop merged) and Alerts (last 7 entries with no user).
  - Storage per disk: bar, %, used/free, Network badge; amber at 85% or more, red at 95% or more.
  - Paths.
- **Users** (users.rs):
  - Mac: count pills and "New user". Phone: one count line, and "New user…" as the last row.
  - "Find a user" field.
  - Rows: avatar, name, detail (You · Administrator · Disabled · Hidden · Active X ago), chevron.
  - **User page**:
    - avatar, name, badges;
    - facts: Last active, Last login, Password Set / No password (amber);
    - rows: Access (→ access editor), Change password, Enable/Disable (confirm), **Delete user** (last, red, danger confirm);
    - on your own account these are replaced by a note.
  - New user: Name (required) and Password (optional).
- **Access editor** (access.rs):
  - Account: Administrator (danger confirm), Disabled, Hidden. Locks: your own account; an admin cannot be disabled.
  - Libraries: All libraries, or a checkbox per library. Warns when none is chosen.
  - Playback: Play media, Transcode video, Transcode audio, Remux.
  - Features: Download, Delete media, Manage collections, Manage subtitles, Control other users, Control shared devices; SyncPlay Create+Join / Join / None.
  - Limits: remote bitrate (Mbps, 0 = none), active sessions, failed sign-ins before lockout (−1 = never).
  - Save bar with Discard / Save: re-reads the policy and merges the edited keys. Leaving unsaved asks "Discard the changes?".
  - No parental controls.
- **Libraries** (libraries.rs):
  - Summary line and Scan All.
  - Cards: art, Idle / Queued / Scanning badge with progress, kind, counts, folder paths, "Scan Library".
  - No create, edit or delete.
- **Config pages** (config.rs; generic `Spec` of field kinds Bool, InList, Number, Text, Path, Choice, List, Multiline; `value_button` opens a prompt):
  - General: server name, display language, cache and metadata paths, Enable Quick Connect, parallel scan and image-encode limits.
  - Library Display; Library Metadata (language, country, chapter images); NFO Settings.
  - Transcoding: hardware acceleration and decoders, tone mapping, threads, presets, CRF, throttling, segment deletion… Saving asks a danger confirm.
  - Resume (%, durations); Streaming (internet bitrate limit); Trickplay (interval, widths, tiles, quality…).
  - Networking: ports, HTTPS, base URL, bind addresses, LAN, proxies, remote access and IP filter, IPv4/IPv6, discovery. Saving asks a danger confirm: "can make the server unreachable, also for this app".
  - Save bar: "N changes are not saved." Discard / Save. Drafts survive a section switch.
- **Branding** (branding.rs):
  - Login disclaimer (text editor), Enable splash screen, Custom CSS (mono editor).
  - Splash preview; "Upload image" (≤10 MB); "Remove" (danger confirm).
- **Devices** (devices.rs):
  - Counts.
  - Rows: glyph, active dot, name, "This device", app + version, last user, Active now / ago.
  - **Delete** (danger confirm; not for this device).
- **Activity** (activity.rs): the last 100 entries grouped by day, with counts. Read-only.
- **Scheduled Tasks** (tasks.rs):
  - Grouped by category, with running and failed badges and "Find a task".
  - Row: name, description (2 lines on the Mac, 3 on a phone), status with progress or the last result, and the schedule (Mac ≥900 pt).
  - Run; Stop (red, no confirm); "…" → "Edit schedule…".
  - Rows have a set height per category (`rows`) for frame cost.
- **Triggers editor** (triggers.rs):
  - The list in plain words, each with Remove (confirm).
  - "Add a trigger": Daily / Weekly / Interval / Startup, day, time, hours, time limit, with a live preview.
  - Each change applies at once.
- **Plugins** (plugins.rs):
  - Cards: image, status badge (Active / Restart needed / Disabled / Malfunctioned / …), version, description.
  - "Settings" (only when the plugin has a config).
  - "…" menu: Enable/Disable, "Open settings in the web", "Uninstall…" (danger).
  - No catalog or install.
- **Plugin settings editor** (plugin_config.rs):
  - A generic JSON form: bool, number, text, long text, list, nested group.
  - A filter appears above 15 fields.
  - Secret keys are masked with Reveal/Hide.
  - Save bar; a leave guard.
- **Logs** (logs.rs):
  - Groups Server / Plugins / Media conversion ("Direct stream · 21 Oct 2026 21:05"); "Find a log file".
  - Mac: list beside the viewer from 820 pt. Phone: list → file page.
  - Viewer: the last 300 lines (the last 16 MB at most), time and level columns, warning and error counts. Parses the plugin and FFmpeg formats.
- **API Keys** (api_keys.rs):
  - Rows: app name, created date, masked key.
  - Copy; **Revoke** (danger).
  - "New API key" (App name) → shown once in the Secret dialog.
- **Metadata editor** (src/metadata/; admin; card and detail menu entries "Edit metadata", "Edit images", "Identify", "Refresh metadata"):
  - A modal with the tabs Details / Images / Identify / Refresh. It has no phone layout of its own.
  - Details:
    - Title, Original title, Sort title, Tagline, Overview;
    - Year, release and end dates, community and critic rating, parental rating, series status;
    - Genres, Tags, Studios;
    - IMDb, TMDb and TVDB ids;
    - People (read-only);
    - Lock item and per-field locks.
    - Save sends only the changed fields.
  - Images: the current images with Delete (confirm); "Find images" by type, 30 a page, "Use" (confirm). No upload.
  - Identify: Name, Year, IMDb and TMDb → results → "Use this match" (danger), "Replace the images too".
  - Refresh: Scan / Search missing / Replace all, with options for images and trickplay.

---

## 2. Popups, menus, panels, dialogs

Rule: **one popup at a time** (§7). Menus: `ui/menu.rs` (`MenuState`, radio, checked, detail line, icon, submenu, separator). On a phone a menu is a sheet from the bottom, and a submenu takes the sheet over with a row back. A popup from the player bar on a phone in landscape opens upward, above the bar.

- **Card context menu** (`open_card_menu`, src/views/cards.rs:999). Mac: right click or the hover "More" button. Phone: **long press** (`LongPressEvent`, claimed with `prevent_default` so the release does not open the item). Entries:
  - Play (playable or series).
  - In a SyncPlay group: "Play next in the group", "Add to the group queue".
  - Mark played / unplayed; Add to / Remove from favorites.
  - On a home row: "Remove from Continue Watching" / "Remove from Next Up" (`hide_from_row`).
  - Add to playlist… / Add to collection… (`list_menu_items`); "Remove from collection" on a collection page.
  - While casting to a Jellyfin session: "Play next on {name}", "Add to the queue of {name}".
  - Download / Pause download / Resume download / Remove download / Download season.
  - Hide / Hide this episode / Hide this season / Hide the whole show / Unhide (Enhanced).
  - Admin: Edit metadata, Edit images, Identify, Refresh metadata.
  - Open in Jellyfin Web.
- **Card hover overlay** (Mac): play button, plus watched, favourite and more.
- **Card badges**: watched check or unplayed count (`indicator`); progress bar; Enhanced quality tags (4K, DV, HDR, Atmos…) at the top left.
- **Seerr card menu**: a status (Pending approval / Requested / Available), or "Request" / "Request all seasons"; then "Open on TMDB".
- **Account menu / sheet** (`rebuild_menu`, shell.rs:567):
  - Every server label with its profiles (radio; a tap switches).
  - Dashboard (admin).
  - Servers & profiles….
  - Mac only: Trailer backdrops (check) and Quality tags (check). On a phone these are in Settings › Display only.
  - The Jellyfin Enhanced submenu (feature toggles, "Keyboard shortcuts…").
  - Phone only (the Mac has them in the top bar): Play on (detail "A device of yours, a Chromecast or AirPlay" / "Another device plays"); SyncPlay ("Watch together with others" / "In a group"; only with access and a socket session); Random item.
  - Settings ("Playback, subtitles, home page, profile").
  - Downloads ("Movies and episodes kept on this device"; when allowed).
  - Quick Connect ("Sign in another device with its code").
  - Restart to update (Mac, when ready).
  - About Bloom.
  - Sign out.
- **Library menus**: View, Sort, Filter, and More (collection).
- **Detail menus**: Audio, Subtitles, More (Refresh, Open in Jellyfin Web, lists, hide, metadata).
- **Player menus**: Subtitles, Settings (gear), Audio. Contents in §3.
- **Panels** (`Panel`): Episodes (player), Help (Enhanced keys), SyncPlay, Cast, Subtitles (search), Bookmarks, Seasons (request dialog). Open with `toggle_panel`/`open_panel`; a click outside dismisses.
- **Dialogs**:
  - Add to playlist / collection: the existing lists plus "New … name" and Create.
  - Rename playlist; Delete playlist / collection (danger).
  - Request more seasons.
  - Quick Connect authorize (code).
  - Change password; profile picture picker.
  - Downloads "Remove all".
  - The metadata manager.
  - Every admin confirm, prompt, secret and text editor.
  - `ui::dialog` is the one shell: a scrim with occlusion, the keyboard inset, and the Settle entrance. On a phone a dialog with a field centres above the keyboard (`pb(keyboard_inset())`).
- **Chips in the top bar**: cast target (glyph, "title · position" or Connecting… / Nothing plays, inline pause), downloads ("Downloads · N active · %"), offline, restart to update.
- **Toasts**: one queue (top or bottom); several queued downloads become one toast with a count.

---

## 3. Player — src/views/player.rs, src/app/playback.rs, src/player/, src/stream.rs, src/queue.rs

### 3.1 Engine and platform
- libmpv: on the Mac the app's own render thread with display-synced pacing; on iOS MPVKit, vo `gpu-next` through MoltenVK into a `CAMetalLayer` under the UI.
- The player page has no background on iOS; it is a hole over the video.
- Mac-only pieces: the render thread, the GL surfaces, the trailer player, the display clock (`pacing.rs`), the keep-awake assertion (`awake.rs`), PiP (the window shrunk, `pip.rs`), fullscreen.
- **Nothing on the mpv worker waits for the network** (§8 rule; subtitle fetch and reports run on their own threads).
- Picture choices that carry over:
  - `cscale=bilinear` for 4K (GPU budget);
  - deinterlace only frames marked interlaced, at the field rate;
  - a sharper upscale;
  - verify the server's TLS on streams;
  - the token in a header, never in a URL (except AirPlay and Chromecast HLS, by necessity).

### 3.2 Layout and controls
- Full-bleed video. Top: header with a shade. Bottom: a floating glass panel (seek row above the transport and tools row).
- Controls hide after 2.5 s. They stay up while paused, buffering, starting, scrubbing, or with a menu or panel open.
- Mac: moving the pointer shows them; a click on the video pauses (or closes a panel). The pointer hides with them.
- **iOS: a tap on the video shows or hides the controls** (or closes a panel). Pause is only the button.
- Spinner: "Opening stream…" / "Buffering…".
- **Header**: Back (stops the player), title, Enhanced rating chip (not on a phone), SyncPlay chip ("Group · loading / catching up / waiting for a member / paused / in sync / idle / not following").
- **Seek row**: elapsed, slider, "-remaining".
  - Dragging only moves the preview; the release seeks.
  - Touch: the slider claims the drag at its start (`ui/slider.rs`, `TouchDragEvent` + `prevent_default`) so the pan does not scroll.
- **Transport**:
  - Previous (Shift-P): in the first 5 s it goes to the previous item, else restarts.
  - Rewind (skip back length, default 10 s); Play/Pause; Forward (default 30 s).
  - Next (Shift-N, when the queue has items).
  - Community rating ★ (not on a phone).
  - "Ends at HH:MM" (speed-aware, follows the 12/24 h clock; not on a phone).
- **Tools**:
  - Favourite (not on a phone).
  - Subtitles.
  - Mute and a volume slider (not on a phone).
  - Bookmarks (not on a phone; Enhanced).
  - Episodes (E; episodes only).
  - Settings gear.
  - SyncPlay (green in a group).
  - Play on / Cast (green while casting).
  - Mac only: small window (PiP) and Fullscreen (F).
- **Phone trims** (`PlayerMetrics.extras=false`):
  - Drops rating, Ends at, favourite, mute, volume slider, bookmark button and header rating chip.
  - 44 pt controls; transport centred, tools at the right; everything inside the safe area (`shell::safe_insets`, `panel_corner`).
  - The header keeps the title alone.
  - **The phone turns to landscape when the player opens and back to portrait when it closes** (`gpui_ios::set_landscape`). The iPad keeps its orientation: a geometry request there makes iPadOS 26 window the app.
  - Skip and Up next yield to a panel in that corner.
- **Keyboard** (Mac, or an iPad with a keyboard):
  - Space/K pause; ←/→ ±5 s; J/L ±10 s; ↑/↓ volume ±5; M mute; F fullscreen; E episodes; Shift-N/P next/previous.
  - Esc: close a panel, then leave fullscreen, then stop.
  - Z / Shift-Z: subtitle delay ∓100 ms. Ctrl-− / Ctrl-=: audio delay.
  - The Enhanced keys (§3.13). "?" shows the help.

### 3.3 Timeline: chapters, trickplay, bookmarks
- Chapter ticks; none within 1 s of the ends.
- Bookmark diamonds (Enhanced).
- Hover or drag preview: a 240 px trickplay frame cut from the server's tile sheets, with the chapter name and the time. A 160 px text box when there is no trickplay. Sheets near the resume point preload.

### 3.4 Media segments: skip intro and credits
- `client.media_segments`; stored in `meta.json` for downloads, so skipping works offline.
- Button "Skip Intro" / "Skip Credits" / "Skip Recap" / "Skip Preview" / "Skip":
  - only for ranges of 3 s or more; hidden in the last 1 s;
  - bottom right above the bar; seeks to the end of the range;
  - hidden while the Up next card shows.
- Auto-skip of intro and/or credits (Enhanced `AutoSkipIntro` / `AutoSkipOutro`), once per segment, with a toast "Skipped intro". Never inside a SyncPlay group.

### 3.5 Up next, autoplay, queue
- Card in the last 30 s, or from the start of the credits:
  - thumbnail, "Up next in N seconds", "S1E2 · Name", and the buttons "Play Now" and "Hide".
  - Pref `enableNextVideoInfoOverlay`; suppressed for an episode when autoplay is off.
- Autoplay: `EnableNextEpisodeAutoPlay`.
- At the end with nothing next, the player closes and the page reloads.
- Queue (`PlayQueue`: upcoming and history):
  - an episode queues the episodes after it;
  - Play All, Shuffle and playlists queue their items; a series expands to its episodes from the next unwatched;
  - only the first item resumes.
- **Lost episode** (`refind_lost_episode`): on a 404 the app finds the same season and episode number under its new id and rebuilds the queue (Sonarr replaces files mid-season).
- A full-screen window that began with the player ends with it; the next episode keeps it (Mac).

### 3.6 Resume
- Detail page: Resume / Play and "Play from the beginning". `play_series` plays Next Up, else the first episode of the first non-special season. There is no resume prompt inside the player.
- An item's position is reported (§3.11), so other devices pick up from it.

### 3.7 Pause screen and info overlay
- **Pause screen** (Enhanced style): after 5 s paused with no input.
  - Dark glass over a 28% backdrop; logo or title; year · rating · runtime; overview; progress with "pos / dur", "N% watched", "Ends at".
  - A tap resumes.
- **Playback info** (gear checkbox or the Enhanced key):
  - A card: Direct play / Direct stream / Transcoding; the reasons; Source; Target; Decoding; Limit; "Connection: 48 Mbps · 2 min ago"; the session.
  - Also mpv's stats overlay.

### 3.8 Audio
- Menu (in the gear menu under "Audio"):
  - direct play: mpv's tracks, "Title · LANG · CODEC · forced";
  - transcode: the server's streams; a change reloads the item at its position.
- "Audio delay" submenu: −500 / −100 / +100 / +500 ms and Reset.
- Settings: preferred language, Play default track, Remember selections. In a transcode with no user choice, the user's language preference overrides the server's pick.

### 3.9 Subtitles
- Menu: Off, then the tracks.
  - Transcode: the server's streams, with the detail "Burned in" or "Loading…".
- **Image subtitles** (PGS/VobSub; `specs/graphical-subtitles.md`):
  - The device profile asks for them as External files (`.sup`, `.mks`).
  - Fetched only when chosen (10–60 MB each); the menu says "Loading…".
  - A switch is instant, with no reload.
  - A failed file burns the track in, once (server before 12.2 for VobSub; never ask for `.sup`).
  - mpv ids are a map (`external_sids`), not list positions.
- Text subtitles of a transcode are fetched on their own thread (the server can take 80 s to make one), then added as a local file.
- "Timing" submenu (the same steps as audio delay); disabled for a burned-in track. A toast shows "Subtitles +100 ms". The delays are per item.
- **"Search for subtitles…"** (admin or `EnableSubtitleManagement`): the Subtitles panel.
  - Language chips.
  - Results: provider, format, downloads, rating, author; badges Hash match, Hearing impaired, Forced, Machine translated.
  - "Download to the library" (server-side; RemoteSearch/Subtitles).
- **Look**: §1.12 Subtitles, mapped to mpv `sub-*`. It overrides the Enhanced plugin's style (presets, colours, sizes, fonts, position).

### 3.10 Quality, bitrate, transcoding
- Gear › "Quality":
  - "Auto · Original|rung" with the detail ("The file as it is · X Mbps" / "Lowered for the connection").
  - The rungs under the source: 120/80/60/40/20 Mbps (4K), 15/10 (1440p), 8/6 (1080p), 4/3/1.5 (720p), 720 kbps (480p), 420 kbps (360p) (`core/src/bitrate.rs LADDER`).
  - HEVC, AV1 and VP9 count 1.5× when the ladder is cut.
  - A choice persists and reloads the item.
- **Adaptive Auto** (off by default; src/adaptive.rs):
  - `/Playback/BitrateTest` at sign-in and on a socket reconnect.
  - Stalls step the quality down; a fresh measurement steps it up; there is hysteresis and a hold-off.
  - A manual choice wins.
- Negotiation (core/src/playback.rs, src/stream.rs):
  - `POST PlaybackInfo` with the device profile: direct play of anything (mpv); transcode to HLS fMP4 HEVC/H.264 + AAC/AC3/EAC3/MP3, ≤6 channels, ≤1080p; HDR ranges declared; subtitles Embed, External srt/ass/ssa/vtt/pgssub/vobsub.
  - A remux counts as a transcode (reported as DirectStream).
  - The transcode ends on stop and on quit.
  - Product principle: **"Play the file as it is. Quality is lowered only when the user asks."**
- Speed: 0.5, 0.75, 1, 1.25, 1.5, 2× (Enhanced keys step 0.25 within 0.25–4×). Locked at 1× in a SyncPlay group.

### 3.11 Playback reporting
- Start; Progress every 10 s and about 300 ms after a pause, seek, volume or track change; Stopped.
- Reports run in order on their own thread; a stale progress is dropped.
- Quitting during playback sends Stopped.
- Offline: positions are kept per server and user, flushed on reconnect, and the server wins when its date is newer (core/src/downloads/offline.rs).

### 3.12 Episode picker panel — src/views/episodes.rs
- Series name; season chips.
- Virtual rows: still (160×90; phone 120×68) with progress, "Now playing" on the current one, "S1E2 Name", runtime, overview.
- Opens scrolled to the current episode. A tap plays and rebuilds the queue.
- Mac/iPad: a 500 pt card over the right end of the bar. Phone: a sheet at the right side, 62% wide, top to bottom.

### 3.13 Jellyfin Enhanced (plugin) features — src/enhanced.rs, src/bookmarks.rs, src/hidden.rs, src/seasons.rs, src/quality_tags.rs
- Flags come from the plugin; each can be turned off for this app (Settings › Display, the account menu submenu):
  - Seerr recommendations (detail rows);
  - Seerr / Sonarr / Radarr links;
  - Ratings in the player (★ plus a tomato critic %);
  - Pause when the window is hidden / Resume when it returns (not in a group);
  - Skip intros / credits automatically;
  - Enhanced keyboard shortcuts;
  - Bookmarks;
  - Hidden content;
  - Request more seasons.
- Quality tags on cards (`config.quality_tags`).
- Custom subtitle style.
- Pause screen.
- Shortcut actions:
  - CycleAspectRatio (Auto / Cover / Fill), ShowPlaybackInfo, CycleSubtitleTracks, CycleAudioTracks;
  - speed up / down / reset; SkipIntroOutro; BookmarkCurrentTime;
  - FrameStepBack / Forward; JumpToLastPosition; JumpToPercentage 0–9;
  - page keys GoToDashboard, QuickConnect, GoToHome, PlayRandomItem.
  - Not done: SubtitleMenu, OpenEpisodePreview.
- Help panel "Keyboard shortcuts…" in three columns. Not useful on a phone: the phone sheet drops the keyboard entries.
- **Bookmarks**:
  - Stored in the plugin's `bookmark.json` through its per-entry endpoints, so they sync with the web client.
  - Panel: label field, "Add mm:ss", rows (time, label, delete); a click jumps.
  - Shown on the detail page and as timeline marks.

### 3.14 Trailers
- Detail "Watch the trailer": the remote URL. Mac: the browser. iOS: an SFSafariViewController sheet.
- Hero trailers: Mac only (§1.5).

### 3.15 Now Playing and media keys — src/nowplaying.rs
- MPNowPlayingInfoCenter: title, series, "Season N · Episode M", duration, artwork, rate.
- MPRemoteCommandCenter: play, pause, toggle, stop, next, previous, skip ±, seek.
- Commands go to the cast target when one is set. MediaPlayer exists on iOS; the iOS host sets AVAudioSession `.playback` / `.moviePlayback` and the background-audio mode.

### 3.16 Not present today
- No iOS AVPictureInPicture.
- No external player.
- No Mac-style PiP on iOS.
- No volume UI on a phone (the hardware buttons are the volume).

---

## 4. Remote and cast

### 4.1 "Play on" panel — src/cast/ui.rs, src/cast/mod.rs, src/cast/target.rs, src/cast/remote.rs
- Opened from: the top-bar Play on button, the player's cast button, or the phone account sheet. Esc closes it.
- **No target**: sections:
  - "This device" (✓);
  - "Jellyfin devices": controllable sessions, "client · now-playing" / "nothing plays", "Looking for devices…", or a hint;
  - "Chromecast": name and model, "Searching…" for 6 s;
  - "AirPlay": "Choose an Apple TV or AirPlay device", with an AVRoutePickerView over the row.
- **With a target**:
  - glyph and name, the detail line, "Not connected: …" / "Connecting…";
  - title and "Paused · pos / dur"; a seek bar;
  - Previous / Next (Jellyfin only), Pause/Resume, Stop;
  - volume: mute, slider and number;
  - Audio and Subtitles lists (when `can_tracks`);
  - "This device → Play here instead": resumes locally from the target's position. Jellyfin is left playing; Chromecast and AirPlay are stopped.
- While a target is set, every play in the app goes to it. Media keys go to it too. A top-bar chip shows it.
- Toast "Play On: Playback goes to {name}".
- `cast_off` runs on every session change (sign-out, switch).
- On a phone in landscape the panel body scrolls (`max_h` + `overflow_y_scroll`).
- `Target`, `TargetView` and `RemoteTarget` hold no GPUI; they are candidates to move to core.

### 4.2 Controlling another Jellyfin client
- `GET /Sessions` filtered by `controllable`. The list comes from the socket ("Sessions" after `SessionsStart`) or a poll every 2 s (15 s with the socket).
- Play: `POST /Sessions/{id}/Playing` with PlayNow / PlayNext / PlayLast and a start position.
- Playstate: Pause, Unpause, PlayPause, Stop, NextTrack, PreviousTrack, Seek.
- General: SetVolume, ToggleMute, SetAudioStreamIndex, SetSubtitleStreamIndex, DisplayMessage.
- The state is optimistic between reports.

### 4.3 Being controlled
- Setting "Allow other devices to control this app" (default on).
- Capabilities are posted on each socket open (`PlayableMediaTypes=[Video]`, SupportsMediaControl).
- `SUPPORTED_COMMANDS`: VolumeUp, VolumeDown, Mute, Unmute, ToggleMute, SetVolume, SetAudioStreamIndex, SetSubtitleStreamIndex, DisplayMessage, ToggleFullscreen.
- Play messages: PlayNow, PlayNext, PlayLast with StartIndex and stream indexes. In a SyncPlay group a play goes to the group.
- Playstate: Rewind −10 s, FastForward +30 s.
- DisplayMessage shows a toast.
- Everything goes through the playback facade (`src/playback.rs request_*`) so a group stays authoritative. Parse and plan live in core (`core/src/cast/protocol.rs`).

### 4.4 Chromecast — core/src/chromecast/ (reusable), src/chromecast/
- Discovery: Bonjour `_googlecast._tcp` through `core/src/dnssd.rs`. It runs only while the panel is open or a target is set.
- TLS session: ping every 5 s, dead after 15 s, reconnects, coalesces seeks.
- **Certificate pinning on first use**, per device (`config.cast_pins`). After 3 refusals in a row it re-pins (devices rotate certificates).
- Receivers:
  - the Jellyfin receiver app (F007D354, or 6F511C87 unstable) with an identity and item stubs;
  - else the Default Media Receiver (CC1AD845) with an HLS URL.
- Track change only with the Jellyfin receiver. No skip.
- Mock device (`cast-mock` feature, `BLOOM_CHROMECAST_MOCK=1`).
- **iOS gap**: `ios/host/Info.plist` lacks `NSBonjourServices` and `NSLocalNetworkUsageDescription`, which a Bonjour browse needs.

### 4.5 AirPlay — src/airplay/ (app side, AVFoundation through objc)
- An AVPlayer with `allowsExternalPlayback`, given the server's HLS URL (one audio track, a burned-in subtitle).
- The user picks the receiver in AVRoutePickerView. A `_airplay._tcp` browse is for names only.
- Reports start, progress and stop to the server; ends the transcode.
- A track change re-sends the item from the position.
- Hardening: commands are deferred while loading; seeks coalesce; a pause in the last half second does not end the session; a new session takes the app's mute and volume; a route that fails comes back with a notice.
- The picker needs an NSWindow today, so it does not work on iOS. A native rebuild uses AVPlayer, AVRoutePickerView and AVPlayerViewController directly.

### 4.6 SyncPlay — core/src/syncplay/ (reusable), src/syncplay/
- Access comes from the user policy `SyncPlayAccess`: CreateAndJoinGroups / JoinGroups / None. Buttons are hidden for None.
- **Panel, not in a group**: the groups (name, participants); a tap joins. "New group" ("{user}'s group") with create access. Empty: "No group is open. Make one, and others can join it."
- **Panel, in a group** (title = group name):
  - state: Nothing plays / Waits for a member / Paused / Plays · participants;
  - **Queue**: numbered rows with ▶ on the current item; a tap jumps; move up/down; remove (not the current item);
  - Shuffle On/Off; Repeat Off / The queue / This item;
  - "This player": Stop / Resume local playback (stay in the group without being waited for);
  - "Leave the group";
  - stats: "Server clock ±X ms · ping Y ms · drift ±Z ms".
- Toasts: joined, left, user joined or left, refused (one text per reason), "The group goes on without this player."
- Card menu: "Play next in the group" / "Add to the group queue". A local play inside a group goes to the group.
- Core engine:
  - `Core::handle` → actions; phases Idle … Playing; scheduled unpause at server time.
  - Drift: no correction under 20 ms; a speed change of ±5% (±10% above 300 ms) for about 2 s; a jump above 1 s. Nothing is measured during a speed burst (mpv reports a wrong position then).
  - NTP-like server clock (`/GetUtcTime`); sleep detection.
  - Ready re-sent after 5 s; "steps aside" after 3 identical server seeks.
- Settings: Keep in step; Delay of this player.
- Leaving a group gives back normal speed and controls.
- The panel renders only with a socket session.
- Verify-before-relying rules (§8): a 204 from a SyncPlay request does not mean the server did it.

### 4.7 Server socket — core/src/realtime.rs (reusable), src/socket.rs, core/src/news.rs
- WebSocket `/socket`. KeepAlive every 30 s (or half the server's limit); dead after 75 s; backoff 1–30 s.
- Dispatch order: cast (capabilities, Sessions, Play, Playstate, GeneralCommand), adaptive (re-measure on reopen), connection (offline / online), SyncPlay (SyncPlayCommand, SyncPlayGroupUpdate), news.
- News: LibraryChanged and UserDataChanged, debounced (3 s quiet, 15 s cap). Reloads the title index and Home or Library; waits while the player is open.

---

## 5. Other features

### 5.1 Multiple servers and accounts
- §1.2. Config `servers[{id, name, url, profiles[{user_id, name, token, image_tag}]}]` and `active`.
- **The token goes in the Keychain on iOS** (today it is in config.json, mode 0600).
- Never print or copy the token.

### 5.2 Theme, glass, motion — src/ui/{glass,theme,motion,tone}.rs
- Dark only. Liquid glass on by default (Settings › Display), else frosted.
  - Mac tuning: bezel 16, thickness 20, rim 0.4, rim width 0.75, frost 0.5, alpha 0.35.
  - Phone: a popup frosts fully (frost 1, blur 1); the tab bar keeps the lens.
- Glass over the video:
  - Mac: a quarter-size copy of the frame.
  - iOS: the mpv worker copies the frame every 100 ms with `screenshot-raw` + `screenshot-sw` while a popup is up or the controls show.
  - A native rebuild over AVPlayer / Metal can do this with a real blur.
- Motion (§7).

### 5.3 Notifications
- Toasts only. No push or local notifications.
- Server news over the socket (§4.7). DisplayMessage → toast.

### 5.4 Automation events and lights (experimental; Mac in practice)
- Unix socket `~/Library/Application Support/bloom/events.sock` (0600). One JSON line per event; read-only; each reader gets `hello`, then `state`.
- Events:
  - playback.started / paused / resumed / seeked / stalled / recovered / rate_changed / stopped;
  - segment.entered / exited;
  - artwork.colour; window.changed;
  - with colours on: frame.colours (14 bands, about 12/s), detail.opened / closed.
- Off by default. Titles and colours are opt-in. Never the token, server, path or user.
- **bloom-lights** (`lights/`): a helper process that drives Govee LAN lamps:
  - film 5%; pause warm white at 25%; credits 40%; restore at the end;
  - an "aura" that follows the frame colours.
  - Settings: Find, Blink, use checkboxes, sliders.
- No iOS equivalent today. Out of scope unless asked.
- Spec: specs/outbound-events.md (stages 2–3 planned).

### 5.5 Debug channel — src/debug.rs (dev only)
- A Unix socket driven by `dev/jctl`.
- Areas: navigation, playback, admin, servers and sign-in, settings, Mac, diagnostics, lists, downloads, connection, cast, chromecast, airplay, syncplay, events, socket-inject, a11y…
- The iOS test tools depend on it (`dev/ios-ui tap-label` reads the a11y tree through it; `/tmp/bloom-ios.sock`). **A native app needs an equivalent test hook**, or XCUITest with accessibility identifiers, to keep touch tests that do not rely on synthetic coordinates.

### 5.6 Downloads and offline — core/src/downloads/ (reusable engine), src/downloads/
- What: Movie, Episode, Video, MusicVideo, and a whole season (its missing episodes). The **original file only** (`/Items/{id}/Download`); no transcoded downloads.
- Gated by the policy `EnableContentDownloading`.
- Stored per server: the file, `meta.json` (item, media segments, subtitle list), images, sidecar subtitles (srt, ass, ssa, vtt, PGS as `.sup`; no VobSub).
- `.part` files with Range resume; stall detection; 6 attempts; a full disk is final; a changed server file restarts.
- States: Queued, Downloading, Paused, Failed, Done.
- Storage limit and 5 GB minimum free; 1–2 at a time.
- **Detail button**: "Download for offline use"; a progress ring (a click pauses); Failed "Click to try again"; Downloaded "Click to remove the file"; season variants.
- Offline playback from the local file with sidecars and stored segments. The offline detail page comes from `meta.json`. A download of another server than the session's is refused.
- Position sync (§3.11).
- Index recovery when the list is lost. Ids and file names from the server are checked before use as paths.
- iOS: background URLSession is the native fit. The core engine is threads plus HTTP; decide on reuse or replacement.

### 5.7 Hidden titles (Jellyfin Enhanced) — src/hidden.rs
- Server file `/JellyfinEnhanced/user-settings/{user}/hidden-content.json`. Read the whole file, change one entry, write it back, keep unknown keys.
- Active when the plugin feature, the user's `Settings.Enabled` and the app switch are all on.
- Surfaces filtered by the Filter* flags: library, latest, hero, search, Next Up, Continue Watching.
- Scopes: Everywhere, Next Up only, Continue Watching only, Home rows only. Hiding again widens the scope. A hidden series hides its episodes. Pages drop the item at once.
- Menu: Hide / Hide this episode / Hide this season / Hide the whole show / Unhide (with `ShowHideButtons`).
- Settings page: rows with Unhide. Empty: "Nothing is hidden…".

### 5.8 Jellyseerr requests (through the Enhanced plugin proxy `/JellyfinEnhanced/jellyseerr/...`)
- Search "Discover on Seerr" row; detail rows "Recommended on Seerr" and "Similar on Seerr"; Seerr, Sonarr and Radarr links.
- Seerr card: type and status (2 pending, 3 processing, 4 partly available, 5 available; amber when waiting on a person). A tap opens the library item, or the menu.
- Menu: "Request" / "Request all seasons", "Open on TMDB".
- **Request more seasons** (src/seasons.rs):
  - Compares Seerr's seasons with the library: Available / Partly / Requested / Pending / Blocked / Missing.
  - Dialog with checkboxes, "Request N seasons". `POST …/jellyseerr/request`.
- **A request starts real downloads**: only an explicit button sends one. No debug verb sends one.

### 5.9 Ratings, played, favourites
- Community rating ★ and critic % (Enhanced, player), official rating chip.
- Mark played / unplayed; favourite on cards, the hero, the detail page and the player.
- Watched check and unplayed count on cards.

### 5.10 Lists
- §1.10, §2. Collections need admin or `EnableCollectionManagement`. Delete is guarded to Playlist and BoxSet kinds only.

### 5.11 Mac-only (not for iOS)
- Menu bar (src/menus.rs):
  - Bloom: About, Check for Updates…, Settings ⌘,, Hide, Quit.
  - File: Close ⌘W (the app stays in the Dock).
  - Edit: Undo, Redo, Cut, Copy, Paste, Select All.
  - View: Home ⇧⌘H, Back, Forward, Search ⌘F, Random, Reload ⌘R, Full Screen.
  - Playback: Play/Pause, PiP ⇧⌘P.
  - Window; Help (README, Release Notes, Report a Problem).
- Sparkle updates. Keep-awake. Trackpad swipe. Window frame memory. Show in Finder. Layered app icon.

---

## 6. iOS behaviour already designed (requirements to honour or replace on purpose)

Daniel's direction (2026-10-21): **"the menus and the navigation should feel native on each platform."** PRODUCT.md: "A Mac app on the Mac, a phone app on the phone; shared code is an implementation detail the user never sees." DESIGN.md: **"Don't reinvent a native control on the phone for flavour; the glass is the brand, the structure is the platform's."**

- **Structure**
  - A glass tab bar at the bottom: Home, up to 3 libraries, Favorites when there is room, Search.
  - Tab bar behaviour: lights only Search on search, nothing on Settings, Dashboard or Downloads; steps aside for the keyboard; icon and label centred (Daniel: "slightly top justified" was a bug).
  - Top bar: back, title, account.
  - Menus are sheets from the bottom; a submenu takes the sheet over with a back row.
  - The account sheet holds Play on, SyncPlay and Random item, and drops Trailer backdrops and Quality tags (they live in Settings › Display).
  - Dashboard and Settings: a list of sections first, each section pushed.
  - Users: a list → a user page → the access editor, with Back in that order.
  - Logs: a list → a file page.
- **Layout**
  - Three posters across (gap 14, 121 pt posters).
  - Library toolbar on two rows, no letter rail, the view as a pill (Daniel: the title "stood twice").
  - A pager under the grid ("a page of a hundred is ten phone screens").
  - Detail in one column without the floating poster; the actions on their own row; cast tiles 96 pt; episode rows with a wide still; track boxes under their label.
  - Row headings have no arrows (a finger pans).
  - Tap targets ≥44 pt. Fields and buttons 50 pt with 17 pt text (**The Phone Reads at 17 Rule**; Daniel 2026-10-22 "found the fields small").
- **Player**
  - Landscape on open, portrait on close (phone only; the iPad keeps its orientation).
  - A tap toggles the controls instead of pausing.
  - Trimmed controls (§3.2); 44 pt controls.
  - Popups open upward, above the bar.
  - The episode picker is a sheet at the right over the video.
  - **The bar is glass over the picture, not a dark slab** (Daniel 2026-10-23). The video backdrop copy runs while the controls show.
  - The controls hide a few seconds after the last touch.
- **Glass on a phone**: popups frost over fully. Daniel found the menus "hard to see over background" with the Mac's clear tuning. The tab bar keeps the lens.
- **Long press on a card** opens its menu (no hover or right click on a finger). The release must not open the item.
- **Keyboard**
  - The page moves with the keyboard's slide. Daniel's first phone run: the sign-in card was "partially blocked by the keyboard".
  - Cards and dialogs centre above the keyboard; a tall card scrolls to its end.
  - **Each field names its keyboard** (Daniel: "the fields do not allow copy and paste nor do they allow apple siri completion"):
    - URL with Go;
    - username: untyped, with no autocorrection (autocorrect turned "dro" into "drop"; the username content type put a Passwords key over the field, which Daniel does not want);
    - password: secure with the keychain and Go;
    - code: number pad / one-time code;
    - search: predictions and Search;
    - name: word capitals and Done;
    - prose: correction and sentence capitals.
  - A long press opens the system edit menu (Cut, Copy, Paste, Select All; a password is never copied out).
  - Predictions and autocorrect must work. Daniel: a tapped suggestion "doesn't work" was a bug.
  - The Passwords bar must not leak onto the search field after the username or password field. Check both walks: username → Back → search, and password → Back → search.
  - The keyboard is dark.
  - Native SwiftUI TextField / SecureField with `textContentType` handles most of this; keep the username choice.
- **Trailers** in an SFSafariViewController sheet.
- **VoiceOver**: every control carries a role and a label (tabs, cards by title, hero and detail buttons, player buttons, picker rows, settings controls). The touch tests rely on them. SwiftUI gives this natively; keep stable labels and identifiers.
- **Splash**: the bloom animation, 1.5 s, never a wait.
- **Motion**: a page pushed on a phone slides from the right; Back fades; sheets rise. Native NavigationStack and sheets give this. Honour Reduce Motion.
- **iPad**: the desktop layout today, glyph-only tabs under 900 pt. `UIRequiresFullScreen` stays true. Avoid geometry requests (iPadOS 26 windows the app). A native iPad design is still open.
- **Background audio** (UIBackgroundModes audio; AVAudioSession `.playback`). App icon 1024 px from assets/icon/bloom.png.
- **Daniel's process rules**: "no build goes to the phone before the simulator shows no crash (the walk, and the sign-in fields by hand)"; one step at a time, shown in a build; everything tested by touch.
- **Open items from CLAUDE.md**: Dynamic Type (the theme has `text_scale`; nothing on iOS sets it); a hero swipe on the phone; the iPad's own pass.

---

## 7. Design rules that must carry over (CLAUDE.md, DESIGN.md)

- **Dark only.** Page Theatre Black `#101010`.
- **One ink; colour only for state.**
  - All text and icons are `#f5f5f7`; quieter text is the same ink at 70%, 60% or 50%. Icons sit on the quiet disc (`rgba(204,204,207,0.25)`).
  - Green `#7ee787` = live or working. Amber `#e3b341` = needs a look. Red `#ff7b72` = failed or dangerous. Danger fill `#c62828`.
  - A badge is a quiet pill; a state shows as a dot.
  - Daniel called the rainbow dashboard "slop": no category colours, tinted tiles, coloured badges or rainbow avatars.
  - No second accent. Colour comes from the artwork.
- **No gradients on cards. No shadows.** Depth is translucency plus the 1 px light edge `0xf5f5f733` (Screen White 20%).
- **Glass.**
  - Every popup, sheet, panel and bar is smoked glass (`rgba(42,42,42,0.72)`) with the light edge.
  - Rejected by Daniel: "a dark scrim over glass ('dark foreign blob')" and "a white wash with a uniform ring ('flat gray outlined button')". Glass controls draw no ring.
  - The dashboard sits on plain black (the indigo wash was dropped on 2026-10-23).
- **Popups share one look.** Daniel 2026-10-03: "the blurred background from the original component with the size and spacing of the sync play menu".
  - Corner radius 20–24.
  - Rows 38 pt or taller, 12 pt corners, 12 pt padding.
  - 15 pt medium text; 12 pt quiet text for details and headings.
  - An entry can have an icon and a detail line; hairline separators.
- **One popup at a time.** A menu, the episode list and a panel are never open together. Opening one closes the rest. A panel closes on a click outside. Check it: open the new popup, then another, and look.
- **Type**: Google Sans (Geist, then the system) at Display 27/700, Headline 22/600, Title 17/500, Body 15/400, Label 12/500; Geist Mono 13 for logs, paths and the Quick Connect code. Nothing under 12 pt.
- **Shapes**: row 12, control 14, pill 18, sheet 20, card / panel / tab bar 24, connect card 28, posters 12.
- **Spacing**: unit 4; groups 22 apart; card padding 32 on the Mac, 24 on a phone; card gap 27 on the Mac, 14 on a phone; phone gutter 16.
- **Buttons**: one white primary per page (Play, Sign In, Connect); quiet glass for the rest; plain (dashboard); danger red.
- **Motion** (`ui::motion`, DESIGN.md):
  - one curve, ease-out quint; 150 ms in place, 200 ms arriving;
  - Push from the right by 32 pt; Lift 12 pt from below (a replaced page); Rise 28 pt (sheets); Settle 6 pt over a fading scrim (popups and dialogs); Fade (in place, or Back);
  - **The Arrival Rule**: motion marks where something came from.
  - **The Quiet Return Rule**: going back fades; only forward slides.
  - No bounce, no spring, no movement on hover, nothing over 200 ms. Exceptions: the hero cross-fade (500 ms) and the splash.
  - Reduce Motion (system or theme) turns movement off.
- **Terminology** follows Jellyfin: libraries, Next Up, Continue Watching, SyncPlay, Quick Connect, Cast.
- **Copy**: plain words. An error says the cause and what to do. "this Mac" in a sentence becomes `brand::THIS_DEVICE` ("this device" on iOS).
- **Hero**: the signature surface (§1.5); a 55% shade only where text must read.
- **Performance budget**: `frame_cpu` under 8 ms on the Mac. Virtualise long lists (`rows`, `lazy`). Set card widths instead of flex shares (the layout engine measured them many times).

---

## 8. Safety rules for building and testing an iOS app (CLAUDE.md)

- **Writes to Daniel's server**: allowed only when the test puts the data back.
  1. Keep the original.
  2. Change one harmless value, or save the unchanged object.
  3. Check with a GET.
  4. Revert.
  5. Check that the data equals the original.
  - SyncPlay group calls are allowed.
- **Never in a test**: restart or shut down the server; delete a user, device or library item; revoke a key that is not the test's own; change ports, addresses, HTTPS, remote access or paths; sign Daniel's session out.
  - In the app these are the dashboard's Restart, Shut Down, Delete user, Delete device, Revoke, the Networking and Transcoding saves, and forgetting a profile, which logs its token out.
- **Playback in tests**: muted only; no playback reports (`BLOOM_NO_REPORT=1`); never on an item Daniel has started. A test instance on Daniel's config is the same server session as his app; turn the socket off unless the test needs it (`BLOOM_NO_SOCKET=1`).
  - On 2026-10-03 Daniel heard a test play and took its frozen window for his own app. `dev/ios` starts the simulator app with the socket, the reports and the sound off; a native build needs the same switches.
- **Never print or copy the access token** (the config, logs, a dialog).
- **Cast devices in the house** (the "Living Room" TV box, the Apple TV): a test may discover them and read their status. It must not launch, load, change volume or stop anything. Use the mock (`BLOOM_CHROMECAST_MOCK=1`).
  - A tap that lands on a cast device row connects to it; the TV is not launched, but take care in touch walks.
- **Jellyseerr**: a request starts real downloads. Never send one in a test.
- **No synthetic pointer or keyboard input on the Mac**: it is blocked, and Daniel may be at the machine. Drive tests through the debug channel. In the simulator, idb taps are the touch tests (`dev/ios-ui`).
- **A test instance must not take the keyboard focus** (keys Daniel typed went to the test window).
- **Do not restart Daniel's own app** while its player is in use. Do not run `dev/install` (the installed app updates from releases).
- **Test account**: `jellyui-test-user` (`test-account.txt`). Its password got a 401 on 2026-10-21; `dev/test-user` recreates it, and Daniel must run it (the permission tool refuses it to the agent). Test items and lists are named `jellyui-test-*`; the `lists` debug verb changes only those.
- **Server choice**: `live` (read-only, the real library) / `mock` (fixed library, no media) / `container` (`dev/jellyfin-local`; for writes and playback, and `BLOOM_JELLYFIN_IMAGE=jellyfin/jellyfin:12.1` for Daniel's server version).
- **Device builds**: team T46QDHL88N. Phone build only after a clean simulator walk, including the sign-in fields by hand.
- **Facts to verify before relying on them**:
  - an item id does not last (Sonarr replaces files; the old id answers 404);
  - a SyncPlay 204 is not success;
  - mpv's position is about 100 ms off while its speed is not 1.0;
  - a race fix needs a test that fails with the fix reverted.
- **Commits**: only when Daniel asks; never push without his word; no Claude attribution.

---

## Appendix: reuse map (core vs rebuild)

- **Reusable from core** (no window):
  - `jellyfin` (REST `Client`), `pages` (loaders), `config`, `connection`, `realtime` (socket), `news`;
  - `playback` (device profile, request body), `bitrate` (ladder, Auto), `quality_tags`, `subtitles`, `languages`, `trailers`, `queue`, `search_index`, `draft` (`with_changes`);
  - `downloads` (engine, offline positions), `syncplay` (core, drift, clock, protocol), `cast::protocol`, `chromecast` (all), `dnssd`, `events`, `images` (fetch and decode).
- **App-side today, no GPUI inside, worth moving to core**: `cast/target.rs`, `cast/remote.rs` traits; `stream.rs` negotiation; `adaptive.rs`; `queue.rs` logic; the `enhanced`, `hidden`, `seasons`, `bookmarks` and `lists` request blocks (`easy_ext` impls on `Client`); `admin/*` request logic and `config.rs` specs.
- **Rebuild natively in Swift**:
  - all views;
  - the player shell (mpv via MPVKit, or AVPlayer for HLS);
  - AirPlay (AVRoutePickerView);
  - Now Playing;
  - Keychain tokens;
  - background downloads;
  - PiP (AVPictureInPictureController with a sample-buffer layer if mpv stays).
- **Drop on iOS**: Sparkle, the menu bar, the automation socket and lights, Show in Finder, the trackpad swipe, the hero trailer (or rebuild it with AVPlayer if trailers resolve to a playable URL).
