# Graphical subtitles as files

Status: built on the branch `graphical-subtitles` (2026-10-29), not merged.
The spec is from 2026-10-27; "What was built" at the end says where the
build went another way, and holds the measurements.

## Purpose

A PGS or VobSub subtitle on a transcode plays without burn-in. The server
hands the track over as a file, mpv adds it on the side, and a switch of
the subtitle is a switch in mpv, not a reload of the item.

## The problem today

Bloom's device profile (`device_profile`, `src/stream.rs`) names srt, ass,
ssa and vtt as External and every format as Embed. For a direct play that
is right: mpv plays the embedded track. For a transcode (HLS, fragmented
MP4) the server cannot carry an image track in the segments, so a PGS or
VobSub track the user picks is burned in (`DeliveryMethod` "Encode"):

- The server re-encodes the video for the subtitle alone. A file the
  server would otherwise copy (a remux over the bitrate limit, a
  container mpv could not take) becomes a full transcode.
- Each switch of the subtitle loads the item again (`stream_subtitle` →
  `stream_reload`): a few seconds of black, a new play session, a new
  first-segment wait.
- The menu marks the track "Burned in" and off is a reload too.

Most of Daniel's films carry their subtitles as PGS, and his remote path
transcodes everything over 25 Mbps (`jellyfin-server-remote-path`), so
this is the common case away from home.

## What the server does

