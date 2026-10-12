# Events for other programs

Status: proof of concept, stage 1 built (2026-10-08). The format is version 0
and can still change.

## Purpose

Bloom tells other programs on this Mac what the player does, as it happens.
A program reads the events and does what its user wants: it dims the lights
when a film starts, sets a status, sends a notice to a phone.

Bloom only tells. It does not control lights, post to services or run
automations itself.

## Why the client, and not the server

The Jellyfin server already has a Webhook plugin. It sends playback start,
stop, pause and a position about every 10 seconds, for every client. An
administrator can set it up today. Bloom's dashboard can edit its settings
with the general plugin settings editor (Dashboard > Plugins); no special
view is needed for that.

The plugin cannot tell what only the client knows:

| Only Bloom knows | Use |
|---|---|
| The moment of a pause, a seek, a stall or a recovery | Lights that react at once |
| Full screen and picture in picture | No cinema lights while the film is a small window |
| The moment the credits or an intro start | Lights that rise at the credits |
| The audio and subtitle track in use | A status, accessibility tools |
| Downloads on this Mac and plays made offline | A notice when a download is done |
| The cast target | No local automation while the TV plays |
| SyncPlay drift | Diagnosis |

The plugin also needs administrator rights on the server. The events of
Bloom work for every user, on any server.

Conclusion: this is a feature of Bloom. The server plugin stays the right
tool for things that must cover every device (scrobbling, parental limits).

## Where the setting lives

In Settings, not in the server dashboard. The dashboard is for the server;
this is a setting of the app on one Mac, kept in the config file of the app.

- Built: a page of its own, Settings > Automation, with the switch, the
  detail level, the socket path with a copy button, the number of programs
  that read now, and how to try it.
- Stage 2 adds the hook program to that page.
- The same page has the group "Lights (experimental)" (below). Its test
  film shows in a development instance only (one with `BLOOM_DEBUG_SOCK`:
  `dev/run`, `dev/run-test`).

Off by default. Two settings:

| Setting | Config key | Default | Meaning |
|---|---|---|---|
| Events for other programs | `events` | off | Opens the socket |
| Events name the title | `events_titles` | off | Adds title, series, season, episode and year to the item |
| Tell the colours of the picture | `events_colours` | off | Sends `frame.colours`, `detail.opened` and `detail.closed`, for a light that follows the film |

## Transports

### 1. The stream (stage 1, built)

A Unix socket in the folder of the app:

    ~/Library/Application Support/bloom/events.sock

- Mode 0600. If the mode cannot be set, the stream stays off.
- Read-only. Bloom does not read what a reader sends.
- One line of JSON for each event, ended by a newline.
- A test instance uses `events-<BLOOM_TEST_NAME>.sock`.
- Read it with `dev/events`, or `nc -U <path>`.

The first two lines for each reader:

1. `hello`: the app, its version, the format version.
2. `state`: what plays now, so a reader that connects in the middle of a
   film can act at once.

Back pressure: each reader has a queue of 256 lines. A reader that falls a
whole queue behind is closed. Its program sees the end of the stream and
connects again. Bloom never waits for a reader.

The end of the stream (Bloom quit, crashed, or the setting went off) counts
as "playback stopped" for a reader.

### 2. The hook (stage 2, not built)

One program, named in the settings, started for each event:

    <program> <event name>      with the JSON of the event on stdin

- Environment: `BLOOM_EVENT`, `BLOOM_EVENT_SEQ`, `BLOOM_SESSION`.
- One worker thread, a queue of 32 events, 10 seconds for each run.
- Not started for events that come many times a second.
- A test instance never runs the hook.

The hook is for one-shot things (a notice, a status). Anything that lasts
(a fade of the lights, a presence that must stay) needs the stream: two
hook processes for a pause and a resume would overlap and fight.

### Later, by demand

A built-in webhook, a `bloom events` subcommand, a local WebSocket for
browser tools. Each has a cost in security or upkeep; the hook covers most
of them with `curl`.

### Not planned

