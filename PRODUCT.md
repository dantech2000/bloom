# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

One Rust codebase with a Mac app and an iOS app. Each is meant to feel
native on its platform, menus and navigation included; the Mac keeps the
desktop layout, the phone has its own, the iPad still follows the desktop
layout (confirmed 2026-10-22).

## Users

Daniel and the accounts of his household on his own Jellyfin server, today:
family members with their own profiles, watching on a Mac, an iPhone and,
later, an iPad. The job is watching the library: pick something up where it
was left, find a title, watch an episode or a film at the quality the file
has, sometimes together with someone on another device, sometimes offline.
Daniel also administers the server from the same app.

A public release is the intent once the app holds up at home (confirmed):
anyone with a Jellyfin server, from GitHub releases on the Mac and a store
or TestFlight on iOS. Not yet true; nothing is designed for a mass audience
until it is.

## Product Purpose

Bloom is a native Jellyfin client. It exists so that a Jellyfin library is
watched through an app that is fast, plays the original file, and looks
and behaves like a native app on the Mac and on the phone, rather than
through the web client or a wrapper around it. Success at home: the
household reaches for Bloom first, and nothing about playback or sign-in
needs explaining.

## Positioning

The whole server in one client (confirmed): browsing, playback, SyncPlay,
casting to other devices, downloads and the complete administrator
dashboard in one native app. A neighbouring client offers the watching
part; Bloom also runs the server from the same window, on the same
account.

Supporting facts the app already delivers (from the README and the code):
direct play of the original file through mpv with frame pacing locked to
the display, and nothing that lowers quality unless the user turns it on;
SyncPlay with drift correction; Chromecast and AirPlay; downloads with
resume and offline playback.

## Operating Context

- A home Jellyfin server (Jellyfin 12.1) reached on the house network
  directly and from outside through Cloudflare. The library is real:
  films and series, some in 4K.
- Devices: a MacBook Air, an iPhone, an iPad; a TV box and an Apple TV
  the app can cast to.
- The Mac app updates itself from GitHub releases. The iOS app is
  installed from the Mac with a developer certificate for now.
- Other Jellyfin clients in the house can control Bloom and join its
  SyncPlay groups.

## Capabilities and Constraints

- Rust throughout: GPUI draws the interface, libmpv plays the video
  (MPVKit on iOS). No web view anywhere in the interface.
- Features: home with Continue Watching and Next Up, libraries with sort
  and filter, series and seasons, collections, playlists, search with a
  local title index, a player with chapters, seek previews, intro and
  credit skip, audio and subtitle choice, speed, picture in picture (Mac),
  a Quality menu, SyncPlay, Play on another device, downloads, the server
  dashboard, account settings, Jellyfin Enhanced settings, Jellyseerr
  requests.
- The phone has a first pass of its own layout; the iPad has none yet.
- Terminology follows Jellyfin's: libraries, Next Up, Continue Watching,
  SyncPlay, Quick Connect, Cast.
- Licence AGPL-3.0-or-later. Forked from jellyui; the first four commits
  are that project's.
- Undecided: the iOS distribution channel (TestFlight or the App Store),
  and when the public release happens.

## Brand Commitments

- The name is Bloom, the tagline "Jellyfin client". The icon is
  assets/icon/bloom.png.
- Established by Daniel in the repo's notes and binding: the app is dark
  only; text and icons use one ink, and colour means a state, never a
  category; surfaces are the glass of the theme and popups share one look.
- Daniel's direction: menus and navigation should feel native on each
  platform.

## Evidence on Hand

- The real library on Daniel's server, for screenshots and demonstrations.
- The README's feature list, which is true of the shipped Mac app.
- No testimonials, press, customers or benchmarks exist. Future work must
  not invent any.

## Product Principles

- Play the file as it is. Quality is lowered only when the user asks.
- Native on each device. A Mac app on the Mac, a phone app on the phone;
  shared code is an implementation detail the user never sees.
- One app for the whole server. Watching and administering are the same
  account in the same window.
- Tested on the real library before it is called done, on the device it
  ships on.
- The household first. Anything the family would need explained is not
  finished.

## Accessibility & Inclusion

VoiceOver works on iOS through a bridge from the interface's accessibility
tree; interactive controls carry roles and labels, which the touch tests
also rely on. No further standard has been set.