Read in the source of 12.1 and 12.4; the behaviour is the same in both,
12.2 made the endpoint reliable (server PR 18168, "Serve graphical
subtitles as raw files with range support"):

- `SupportsExternalStream` is true for a PGS or VobSub stream, embedded or
  not (`MediaSourceManager.StreamSupportsExternalStream`).
- A profile entry `{ Format: "pgssub", Method: "External" }` matches an
  embedded or external PGS track on a transcode. A profile entry
  `{ Format: "vobsub", Container: "mks", Method: "External" }` matches an
  embedded VobSub track, or an external one that is already an `.mks`
  (`StreamBuilder.IsVobSubMksProfile`). An external `.idx/.sub` pair gets
  no file: the encoder tries an srt conversion of it and fails.
- With one of these the `PlaybackInfo` answer marks the stream
  `DeliveryMethod: "External"` with a `DeliveryUrl` of the form
  `/Videos/{item}/{source}/Subtitles/{index}/0/Stream.pgssub` (or
  `Stream.vobsub`). The transcode address then carries no
  `SubtitleStreamIndex` (`StreamInfo.ToUrl`), so the server does not burn
  anything in and the video is a copy where the bitrate allows it.
- The first request for the file runs `ExtractAllExtractableSubtitles`
  over the whole media file, as a text track's first request does today
  (about 80 s for a 9 GB episode, measured 2026-10-17). The result is a
  `.sup` for PGS and an `.mks` for VobSub in the server's subtitle cache;
  later requests serve it at once, with range support.
- Jellyfin Web 12.2 declares the same two entries behind its "PGS
  rendering" setting and renders them in the browser. Bloom lets mpv do
  it: `sub-add` takes a `.sup` and an `.mks`.

This is not gated on 12.2 on the server side by the profile matching; what
12.1 answers for `Stream.pgssub` on the wire is untested (12.1 serves it
through `EncodeSubtitles`, 12.2 straight from disk). The measurement below
settles whether Bloom needs a version check.

## The change, by file

### 1. The profile (`src/stream.rs`, `device_profile`)

Add two External entries after the text ones. The Embed entries stay, so
a direct play is judged as before (the server takes Embed first when it
does not transcode, `StreamBuilder.CanConsiderEmbedSubtitle`).

```rust
subtitles.push(json!({ "Format": "pgssub", "Method": "External" }));
subtitles.push(json!({ "Format": "vobsub", "Container": "mks", "Method": "External" }));
```

`resolve` needs no change: a stream with `DeliveryMethod` "External" and a
`DeliveryUrl` already becomes an `ExternalSub`, `burned_subtitle` reads the
`SubtitleStreamIndex` the address no longer carries, and the menu's
"Burned in" detail is `sid_of_external(index).is_none()`, so a PGS track
with a file loses the label on its own.

### 2. The file name (`src/player.rs`, `fetch_subtitles`)

mpv reads the format from the extension. The address ends in
`Stream.pgssub` or `Stream.vobsub`; the current rule keeps an extension of
four characters or fewer and falls back to `srt`, so a PGS file would be
written as `.srt`. Add a function and use it there:

```rust
/// The extension of the file written for a subtitle address: what the
/// server names, as mpv knows it. The server's names for the two image
/// formats are not file extensions (`.pgssub` is refused by ffmpeg).
fn subtitle_extension(url: &str) -> &'static str
// "pgssub" | "pgs" | "sup" → "sup"; "vobsub" | "mks" → "mks";
// "srt" | "ass" | "ssa" | "vtt" → itself; anything else → "srt".
```

### 3. The size (`src/player.rs`, `fetch_subtitle`)

`SUBTITLE_LIMIT` is 32 MB and the body is read into memory before it is
written. A PGS track of a film is 20 to 60 MB, a 4K one up to about
100 MB. Raise the cap to 256 MB and stream the body into the file
(`std::io::copy` from the body reader with a `take`), so the memory of the
app does not grow by the file. The same for `get_bytes` in the downloads
engine (its cap is 64 MB), or a `get_file(url, path)` beside it.

### 4. Fetch an image track when it is chosen, not at the start

`fetch_subtitles` fetches every External track of a load at play start, in
order. With image tracks that is a film's ten PGS tracks, 300 MB, on each
play, and on the remote path those bytes compete with the video segments
on the 25 Mbps link. So:

- A text track is fetched at the start, as today.
- An image track is fetched when the user picks it (and the one that
  starts selected, at the start). `SubtitleFile` gets `graphical: bool`;
  `fetch_subtitles` skips an unselected graphical one; `Player` gets
  `add_subtitle(SubtitleFile)` that fetches one file for the current
  generation and sends `Cmd::AddSubtitle` with `select: true`.
- `stream_subtitle(index)` on an image track without a file yet calls
  `add_subtitle`; mpv selects it when it comes.
- While the file is on its way the menu entry says "Loading…" in its
  detail line (the status gets the set of pending server indexes; the
  worker clears an entry at `AddSubtitle` or at a failed fetch). On the
  Mac the menu is the popup, on a phone the sheet; the entry is the same.

### 5. The mpv id of a file (`Resolved::sid_of_external`, `subtitle_of`)

Both compute mpv's id from the position in `external_subs`, so they assume
every file is added, in order. Step 4 breaks that order, and today a file
the server did not give (`continue` in `fetch_subtitles`) already shifts
the ids of every later file: the menu then selects the wrong track. Make
the map explicit:

- `Cmd::AddSubtitle` carries the server `index`.
- After `sub-add` the worker reads `track-list` and takes the id of the
  last subtitle track (mpv appends), and writes `index → id` into the
  shared status (`Status::external_sids: Vec<(i64, i64)>`); `Cmd::Stop`
  and a new load clear it.
- `sid_of_external` and `subtitle_of` read the map from the player status
  instead of the position. The callers (the menu, `stream_subtitle`, the
  "Playback info" overlay) pass the status they already hold.

### 6. Downloads (`core/src/downloads/engine.rs`, `fetch_subtitles`)

The sidecars of a downloaded item: the match on the codec skips every
image format ("Image subtitles are not a file mpv takes on the side",
which is not so). Add `"pgssub" | "pgs" | "hdmv_pgs_subtitle" => "sup"`.
The fallback address the engine builds, `Stream.sup`, is one the server
takes (`MediaStream.IsPgsFormat` accepts "sup"). An external VobSub pair
stays skipped (see "What the server does"). `local_source`
(`src/downloads/mod.rs`) hands the files to mpv by name, so a `sub-3.sup`
in `meta.json` plays without a change there.

### 7. iOS

The same code runs on the phone. Nothing platform-specific; the temp
folder of the subtitle files is the app's.

## What does not change

- A direct play: the embedded track plays from the container, as today.
- Text subtitles: the same path, fetched at the start.
- A transcode of a file whose chosen subtitle has no file (an external
  `.idx/.sub`, a DVB or teletext track): burned in, as today.
- The order "Nothing on the mpv worker waits for the network" holds:
  every fetch is on the subtitles thread; the worker gets a local path.

## Testing, three tiers

The formats under test are real files, not stand-ins: a PGS stream mpv,
ffmpeg and the server decode as one, and a VobSub stream made from it.

### Where the files come from

ffmpeg has no PGS encoder, and the only PGS in the house are Blu-ray rips.
So `dev/make-sub-clip` (to add; a proof is at
`/private/tmp/claude-501/-Users-drodriguez-github-com-bloom/df73cc6c-d04d-438d-bb4c-4c1cdd16bc1c/scratchpad/sup/make_sup.py`,
checked on 2026-10-28) makes them:

1. ffmpeg's `drawtext` renders each caption to a gray bitmap.
2. A PGS encoder of about 60 lines of Python (the format is five segment
   kinds with a run-length bitmap) writes a `.sup` with the captions at
   their times. `ffprobe` reads it as `hdmv_pgs_subtitle`, and
   `[0:v][1:s]overlay` paints the caption on a frame, which proves it is
   a PGS any decoder takes.
3. `ffmpeg -i x.sup -c:s dvdsub -f matroska x.mks` makes the VobSub from
   the same bitmaps (ffmpeg has a dvdsub encoder; a bitmap-to-bitmap
   transcode needs no text rendering).
4. One 20-second clip of `testsrc2` gets srt, PGS and VobSub embedded
   (`-c:s copy`, each tagged `eng`), and the `.sup` sits beside it as an
   external sidecar. About 3 MB in all. The captions name their format
   ("First caption, PGS"), so a screenshot shows which track plays.

The clip goes into `dev/jellyfin-local`'s library (a seventh film in
`media()`), and the real-player test takes the `.sup` from the same
script. No fixture is checked in; the script is.