Commands into the app (play, pause, seek) over this channel. Three ways in
exist already: the media keys, Jellyfin's remote control of the session,
and the debug channel.

## The format

    {"v":0,"seq":412,"time":"2026-10-08T19:04:11.482Z","event":"playback.paused","instance":"default","data":{...}}

| Field | Meaning |
|---|---|
| `v` | Format version. 0 during the proof of concept. From 1 on, fields are added freely; a removed or changed field raises the number. |
| `seq` | Counts up by one for each event of this run of the app. A gap means events were lost. The two lines that greet a reader (`hello`, `state`) are no events: they carry the number of the last event and take none. |
| `time` | UTC time of the event. |
| `event` | The name, as in the list below. |
| `instance` | `default`, or the name of a test instance. |
| `data` | The fields of the event. |

### The playback object

    {"session":3,"item":{"id":"...","type":"Movie"},"position":1840.2,"duration":7200.0,
     "paused":false,"buffering":false,"rate":1.0,"target":"local",
     "window":{"fullscreen":true,"pip":false}}

- `session` is the same for every event of one playback, and moves on at
  each start.
- `item` has `id` and `type`. With "Events name the title" on, it also has
  `name`, `series`, `season`, `episode`, `year`.
- `position` and `duration` are seconds. There is no event for the position
  alone: a reader works it out from `position`, `rate` and `time`.

### Events

Built in stage 1:

| Event | Data | When |
|---|---|---|
| `hello` | app, version, format | First line for a reader |
| `state` | `playback` (or null), `window` | Second line for a reader |
| `playback.started` | playback | An item is loaded and plays |
| `playback.paused`, `playback.resumed` | playback | |
| `playback.seeked` | playback, `from` | The position jumped by more than 2 seconds against the clock |
| `playback.stalled`, `playback.recovered` | playback | The player waits for data, and has it again |
| `playback.rate_changed` | playback | The speed changed |
| `playback.stopped` | playback, `reason`, `played_to_end` | `reason`: `ended`, `user`, `error`, `replaced` (the next item took its place) |
| `segment.entered`, `segment.exited` | session, `kind`, `position` | `kind`: `Intro`, `Outro`, `Recap`, `Preview` |
| `artwork.colour` | session, `colour` (`[r, g, b]`) | A moment after `playback.started`: the strongest hue of the poster, at full strength. Not sent for artwork with no colour (greys), or when the artwork cannot be read (offline). |
| `window.changed` | `fullscreen`, `pip`, `player_open` | Only while the player is open (stage 1 limit). `player_open` is true only while an item plays or is paused: it is false while an item loads and after its end. |

With "Tell the colours of the picture" on (built on 2026-10-08):

| Event | Data | When |
|---|---|---|
| `frame.colours` | session, `colours`: 14 of `[r, g, b]`, from the top of the picture to its bottom | About 12 times a second while a film plays and a program reads (each second frame of a film at 24 a second; never closer than 75 ms). Each is the mean colour of one band of the picture; black bars are left out. Not sent during a pause. |
| `detail.opened` | `item` (id, type), `colours` as above, of the backdrop | A detail page opened and its artwork is read. A light can take the colours at once, so it is with the film from its first frame. Not sent for a page with no artwork, or when the artwork cannot be read. From one detail page to the next there is no `detail.closed` between the two. |
| `detail.closed` | | The detail page that was told is left, or the next detail page has no artwork. Never without a `detail.opened` before it. |

`frame.colours` is the one event that comes many times a second. A reader
that does not want it leaves the setting off; the hook (stage 2) never gets
it.

Planned:

| Event | Stage | Note |
|---|---|---|
| `playback.settled` | 2 | The state did not change for 750 ms. For lights: a seek arrives as pause, seek, resume within a second, and must not flicker them. |
| `tracks.changed` | 2 | Audio and subtitle track: index, language, codec |
| `segment.skipped` | 2 | With `auto` for a skip by a setting |
| `download.queued`, `.finished`, `.failed`, `.removed` | 2 | Needs a comparison of the download entries; today only a change counter exists |
| `cast.target_changed` | 2 | `local`, `jellyfin`, `chromecast`, `airplay` |
| `connection.offline`, `connection.online` | 2 | |
| `syncplay.*`, `quality.changed`, `update.ready` | 3 | |
| `events.dropped` | 2 | For a reader that lost lines but was not closed |

