---
name: Bloom
description: A native Jellyfin client for the Mac and the iPhone, one ink on black with glass over the film art
colors:
  theatre-black: "#101010"
  screen-white: "#f5f5f7"
  screen-white-quiet: "rgba(245, 245, 247, 0.6)"
  screen-white-dim: "rgba(245, 245, 247, 0.5)"
  screen-white-loud: "rgba(245, 245, 247, 0.7)"
  smoked-glass: "rgba(42, 42, 42, 0.72)"
  smoked-glass-surface: "rgba(42, 42, 42, 0.69)"
  liquid-tint: "rgba(28, 28, 31, 0.35)"
  glass-edge: "rgba(245, 245, 247, 0.2)"
  glass-highlight: "rgba(255, 255, 255, 0.12)"
  disc: "rgba(204, 204, 207, 0.25)"
  scrim: "rgba(0, 0, 0, 0.6)"
  hero-shade: "rgba(0, 0, 0, 0.55)"
  ink-on-white: "#121212"
  plain-control: "rgba(40, 40, 40, 0.8)"
  live-green: "#7ee787"
  attention-amber: "#e3b341"
  failed-red: "#ff7b72"
  danger-fill: "#c62828"
typography:
  display:
    fontFamily: "Google Sans, Geist, -apple-system, sans-serif"
    fontSize: "27px"
    fontWeight: 700
    lineHeight: 1.1
  headline:
    fontFamily: "Google Sans, Geist, -apple-system, sans-serif"
    fontSize: "22px"
    fontWeight: 600
    lineHeight: 1.2
  title:
    fontFamily: "Google Sans, Geist, -apple-system, sans-serif"
    fontSize: "17px"
    fontWeight: 500
    lineHeight: 1.3
  body:
    fontFamily: "Google Sans, Geist, -apple-system, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: 1.4
  label:
    fontFamily: "Google Sans, Geist, -apple-system, sans-serif"
    fontSize: "12px"
    fontWeight: 500
    lineHeight: 1.3
  mono:
    fontFamily: "Geist Mono, Menlo, monospace"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.5
rounded:
  row: "12px"
  control: "14px"
  pill: "18px"
  sheet: "20px"
  card: "24px"
  connect: "28px"
spacing:
  unit: "4px"
  tight: "8px"
  row: "12px"
  gutter-phone: "16px"
  gap: "22px"
  card-gap: "27px"
  card-padding-phone: "24px"
  card-padding: "32px"
motion:
  fast: "150ms"
  normal: "200ms"
  easing: "ease-out quint"
  push-distance: "32px"
  rise-distance: "28px"
  settle-distance: "6px"
  lift-distance: "12px"
components:
  button-primary:
    backgroundColor: "{colors.screen-white}"
    textColor: "{colors.ink-on-white}"
    typography: "{typography.title}"
    rounded: "{rounded.control}"
    height: "44px"
    padding: "0 16px"
  button-primary-phone:
    backgroundColor: "{colors.screen-white}"
    textColor: "{colors.ink-on-white}"
    rounded: "{rounded.control}"
    height: "50px"
    padding: "0 16px"
  button-plain:
    backgroundColor: "{colors.plain-control}"
    textColor: "{colors.screen-white}"
    typography: "{typography.body}"
    rounded: "{rounded.row}"
    height: "38px"
    padding: "0 16px"
  button-danger:
    backgroundColor: "{colors.danger-fill}"
    textColor: "#ffffff"
    rounded: "{rounded.row}"
    height: "38px"
    padding: "0 16px"
  button-quiet:
    backgroundColor: "{colors.glass-highlight}"
    textColor: "{colors.screen-white}"
    rounded: "{rounded.control}"
    height: "44px"
    padding: "0 16px"
  input:
    backgroundColor: "rgba(68, 68, 68, 0.3)"
    textColor: "{colors.screen-white}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    height: "44px"
    padding: "0 10px"
  input-phone:
    backgroundColor: "rgba(68, 68, 68, 0.3)"
    textColor: "{colors.screen-white}"
    rounded: "{rounded.control}"
    height: "50px"
    padding: "0 12px"
  menu-sheet:
    backgroundColor: "{colors.smoked-glass}"
    textColor: "{colors.screen-white}"
    typography: "{typography.body}"
    rounded: "{rounded.sheet}"
    padding: "12px"
  menu-row:
    backgroundColor: "transparent"
    textColor: "{colors.screen-white}"
    typography: "{typography.body}"
    rounded: "{rounded.row}"
    height: "38px"
    padding: "0 12px"
  menu-row-selected:
    backgroundColor: "{colors.glass-highlight}"
    textColor: "{colors.screen-white}"
    rounded: "{rounded.row}"
  card-surface:
    backgroundColor: "{colors.smoked-glass-surface}"
    textColor: "{colors.screen-white}"
    rounded: "{rounded.card}"
    padding: "{spacing.card-padding}"
  poster:
    backgroundColor: "{colors.theatre-black}"
    rounded: "12px"
  chip:
    backgroundColor: "{colors.glass-highlight}"
    textColor: "{colors.screen-white}"
    typography: "{typography.label}"
    rounded: "{rounded.pill}"
    height: "28px"
    padding: "0 12px"
  tab-bar:
    backgroundColor: "{colors.smoked-glass}"
    textColor: "{colors.screen-white-quiet}"
    typography: "{typography.label}"
    rounded: "{rounded.card}"
    height: "62px"
  tab-selected:
    backgroundColor: "{colors.glass-highlight}"
    textColor: "{colors.screen-white}"
    rounded: "{rounded.sheet}"