### Tier 1: unit tests, no server, no player

`cargo test` on fixtures of JSON and bytes:

- `src/stream.rs`: the profile test adds the two External entries (the
  vobsub one with its container) and keeps the Embed one. A `TRANSCODE`
  fixture variant with a PGS stream marked External: `external_subs`
  holds it, `burned_subtitle` is `None`, the address ends in
  `Stream.pgssub`, the key stays out of it.
- `src/player.rs`: `subtitle_extension` on the five names and an unknown
  one.
- Step 5: three files where the second fails to fetch, and the third
  still maps to its id (fails on the code of today).
- Step 4: an unselected graphical track is not requested at play start
  and is requested after `stream_subtitle` (a counting listener, as the
  race tests of `src/app/race_tests.rs` count requests).
- `core/src/downloads/engine.rs`: an external PGS sidecar is written as
  `sub-N.sup`; a VobSub pair is skipped.

### Tier 2: the real player, no server

A `REAL_PLAYER` test that ends with `Closer`, after
`a_slow_subtitle_file_does_not_hold_the_player`: a local listener serves
the `.sup` under `Stream.pgssub` and the `.mks` under `Stream.vobsub`,
the player plays the silent clip, and the test checks that mpv lists two
sub tracks with their titles, that the files on disk end in `.sup` and
`.mks`, and that `sub-add` took them (no "sub-add failed" in the log, the
track `codec` is `hdmv_pgs_subtitle` and `dvd_subtitle`). This proves
mpv's side without a server.

### Tier 3: a Jellyfin in a container, two versions

`BLOOM_TEST_SERVER=container` (`dev/jellyfin-local`, Rancher Desktop must
run; it was off on 2026-10-28) with the clip above. The image is
`jellyfin/jellyfin:latest`, 12.4 today; `BLOOM_JELLYFIN_IMAGE=
jellyfin/jellyfin:12.1.0` is the version of Daniel's server. The script
keeps one data folder, and a 12.1 server does not open a 12.4 database,
so the data folder gets the image tag in its name
(`target/jellyfin-local-12.1.0`) and the container too; a change the
spec asks for in `dev/jellyfin-local`. Then the walk, on each version,
through the debug channel of a test instance (`dev/jctl`):

1. Play the clip with the quality set under its bitrate, so the server
   transcodes.
2. Choose the PGS track. Read "Playback info" and `dev/jctl state`: the
   address carries no `SubtitleStreamIndex`, the play method is the
   copy the bitrate allows, mpv's track list has a sub track with a
   `.sup` path, and `dev/shot` shows "First caption, PGS" at 2 s.
3. The same with the VobSub track: a `.mks` path, "First caption,
   VobSub" on the shot.
4. Switch between the three tracks and off: no reload (the play session
   id stays, `dev/jctl state` shows no `Starting`).
5. Download the clip; `meta.json` lists `sub-N.sup`; play it offline and
   the PGS track is in mpv's list.
6. On 12.1.0: what the endpoint answered for `Stream.pgssub` (status,
   content type, the size against the server's cache file). The spec
   expects the same file; if not, add the version check of step 1 of the
   change (keep `version` from `PublicSystemInfo` on the `Client`).

`dev/ios-smoke` is not touched; the phone runs the same code and a
screenshot of step 2 in the simulator against the container is enough.

### Tier 4: the live server, by hand

Daniel's library (counted 2026-10-28) has PGS in nine films (91 tracks,
"Collateral" among them) and one episode of "After" (two tracks), all
embedded; no VobSub and no external sidecar anywhere. So a Blu-ray track
of real size and timing is at hand for the one thing the clip cannot
measure: the first fetch of a large film's track (the extraction over the
whole file) and a second (the cache), and the fetch over the remote path.
A test instance (`BLOOM_TEST_NAME=subs dev/run-test`, muted, no reports)
on a film Daniel has not started; write the times into this spec. There
is no VobSub to try on the live server; tier 3 covers it.

## Order of work