Fields to add to the playback object: the display the window is on, `muted`,
whether the window is visible.

## What an event never has

- The access token, or any URL that carries it.
- The address or the name of the server.
- A path of a file on this Mac.
- The name or the id of the user.

The title of what plays is in an event only with "Events name the title"
on. A reader with titles can keep or send a watch history; the settings
page must say so.

## Security

Any program that runs as the same macOS user can read the socket. That is
the same boundary as the debug channel, with less behind it: the stream
cannot drive the app. The two stay apart, so a user who turns the events on
does not turn on a channel that can write to the server.

## Cost

- Off: one read of a flag for each look at the player.
- On: the UI thread compares two small snapshots at each poll of the
  player (50 ms while the player is open). At an event it builds one line
  and hands it to each reader's queue without a wait. A thread for each
  reader writes.
- No event is sent from the worker of the player or from the render
  thread. The tap is the poll on the UI thread, which already runs.

Check before each stage ships: `dev/jctl perf` and `dev/jctl pacing` with
the stream on and a reader connected, against the stream off.

## How it is built

| Part | Where |
|---|---|
| The module | `src/events/` |
| The hub, the socket, the readers | `hub.rs` (`Hub`) |
| The events between two moments | `diff.rs` (`between`, a pure function over two `Snapshot`s) |
| The colours of a picture | `colours.rs` (`artwork_colour`, `picture_bands`) |
| The tap | `tap.rs`: `Bloom::events_tick` (the poll of the player and the close of the player), `events_artwork`, `events_page` (each change of the page) |
| The lights (helper, lamps, values) and the test film | `lights.rs` (`Lights`, `Helper`, `set_lights`, `lights_scan`, `lights_choose`, `lights_set`, `events_test_run`) |
| The page of the settings | `ui.rs` (`render_settings_automation`, Settings > Automation) |
| The settings | `Config::events`, `events_titles`, `events_colours` |
| The colours of a film frame | `GlRenderer::colours` in `src/video_surface.rs`, called by the render thread in `src/player.rs` (`Player::watch_colours`, `Player::colours`) |
| Debug verb | `dev/jctl stream on|off|titles on|titles off|colours on|colours off|colours|test|lights ...` |
| Tail | `dev/events` |
| The lights helper | `lights/` |

Tests: `between` over a whole playback, the next item, titles and the
window; the socket (hello, state, numbered events, mode 0600, off closes
it); a reader that does not read is closed and holds nothing up.

## The lights helper (experimental, built on 2026-10-09)

What began as an example is a part of Bloom now: a helper program that
Bloom starts, and that drives the lights the user chose. The user's steps
are in the wiki page `docs/wiki/Lights-(experimental).md`.

The line this spec drew holds in the code: Bloom itself speaks no light
vendor's protocol. The helper does, as a program of its own that shares no
code with the app. A fault of a lamp or of a vendor's protocol stays out of
the player.

| Part | Where |
|---|---|
| The helper | `lights/` (the crate `bloom-lights`, a member of the workspace; `cargo build` builds it next to Bloom, `dev/bundle` puts it into the app) |
| The rules: which event asks for which light | `lights/src/rules.rs` (pure, no brand in it) |
| One light of any brand: what it had, fades, the glide of the picture's colours | `lights/src/light.rs` |
| The interface to a brand | `lights/src/driver.rs`: `Driver` (find the lamps, open one) and `Lamp` (its state, a level, a colour, a white, the frame stream) |
| The first brand | `lights/src/govee.rs` |
| Bloom's part: start and end the helper, find and choose lights, the values | `src/events/lights.rs`, the group "Lights (experimental)" in `src/events/ui.rs`, `Config::lights` |