---

# Design System: Bloom

## Overview

**Creative North Star: "Glass Over the Poster"**

Bloom is a dark room with the film art as its only light. The page is
Theatre Black, the text is one ink, Screen White, and every surface the
interface needs is a pane of smoked glass laid over the artwork: the
account sheet, the player's panels, the tab bar on the phone, the card
on the connect screen. Colour belongs to the posters and the backdrops;
the interface never competes with them. The mood is cinematic, quiet and
precise: the home page opens on a full-bleed backdrop with the title
treatment and a single white Play button, and everything else sits one
step back.

The system is the same on the Mac and the phone and native on each. The
Mac has the top bar with the server name and the libraries, the floating
poster on a detail page and the pointer's hover states. The phone has a
glass tab bar at the bottom, menus as sheets that rise from the bottom,
three posters across, and fields and buttons at the sizes a finger
expects. Shared code is an implementation detail; what the user sees is
their platform's app.

Density is generous. Rows are 38 points or taller, cards have 27 points
between them, text stays at 15 points for reading and 12 for detail, and
nothing is packed to fit. Two things were tried and rejected: a dark
scrim over the glass, which read as a foreign blob, and a white wash with
a uniform ring, which read as a flat grey outlined button. Glass controls
draw no ring.

**Key Characteristics:**
- One ink, Screen White, at three opacities; colour means a state, never a kind of thing
- Smoked glass surfaces with a light edge; depth from translucency, never from shadows
- Film art full-bleed behind the interface, with a shade only where text must stay readable
- Native structure per platform: top bar and hover on the Mac, tab bar and sheets on the phone
- Google Sans for everything the user reads, Geist Mono for logs and paths

## Colors

A monochrome interface over the colours of the library: black, one white
at three strengths, smoked grey glass, and three state colours.