1. `dev/make-sub-clip` and the clip in `dev/jellyfin-local`, with the
   data folder per image. Tier 3 against both versions on the code of
   today, as the baseline: burn-in, a reload per switch.
2. Steps 1, 2, 3 and 6, with their tier 1 and 2 tests. Tiers 3 and 4.
3. Step 5 (the id map), with its test. It is a fix on its own.
4. Step 4 (fetch on choice) and the "Loading…" detail.
5. Update the notes: the "Nothing on the mpv worker waits for the
   network" section of CLAUDE.md gets a line on the image tracks, and the
   profile comment in `device_profile` drops "the server burns in the
   rest".

## What was built (2026-10-29)

Every step, with these differences from the plan:

- A file that fails burns the track in. A server before 12.2 answers 400
  for `Stream.vobsub` (12.1 serves `Stream.pgssub` as it is, so PGS needs
  no version check), and a `.mks` sidecar fails on 12.4 too (the server
  looks for it in the cache of the media file, where it never is). So a
  chosen file that does not come (`PlayerStatus::failed_subs`, read by
  the player poll in `app.rs`) loads the item again with
  `AlwaysBurnInSubtitleWhenTranscoding` (`Load::burn_in`,
  `Bloom::subtitle_file_failed`), once: the answer marks the burned track
  as a file still, so `negotiate` drops it from `external_subs`, and
  `Resolved::burn_in` stops a second round. `stream::State::failed_files`
  remembers the index for the item, so the next choice of it burns in at
  once. On Daniel's 12.1 a VobSub costs one failed request and a reload,
  as every image track did before; a PGS is a file.
- The downloads engine asks for a PGS sidecar as `Stream.pgssub` and
  names the file `.sup` (`sidecar_format`): 12.1 refuses `Stream.sup`.
  `SubtitlesMissing` counts only the sidecars the engine keeps.
- `stream_subtitle` serves a direct play too (a file, else mpv's own
  track by its place), and the direct play's subtitle menu lists the
  files mpv does not have yet (`rebuild_track_menus`), since an image
  track comes only when chosen.
- `dev/jctl track sub <index|off>`, `track audio <index>` and `tracks`
  (the mpv tracks with their server indexes, the map, the pending and
  the burned state) drive and read the walk.
- A stream cleared mpv's `sub-files` with an empty string, which mpv
  took for one file named "" ("Cannot open file ''" at each load);
  `local_source` clears the list with `change-list`.
- `dev/make-sub-clip` muxes with `-copyts`: without it ffmpeg moved the
  PGS packets a second early (the first caption is at 1 s and ffmpeg
  shifts an input to start at 0). A VobSub packet has no end of its own
  from the dvdsub encoder, so each gets the caption's length as its
  duration, which mpv honours and ffmpeg's own burn-in does not.

### Measured

Tier 3, the clip (20 s, 2.6 Mbps, a 1 Mbps cap so the server transcodes;
`walk.sh` picks the embedded PGS, the PGS sidecar, the embedded VobSub,
the VobSub sidecar, the srt, off):

| | Before, 12.4 | After, 12.4 | After, 12.1 |
|---|---|---|---|
| Loads for the walk | 6 (one per pick but off) | 3 | 4 |
| Embedded PGS | burned in, reload | file, no reload | file, no reload |
| PGS sidecar | burned in, reload | file, no reload | file, no reload |
| Embedded VobSub | burned in, reload | file, no reload | 400, burned in |
| VobSub sidecar (.mks) | burned in, reload | 400, burned in; a file in a later run | 404, burned in |
| srt | file | file | file |

The server's default subtitle for the clip is the `.mks` sidecar (index
0), so every run starts with its file failing and a reload: that is the
"2 loads" at the start. The direct play of the clip plays the embedded
tracks from the file as before, and the sidecar PGS as a file.

Tier 4, the live server (12.1.0), John Wick: Chapter 2 (9.3 GB, three PGS
tracks and one srt), a transcode at 4 Mbps, a test instance:

| | |
|---|---|
| The srt at start (the server extracts every track of the file) | 36 s |
| The first PGS chosen after that (from the cache), 11.1 MB | under 1 s |
| The second PGS, 14.7 MB | 1 s |

Tier 2 is `image_subtitle_files_are_tracks_of_their_codec_and_map_by_file`
(src/player.rs), which also covers a lost file: not reported when it was
not to start shown, reported when chosen, and no shift of the other ids.

### Left

- The 12.4 walk's VobSub sidecar: 400 on the first runs, served as a
  file on the last (after the server had extracted the file's tracks
  for other requests); the cause was not run down. A server matter
  either way, and the fallback covers it. An `.idx/.sub` pair is not in
  the library and was not tried.
- A film's text track at start still costs the extraction of every track
  of the file on the first play (36 s above); the PGS tracks ride on it.
