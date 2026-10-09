# Lights (experimental)

Bloom can dim your lights while a film plays, or let them follow the colours of the picture. At the end of the film each light gets back the brightness and the colour it had.

> **Experimental.** This feature is new and can change. It is tested on one light, a Govee H6076 floor lamp. It is in development builds; it is not in a release yet. Tell us what works and what does not in the [issues](https://github.com/dantech2000/bloom/issues).

## What you need

- A Mac with Bloom.
- A Govee light that supports "LAN Control". Most Govee Wi-Fi lights from recent years do.
- The light and the Mac on the same network.

No Govee account, key or internet connection is used. Bloom speaks to the light on your own network only.

## Set it up

### 1. Turn on LAN Control for the light

1. Open the **Govee Home** app on your phone.
2. Open the light.
3. Tap the gear at the top right.
4. Turn on **LAN Control**.

Do this once for each light. If the switch is not there, update the firmware of the light in the same app.

### 2. Turn on the lights in Bloom

1. Open **Settings > Automation**.
2. Turn on **Control lights** in the group "Lights (experimental)".

Bloom turns on "Send events" as well, because the lights need it. macOS can ask whether Bloom may find devices on your local network. Allow it.

### 3. Find and choose your lights

1. Press **Find**. Bloom looks on your network for two seconds.
2. Each light that answers shows as a row, for example "Govee H6076".
3. Press **Blink** on a row to see which light it is. The light goes bright and dim twice. Blink works when no film plays.
4. Tick the box of each light that Bloom may use.

Bloom keeps a light by its own id, not by its network address. When the router gives the light a new address, Bloom finds it again by that id when a film starts. If the light does not answer then, Bloom tries the last address it knew.

### 4. Play a film

Open a film and press play. Your lights now follow the player.

## What the lights do

| Moment | Normal mode | "Follow the picture" |
|---|---|---|
| You open the page of a film | Nothing | The light takes the colours of the film's artwork |
| The film starts | The light dims to the film level | The light shows the colours of the picture |
| Pause | Warm white at the pause level | Warm white at the pause level |
| Resume | Back to the film level | Back to the picture |
| The credits start | The light rises a little | No change |
| The film ends, or you stop it | The light gets back what it had | The light gets back what it had |
| Picture in picture | The light gets back what it had | The light gets back what it had |
| The window is large again | The light dims again | The light follows the picture again |

## Follow the picture

Turn on **Follow the picture** to make a light show the colours of the film. The top of the picture is the top of the light, and the bottom is the bottom. A light with several segments shows several colours at once.

Bloom reads the colours from the film frame itself, about 12 times a second. No camera is used. Black bars above, below and beside the picture are left out.

**Speed of the picture's light** sets how fast the light comes to a new colour. A low value follows a cut at once. A high value is calm.

## Settings

| Setting | Default | What it does |
|---|---|---|
| Light during a film | 5% | The brightness while a film plays (normal mode) |
| Light at a pause | 25% | The brightness while the film is paused |
| Light at the credits | 40% | The brightness from the start of the credits (normal mode) |
| Time of a fade down | 0.4 s | The time between the two steps of a dim. The light glides to each step by itself. |
| White light at a pause | On | Off: a pause keeps the colour |
| Warmth of the white | 2700 K | Low is warm, high is cool |
| Colour during a film | The light's own | "From the artwork" uses the strongest colour of the poster. "One colour" uses the colour you set. |

A change of a brightness is used from the next start, pause or resume. A change of "Colour during a film" is used from the next film.

## If something does not work

**"No light answered."**

- Is LAN Control on for the light in the Govee Home app?
- Is the light on the same network as the Mac? A guest network or a separate "IoT" network often blocks this.
- Did you allow Bloom to find devices on your local network? Check **System Settings > Privacy & Security > Local Network**.
- Is another program for Govee lights open on this Mac? Only one program at a time can hear the answers of the lights. Bloom waits a moment for it; if Find still shows nothing, close the other program and press Find again.

**The light does not change when a film plays.**

- Is the box of the light ticked?
- Press **Blink**. If the light does not blink, Bloom cannot reach it. Press **Find** again.
- Is the film in picture in picture? The lights wait until the window is large again.

**The light does not come back at the end.**

Turn **Control lights** off and on. The light then gets back what it had when the next film starts and ends. If the light did not answer when the film started, Bloom does not know what it had, and sets it to full brightness.

**The light follows the picture late, or the colours look wrong.**

Tell us the model of the light in an issue. The fast mode uses a part of the Govee protocol that Govee does not document. Other models can take a different number of colours, or run from top to bottom in the other direction.

## What is known not to work yet

- A light that does not answer when a film starts: Bloom does not know what it had, and sets it to full brightness at the end.
- A light you tick while the page of a film is open takes the next film, not the artwork of that page.
- Two copies of Bloom with "Control lights" on, for the same light, work against each other.

- Lights of other brands (Philips Hue, LIFX, WLED, Nanoleaf).
- A light with no "LAN Control".
- A scene or an effect that ran on the light before the film is not put back. The light gets back its brightness and its plain colour.
- When an episode ends and the next one starts by itself, the light goes back to its own light for a moment between the two.
- A setting that you change during a pause is used when the film goes on.

## How it works, and your privacy

- Bloom starts a small helper program, `bloom-lights`, when "Control lights" is on. The helper ends when you turn the setting off or quit Bloom, and it gives the lights back first.
- Bloom tells the helper what the player does: start, pause, resume, end, and the colours of the picture. The helper tells the lights.
- Nothing goes to the internet. The helper sends to the lights you chose, on your own network.
- The events have no title of what you watch unless you turn on "Name the title in events". They never have your server address, your account or a file path.

## For developers

- The events are a stream of JSON lines on a socket of this Mac. Any program can read them, not only the lights helper. See [`specs/outbound-events.md`](https://github.com/dantech2000/bloom/blob/main/specs/outbound-events.md).
- The helper is the crate in `lights/`. A brand of light is one file with a driver: find the lamps, ask one for its state, set a level, a colour or a white, and send a frame. `lights/src/driver.rs` has the interface and `lights/src/govee.rs` is the first driver.
- The helper runs by hand as well:

  ```sh
  bloom-lights --scan                      # the lights that answer, one line each
  bloom-lights --identify govee <address>  # makes one light blink
  bloom-lights --ip <address>              # follows Bloom with one Govee light
  bloom-lights --dry-run                   # prints what a light would be told
  ```

- We add a brand only when someone can test it on a real light. WLED is the likely next one.