How the two speak:

- Bloom starts `bloom-lights --with-parent --socket <events socket>
  --settings <lights.json>`. The helper reads the events as any reader does.
- The end of the pipe from Bloom ends the helper: when the user turns the
  lights off, when Bloom quits, and when Bloom is killed. The helper puts the
  lights back first.
- The lights the user chose are in the settings file (`"lamps"`: driver, id,
  model, address). The helper reads the file at each event; other lights
  chosen during a film get that film.
- A light is kept by its id. At its start, and when the chosen lights
  change, the helper looks on the network and uses the address a light has
  now; the address of the file is the fallback.
- Find and Blink are one-shot runs of the helper (`--scan`, `--identify`).
  Bloom reads the lines of a find with care: the names in them come from the
  lights.
- A test instance does not start the helper by itself (it would be a second
  helper on the lights of the app the user runs); `dev/jctl stream helper
  on` does.

A new brand is one file with a `Driver` and a `Lamp`, and a line in
`driver::drivers`. A brand is added only with a lamp of it at hand. Where a
driver runs is decided for each brand: in the helper first; a driver can
move into the app later, as the interface does not know where it runs.

Checked on 2026-10-09 with Bloom, a film and a stand-in for the light on
this Mac that answers a find on the network group: the helper is not started
by a test instance; started by the debug verb, it found the stand-in; chosen,
it was asked for its state and followed a detail page and a film; taken out
of the chosen lights during the film it got back what it had; chosen again
it took the film; the lights off during the film gave it back and the helper
ended; Bloom killed (`kill -9`) gave it back and the helper ended. Not
checked on a real light since the helper was made: the light was out of
reach.

A third Codex pass (2026-10-09) on the helper and the pairing found, and
these are fixed: the helper could end before the second command of a
restore that goes down (it waits for it now, `Lights::settle`); two chosen
lamps asked at once lost one answer, as all lamps answer to one port (one
question at a time in the program, and a wait for a port another program
has); the end of the pipe could miss a connection made at the same moment;
other lights chosen during a film made every light anew (a light that stays
is kept as it is now); the colour of the artwork was lost while the picture
was followed; the lights stayed off after "events off, quit, events on"; a
helper that ended by itself still showed as on (looked for at each change
of the page now); "Blink" next to a film; a light whose address reads
`--scan` turned "Blink" into a find (the first word decides now); an id cut
to 64 characters. A lamp that was told nothing gets nothing "back".

Checked after it with two stand-in lamps at 5%: both were asked and both
answered at the start of a film; at a pause both went to 25%; the pipe from
the parent ended during the pause, and both got their colour, 13% and then
5% before the helper ended.

Found by Daniel on the real H6076 on 2026-10-12, and fixed:

- A find missed the lamp about one time in five, on or off (13 of 16 single
  questions were answered): the question is one packet to a group. A find
  asks again every 0.4 seconds now; 12 of 12 finds saw the lamp, switched
  off. A question to the lamp's own address was answered 30 of 30 times.
- A lamp that was off stayed on after "Blink": the helper did not know on
  from off. `LampState::on` and `Lamp::power` (Govee: `onOff`, `turn`). A
  lamp that is off blinks and is off again (checked on the lamp). For a
  film that dims, a lamp that is off is told nothing; to follow the picture
  it is switched on after the stream began, out of a dark first frame, and
  switched off before the stream ends. The film cases are tested with a
  mock lamp only.
- `dev/run` starts Bloom from a light bundle that had no helper in it
  (`dev/devapp` copies it now).

Known and left: the helper is the file next to the program, with no check
of who made it (as for the program itself); a machine on the network can
answer a find as a lamp does, the protocol has no proof; "Blink" by the
debug verb goes by the place in the list, which can change.

Open: macOS asks for the local network only in a bundled app
(`NSLocalNetworkUsageDescription` is in `dev/bundle`), not tried; a find
next to another Govee program waits 2.5 seconds for the port and then
fails.