### Primary
- **Screen White** (#f5f5f7): the one ink. Text, icons, the Play button's
  fill, the selected tab. The accent is the ink itself, used solid on a
  black ground; there is no second accent.
- **Ink on White** (#121212): the text of a white button, and the ink of a
  selected row's icon.

### Neutral
- **Theatre Black** (#101010): the page ground on every screen, under the
  artwork and visible only where there is none.
- **Screen White, quiet** (60%), **dim** (50%) and **loud** (70%): the
  same ink at lower opacity for secondary text, group labels, details
  under a row, and the quiet links of a card. Opacity is the hierarchy;
  the hue never changes.
- **Smoked Glass** (rgba(42, 42, 42, 0.72)): the tint of every popup,
  sheet, panel and bar, blurred over what lies behind it.
- **Smoked Glass, surface** (rgba(42, 42, 42, 0.69)): the same tint on a
  card or a dialog in frosted mode.
- **Liquid Tint** (rgba(28, 28, 31, 0.35)): the tint of the liquid glass
  shader in dark mode, with its bezel and rim; the lens the Mac's popups
  and the phone's tab bar keep.
- **Glass Edge** (Screen White at 20%): the one-pixel light edge of every
  glass surface.
- **Glass Highlight** (white at 12%): a hovered or selected row, a chip,
  a quiet button.
- **Disc** (rgba(204, 204, 207, 0.25)): the quiet circle an icon sits on
  in navigation and lists.
- **Scrim** (black at 60%) and **Hero Shade** (black at 55%): the dark
  layer behind a dialog, and the shade over the lower part of a backdrop
  so the title and the description stay readable.

### State
- **Live Green** (#7ee787): a session that plays, a task that runs, a
  device that is online. Shown as a dot or a short word.
- **Attention Amber** (#e3b341): something that needs a look.
- **Failed Red** (#ff7b72): a failure. **Danger Fill** (#c62828) fills a
  button whose action stops or deletes something.

### Named Rules
**The One Ink Rule.** Text and icons are Screen White, quieter text is the
same ink at lower opacity, icons sit on the disc. A different colour for
each kind of thing, tinted tiles, coloured badges and gradients on cards
were rejected as slop on 2026-10-03.

**The Colour Is State Rule.** Green, amber and red mean live, needs a
look, and failed. A badge is a quiet pill; a state shows as a dot. No
other colour is the interface's own; the rest comes from the artwork.

## Typography

**Display Font:** Google Sans (with Geist, then the system sans)
**Body Font:** Google Sans (with Geist, then the system sans)
**Label/Mono Font:** Geist Mono (with Menlo)

**Character:** one geometric sans at a few sizes and three weights, set
quietly. The drama is in the film's own title art on the backdrop, so the
interface type never shouts; the hero's bold title appears only when the
film has no title art to show.

### Hierarchy
- **Display** (700, 27px, 1.1): the hero's title when no title art exists;
  the title of a detail page on the phone.
- **Headline** (600, 22px, 1.2): the title of a detail page on the Mac,
  "Connect to a server", "Who is watching?".
- **Title** (500, 17px, 1.3): the name of a row on the phone ("Dashboard",
  "Settings"), a section heading ("Recently Added in Movies", "Cast &
  Crew"), the label of a 50-point button.
- **Body** (400, 15px, 1.4): a menu entry, a description, a setting's
  value, the label of a 44-point button. Descriptions on the Mac are
  capped to a readable measure.
- **Label** (500, 12px, 1.3): the quiet detail under a row, a chip, a
  group heading over a card's fields (13px on the phone), the tab bar's
  words.
- **Mono** (400, 13px, 1.5): logs, paths, the Quick Connect code.

### Named Rules
**The Phone Reads at 17 Rule.** On a phone, a field or a button is 50
points tall with 17-point text, the body size of Dynamic Type; a tap
target is never under 44 points. The Mac keeps 44-point fields and 15-point
text.

## Layout

The Mac window is a full-bleed canvas: a 64-point top bar with the server
name and the libraries in the middle and the account at the right, over
the page. Home and detail pages run their backdrop under the bar; other
pages start below it. The side padding is 3.3% of the width plus 9
points, so a 1440-point window has about 56 points of margin. A row of
posters shows a count that depends on the width: five portrait cards
across under 1200 points, six at 1200, seven at 1400, eight at 1600,
nine at 1920, with 27 points between cards. A detail page on the Mac
floats the poster at the left and runs the text beside it.

A compact window, under 700 points wide, is a phone. The structure
changes to the platform's: a glass tab bar of 62 points at the bottom
with home, up to three libraries, favourites when they fit, and search; a
top bar with back, the title of the page and the account; menus as
sheets that rise from the bottom and take the whole width; three posters
across with 16 points of side padding; the detail page in one column with
the hero text the width of the phone; the dashboard and Settings as a
list of sections first, each section a page of its own. The bars take the
safe area, the page keeps the keyboard's height free and moves with it
as it slides, and a card centres in the room above the keyboard. The iPad
keeps the Mac's layout with the tabs as glyphs under 900 points.

The spacing unit is 4 points. Rows are 38 points or taller with 12 points
of padding and 12-point corners; groups sit 22 points apart; a card's
padding is 32 points on the Mac and 24 on the phone.

## Elevation & Depth

No shadows. Depth is translucency: a surface is a pane of smoked glass
with a one-pixel light edge, blurred over the artwork or the page behind
it, and what lies under it shows through. On the Mac the glass is a lens,
the liquid shader's bezel of 16 and thickness of 20 with a rim of 0.4,
and the middle half stays clear (frost 0.5) so a popup over posters keeps
them visible. On a phone a sheet lies over the page's text, so a popup
frosts over fully (frost 1, blur 1) and only the tab bar keeps the lens;
cards keep theirs on both. While the player is open, the glass samples a
copy of the video frame, so a panel over the film is glass over the film.

A dialog adds one more layer: a scrim of black at 60% over the page, and
the dialog's glass over that.

### Named Rules
**The No Shadow Rule.** Nothing casts a shadow. If a surface needs to
read as above another, it is glass with a light edge, and the blur does
the lifting.

**The Glass Shows Through Rule.** A glass surface never hides what it
covers; a dark scrim over glass and a white wash under a ring were both
rejected. On a phone the frost rises to keep text readable, and that is
the only exception.

## Motion

Things come in; nothing bounces, and nothing moves for its own sake. One
curve, an ease-out (quintic), and two durations: 150 ms for what changes
in place and 200 ms for what arrives. Five entrances cover the app
(`ui::motion`): a page pushed onto the one before slides in from the right
by 32 points while it fades in, as iOS pushes a page (every page a phone
opens: a library from the tab bar, a title from a card, a section from
the dashboard's list); a page that replaces another in the same frame
lifts 12 points into place while it fades in (every page on the Mac, and
a new page of a library's grid on both); a sheet rises 28 points from the
bottom of the window; a popup or a dialog settles down 6 points into place
over a fading scrim; and content that changes in place, such as the
section beside the Mac's sidebar or the page a phone returns to with
Back, fades in. Back is quieter than forward: it fades, it does not
slide. The player's controls and the toasts fade with the same timing;
the hero's backdrop cross-fades over 500 ms, the one slow movement, and
the splash is the one set piece.

Reduce Motion, from the system or the theme, turns the movement off:
everything appears in place, at once, with the same state feedback.

### Named Rules
**The Arrival Rule.** Motion marks where something came from: pushed pages
from the right, sheets from the bottom, popups from just above their
place, a replaced page from just below it. Nothing else moves.

**The Quiet Return Rule.** Going back fades; only going forward slides.

## Shapes

Everything is rounded, and the radius grows with the size of the thing.
A row and a chip's parent take 12 points; a control, a field and a button
take 14; a pill, such as the back button over the connect screen, is a
full 18-point capsule; a sheet is 20; a card, a panel and the tab bar are
24; the connect card is 28. Posters and backdrops are clipped at 12
points. Borders are the glass edge, one pixel of Screen White at 20%, and
nothing is outlined otherwise: glass controls draw no ring in liquid mode.
An icon sits on a circular disc.

## Components

Controls are refined and restrained: one weight of emphasis, the white
fill for the one main action of a page, quiet glass for the rest, and no
decoration beyond the glass itself.

### Buttons
- **Shape:** rounded to the control radius (14px); 44 points tall on the
  Mac, 50 on the phone, 16 points of side padding.
- **Primary:** Screen White fill with Ink on White text (#121212), medium
  weight. One per page: Play on a hero and a detail page, Sign In, Connect.
- **Quiet:** white at 12% with Screen White text; the second action
  beside a primary, such as Use Quick Connect.
- **Plain** (dashboard): rgba(40, 40, 40, 0.8) fill, 38 points tall,
  12-point corners; under the pointer it takes the accent with dark text.
- **Danger:** #c62828 fill with white text, for an action that stops or
  deletes.
- **Icon buttons:** the icon alone on a disc, 44 points, used for the
  hero's info and favourite, and the detail page's download, played,
  favourite and more.
- **Hover:** the fill dims to 88% opacity; a quiet row takes the glass
  highlight. The phone has no hover; a pressed state is the same highlight.

### Chips
- **Style:** a quiet pill of white at 12% with 12-point label text in
  Screen White; the rating with its star, the year, the age rating, the
  end time.
- **State:** a chip never carries a colour; a state is a dot beside the
  text.

### Cards / Containers
- **Corner Style:** 24 points for a card, a panel and a dialog; 28 for the
  connect card.
- **Background:** smoked glass over the page; in frosted mode the surface
  tint, in liquid mode the lens.
- **Shadow Strategy:** none; see Elevation & Depth.
- **Border:** the glass edge, one pixel.
- **Internal Padding:** 32 points on the Mac, 24 on the phone; 22 points
  between groups inside.

### Posters and rows
- A poster is the artwork clipped at 12 points with its title and a
  detail line under it in 52 points of text room. A row of posters scrolls
  sideways with arrows at the right of its heading on the Mac and a
  finger pan on the phone. On hover the Mac shows the played, favourite
  and more buttons on the image; on the phone a long press opens the
  item's menu.
- An episode row has a wide still at the left and the text beside it.

### Inputs / Fields
- **Style:** a dark translucent field (rgba(68, 68, 68, 0.3)) with the
  glass edge, 14-point corners, 44 points tall on the Mac and 50 on the
  phone, placeholder in the quiet ink.
- **Focus:** the edge brightens to the ring grey; no glow.
- **Error:** the edge turns Failed Red; a message in body text under the
  group explains in plain words.
- **Disabled:** the field at half opacity.
- **Phone keyboards:** each field names its keyboard: URL with Go for an
  address, no correction and Next for a username, secure with Go and the
  keychain for a password, the number pad for a code, predictions with
  Search for the search field, word capitals with Done for a name. A long
  press opens the system's edit menu.

### Navigation
- **Mac top bar:** 64 points, transparent over the page; the server name
  at the left of the middle group, the libraries as glyph-and-word tabs,
  the selected one on a glass highlight pill; cast, Jellyseerr, search and
  the account disc at the right. Hover lifts a tab to the highlight.
- **Phone tab bar:** a 62-point glass capsule of 24-point corners floating
  10 points above the home indicator, five entries of glyph over 12-point
  word, the selected one on a highlight pill; it lights only Search on the
  search page and nothing on settings, the dashboard and downloads, and
  steps aside while the keyboard is up.
- **Phone top bar:** back arrow, the page's title in title type, the
  account disc at the right.
- **Menus and sheets:** the Mac's popups are glass cards of 20-point
  corners with 12-point padding; rows 38 points tall with an icon, a title
  in body text and a 12-point detail line, separated by hairlines in the
  quiet ink. On the phone the same list is a sheet from the bottom, full
  width, and a submenu takes the sheet over with a row back. One popup at
  a time: a menu that opens closes every other popup and panel.

### The hero
The signature surface: a full-bleed backdrop with the film's title art at
the left (or the bold display title when there is none), the rating,
year, age and end-time chips, the genres in the quiet ink, the
description, then the info disc, the white Play and the favourite disc.
A shade of black at 55% rises from the bottom so the text reads over any
image, and a position counter with a thin progress bar sits at the right.
On the phone the same elements stack in one column at the width of the
screen.

### The player
The video fills the window with no background of its own. The controls
are glass panels inside the safe area: a transport bar at the bottom with
the seek slider and its preview, and panels for subtitles, episodes,
settings and cast that sit in a corner. A tap on the video shows or hides
the controls, which hide a few seconds after the last touch. On a phone
the episode picker is a sheet at the right over the video.

## Do's and Don'ts

### Do:
- **Do** use Screen White (#f5f5f7) for all text and icons, and lower its
  opacity to 70%, 60% or 50% for secondary, detail and quiet text.
- **Do** draw every popup, sheet, panel and bar as smoked glass with the
  one-pixel light edge, rows of 38 points or more with 12-point corners
  and 12-point padding, 15-point body text and 12-point detail text.
- **Do** let the artwork carry the colour; keep the interface black, white
  and grey.
- **Do** keep one white primary button per page and make every other
  action quiet glass.
- **Do** build the phone's structure from the platform's own patterns: a
  tab bar, sheets from the bottom, 50-point fields with 17-point text,
  44-point tap targets, the safe area respected.
- **Do** keep a menu, the episode list and a panel exclusive: open one,
  close the rest.
- **Do** give what arrives its entrance from `ui::motion` (push, rise,
  settle or fade) and nothing more; honour Reduce Motion.

### Don't:
- **Don't** give a kind of thing a colour: no rainbow avatars, tinted icon
  tiles, coloured category badges or gradients on cards.
- **Don't** cast shadows. Depth is translucency and the light edge.
- **Don't** put a dark scrim over glass, or a white wash with a uniform
  ring around a glass control; both were rejected.
- **Don't** add a second accent colour. Green, amber and red are states,
  not accents.
- **Don't** reinvent a native control on the phone for flavour; the glass
  is the brand, the structure is the platform's.
- **Don't** shrink text below 12 points, or a tap target below 44.
- **Don't** animate for show: no bounce, no spring, no movement on hover,
  and no entrance longer than 200 ms.