## Example: Govee lights

The helper reads the stream and drives Govee lights with
Govee's LAN protocol (JSON over UDP, port 4003; "LAN Control" must be on for
the light in the Govee Home app; no cloud, no key).

    target/release/bloom-lights --ip <address of the light>
    target/release/bloom-lights --dry-run

- Start: asks the light for its state, fades to 5%.
- Pause: warm white (2700 K), fades to 25%.
- Resume: the colour of the film again, fades to 5%.
- Credits: fades to 40%.
- End: puts back the brightness and the colour the light had.
- Picture in picture: leaves the light alone.
- A new fade ends a fade that still runs.
- A program that connects in the middle of a film takes the film: the
  `state` line is a start for it, and a pause when the film is paused.
- Picture in picture: the light goes back to what it had, and takes the
  film again when the window is large, with the pause and the artwork
  colour the film has by then. The artwork of a page does not take the
  light while a film plays small.
- The next item of a queue: when Bloom says the next item took the place
  (`reason: replaced`), the light stays with the film. When an item plays
  to its end and the queue goes on, Bloom tells the end first: the light
  goes back, and comes to the next item when it starts. In both cases what
  the light had is asked for once: a start within 1.5 seconds of a restore
  keeps what that restore put back, and does not ask the light in the
  middle of its glide. (A change made to the light by hand in those 1.5
  seconds is lost at the next end.)

Govee has no command for a fade, but the light glides to each level it is
told (well under a second). The program uses that glide: one command on the
way up; two on the way down, 0.4 seconds apart, the first to the level
half-way for the eye. `--gap` sets the time between the two. A scene
that ran on the light before the film cannot be put back: no Govee API
reports it.

Checked on a real light on 2026-10-08 (a Govee H6076 floor lamp, firmware
1.04.05, LAN Control on), with a test instance of Bloom playing a film,
muted and with no report to the server:

- The light answers a state question on port 4002 of the Mac.
- A fade of 12 steps in 3 seconds arrives whole, and shows as a stutter on
  the wall. 67 steps in 0.65 seconds were better and still stuttered: each
  command starts the light's glide again. One command down was smooth but
  quick; two commands 0.4 seconds apart were smooth, and one command up.
  Daniel judged each by eye.
- Start: 72% to 5%. Pause: warm white at 25%. Resume: 5%. Stop: back to 72%
  and the colour it had. The light's own answer and a webcam frame agree at
  each step.
- The events of that run: `playback.started`, `playback.paused`,
  `playback.resumed`, `playback.seeked` (one, with `from`),
  `playback.stopped` (`reason: user`). No event came twice.

Seen in that run: after a pause the light stayed warm white for the rest of
the film. The resume now shows the colour of the film again. A seek from the
debug channel came as one `seeked` event with no pause around it; a seek
with the mouse or a key is not checked yet.

### What the user can set

Options of the command line, and over them a settings file next to the
socket (`lights.json`; `lights-<name>.json` for a test instance). The
program reads the file at each event, with no restart. A changed value is
used from the next event that sets that part of the light: a new film level
at the next start or resume, not at once.

| Option | File | Default | Meaning |
|---|---|---|---|
| `--film` | `film` | 5 | Brightness during a film, percent |
| `--pause` | `pause` | 25 | Brightness at a pause |
| `--credits` | `credits` | 40 | Brightness at the credits |
| `--gap` | `gap` | 0.4 | Seconds between the two commands of a fade down |
| `--pause-kelvin` | `pause_white`, `pause_kelvin` | 2700 | The white of a pause, brought into 2000 to 9000. `0`, or `pause_white: false`: a pause keeps the colour. Each of the two fields counts by itself. |
| `--colour` | `colour` | `keep` | `keep`: the colour the light has. `artwork`: the colour of `artwork.colour`. `r,g,b` (`[r, g, b]` in the file): one colour for every film |

### The light follows the picture (`--aura`)

With `--aura` (file: `aura`, and `aura_glide`, default 0.07 seconds) the
light shows the colours of `frame.colours` along its length, the top of the
picture at its top. No fades and no film colour: the picture is the light.

- It uses the frame stream of the light (the `razer` command, as OpenRGB
  and Govee's DreamView do): 30 frames a second that glide to the newest
  colours. The stream shows a colour at once, with no glide of the light.
- An H6076 takes up to 14 colours a frame and spreads them over its
  length, the first at its bottom. With 15 it shows nothing.
- A colour of the picture is made stronger for the light (a light shows a
  pale colour as white), and a dark picture is a dim light.
- A detail page: the light takes the colours of its backdrop. Play: it goes
  on with the film. Pause: the white of a pause, in the stream. End, or the
  page left: a glide to the light it had, then the stream ends. The glide
  takes about a second, and the program reads no event in that time.
- "Follow the picture" switched on or off during a film: the light gets
  back what it had, and the film starts again under the other rules.
- The light's own brightness and colour are not changed during the stream,
  so the end of the stream puts them back.

Checked on the H6076 on 2026-10-08 with the webcam of the laptop, one shot
of the window and one of the light at the same time: a shop scene (warm
above, blue below), a red close-up (dim red), a blue street (blue), a pause
(pale warm white), the end (the purple it had), a detail page (its
backdrop), Home (the purple again). Daniel saw an earlier version of the
same on the light ("exactly what we want") and asked for the faster glide
and for the light before play; he has not seen this version yet.

Cost in Bloom, 25-second windows of a 24 fps film, twice each: a frame
takes 3.1 ms from mpv's request to rendered with the colours off, and
4.6 ms on average with them on (the looked-at frames take about 9 ms: the
read waits for the GPU). No late frame in any window. Load average 2.5.

One run of the first build left the light untouched for a whole film, with
the events flowing. Three later runs worked, and the cause is not found.

### Reviews of 2026-10-08

Three reviews of the events module, the frame sampling and the example: one
by the author, one by a second reader, one by Codex (GPT-6.1-SOL). Found
and fixed, in the app: event numbers with a gap when a second reader
connects; a reader that left still counted (the count turns the frame
sampling on); an event lost between the state and the list of readers; a
reader accepted after the stream went off; a reader closed as slow that
still got its queue; a title in the greeting after the titles went off; a
false seek after a sleep of the Mac; the sampling left on with the stream
off; artwork of an older visit of a page, or of an older film, taken for
the newest; a detail page with no artwork that kept the colours of the page
before it. In the example: the next item of a queue asked the light in the
middle of its restore; sender threads that piled up; no restore at a closed
socket while a detail page had the light; a program that connected in the
middle of a film did nothing for that film; picture in picture left for
good; bad numbers on the command line; an answer of another light taken for
this one.

Not fixed, by choice: an item whose artwork arrives after the page opened
is not asked for again; a slider of the lab writes its file on the UI
thread (once, 120 ms after the slider rests); a look at a frame that began
before the colours went off and on can still be published (it is of the
frame on screen).

Not done on the light since these fixes: the light was out of reach.
Checked in its place, with Bloom, a film and the example against a stand-in
for the light on this Mac (it answers the state question as the light does
and writes down what it is told):

| Step | What the stand-in was told |
|---|---|
| A detail page | Asked for its state; the stream starts; frames with the colours of the backdrop |
| Play, then a seek | Frames go on: near black for the black start of the film, then the picture |
| Pause | Every colour of a frame the same warm white, at a quarter |
| Resume | The picture again |
| The example killed and started again during the film | Asked for its state; the stream starts; the picture |
| "Follow the picture" off during the film | A glide to its own light; the stream ends; its colour; 72%, then 29% and 5% |
| "Follow the picture" on during the film | Its colour and 72%; then the stream starts; the picture |
| Stop | A glide to its own light; the stream ends; its colour and 72% |
| Home | Nothing |
| A detail page, then Bloom quits | Asked; the stream; the backdrop; at the quit a glide, the end of the stream, its colour and 72% |

28 frames a second, 14 colours in each, no frame with a wrong check byte.
That run found one more fault: the brightness of a restore went out from a
thread and could arrive after the start of the stream. A fade now sends its
first command before it returns.

What only the real light can show stays open: how the light takes a
brightness command next to the stream, and how the glides look.

### The lights on the settings page

The group "Lights (experimental)" on Settings > Automation, shown to every
user:

- "Control lights": starts and ends the helper (`Config::lights`), and
  turns the events on when they are off.
- "Find": the lights that answer, each with "Blink" and a box to choose it.

- "Follow the picture" and its speed (`aura`, `aura_glide`); the switch
  also turns "Tell the colours of the picture" on.
- A slider for each brightness, for the time of a fade down, for the warmth
  of the white of a pause and for the hue of the one colour; a switch for
  the white of a pause; a button that goes through the three colour choices.
- "Test film" (a development instance only): sends `playback.started`, `artwork.colour`,
  `playback.paused`, `playback.resumed`, `segment.entered` (credits) and
  `playback.stopped`, 3 seconds apart, with no film. Each has `"test": true`
  and the session 0. A film of the app ends a test film.
- The page writes the settings file. Bloom still sends nothing to a light.
- The debug channel does the same: `dev/jctl stream helper on|off`,
  `stream scan`, `stream lamps`, `stream choose <n>`, `stream identify <n>`,
  `dev/jctl stream test`,
  `dev/jctl stream lights film=8 gap=0.5 colour=artwork|keep|fixed hue=200
  pause_white=true pause_kelvin=3200`.

Checked on 2026-10-08 with the program in `--dry-run`: the test film gave
the colour, 41% then 8% (film 8, from 100), white and 25% at the pause, the
colour and 11% then 3% at the resume (film set to 3 during the run), 40% at
the credits, 100% at the end. A real film gave `artwork.colour`
`[140, 224, 255]` for its poster; the strength of that colour was raised
after it and is not yet judged on a light.

Bloom itself does not speak a light vendor's protocol: the helper does, and
the same stream serves Home Assistant and any other program.

## Other uses checked

| Use | Needs | Works |
|---|---|---|
| Phone notice when a download is done | Hook, `download.finished`, ntfy or Pushover | Yes; the easiest |
| Focus while a film plays | Hook, two Shortcuts, `shortcuts run` | Yes; about a second late |
| "Now watching" file for OBS, SwiftBar, Raycast | Hook or stream, titles on | Yes |
| Slack status | Hook, a user token | Yes; can expire at the film's end |
| Discord presence | Stream (the presence ends when the connection closes), titles on | Yes; not from a hook |
| Home Assistant | Hook that posts to a webhook trigger | Yes; then any light or device |

## Stages

1. **Built.** The stream, the playback, segment, window and artwork colour
   events, the two settings, the debug verb, the tail script, the lights
   helper with its settings file, and the lights on the settings page.
2. The hook. `playback.settled`. Downloads, tracks, cast target,
   connection. Window events while the
   player is closed. An example hook for a phone notice.
3. By demand: a built-in webhook, SyncPlay and quality events, a `bloom
   events` subcommand.

## Open questions

1. Version 0 for how long? Proposal: until stage 2 has shipped in one
   release, then 1.
2. Should a test instance emit by default? Proposal: the stream yes (own
   socket), the hook never.
3. Should Bloom start a reader program itself (a login item for
   `bloom-lights`)? Proposal: no; document `launchd` for it.
4. Which Govee models are in the house, and do they have LAN Control? An
   H6076 works; others are not tried.
5. The lab stays out of the app a user installs. Should the settings of the
   lights get a page of their own for users later, and then where does the
   program that speaks to the light live?
6. `artwork.colour` takes the poster. Is the backdrop the better picture?
7. A light with more or fewer than 14 colours: should the number of bands
   be a setting of Bloom, or does the reader fold 14 into what it has?
8. Should a pause in the aura keep the last picture, dimmed, as a choice?
