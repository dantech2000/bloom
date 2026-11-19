import BloomCore
import MPVUI
import SwiftUI

/// A menu entry with a checkmark when chosen.
struct Checked: View {
    let title: String
    let on: Bool

    var body: some View {
        if on { Label(title, systemImage: "checkmark") } else { Text(title) }
    }
}

/// The choices of a delay, from −1 s to +1 s.
struct DelayChoices: View {
    let value: Double
    let set: (Double) -> Void

    var body: some View {
        ForEach([-1.0, -0.5, -0.25, -0.1, 0, 0.1, 0.25, 0.5, 1.0], id: \.self) { secs in
            Button { set(secs) } label: {
                Checked(title: PlayerView.delay(secs), on: abs(value - secs) < 0.001)
            }
        }
    }
}

/// The tools under the seek bar: subtitles, audio, episodes, the small
/// player, AirPlay, Picture in Picture, bookmarks, SyncPlay, settings. A view
/// of its own that reads no time, so its menus stay open while the film
/// plays (2026-11-14: they closed on their own as the player redrew).
struct PlayerTools: View {
    @Environment(AppModel.self) private var app
    let model: PlayerModel
    /// The word under each tool, for the first few plays (`ToolWords`).
    @State private var words = ToolWords.shown()

    var body: some View {
        HStack(spacing: words ? 4 : 8) {
            Menu {
                TrackEntry(name: TrackName(name: "Off", detail: nil), on: !model.subtitleTracks.contains(where: \.selected)) {
                    model.selectSubtitle(nil)
                }
                ForEach(model.subtitleTracks) { track in
                    TrackEntry(name: model.name(of: track), on: track.selected) { model.selectSubtitle(track) }
                }
                if !model.serverSubtitles.isEmpty {
                    Section("From the server") {
                        ForEach(model.serverSubtitles, id: \.index) { subtitle in
                            let name = model.name(of: subtitle)
                            TrackEntry(name: model.fetchingSubtitle == subtitle.index ? TrackName(name: name.name, detail: "Loading…") : name, on: false) {
                                model.releaseControls(); Task { await model.selectServerSubtitle(subtitle) }
                            }
                        }
                    }
                }
                if app.prefs.canManageSubtitles {
                    Divider()
                    Button("Search for subtitles…", systemImage: "magnifyingglass") { model.showSubtitleSearch = true }
                }
            } label: {
                Label("Subtitles", systemImage: "captions.bubble").toolTarget()
            }
            .simultaneousGesture(TapGesture().onEnded { model.holdControls() })
            .accessibilityIdentifier("player.subtitles")

            if model.audioTracks.count > 1 {
                Menu {
                    ForEach(model.audioTracks) { track in
                        TrackEntry(name: model.name(of: track), on: track.selected) { model.selectAudio(track) }
                    }
                } label: {
                    Label("Audio", systemImage: "speaker.wave.2").toolTarget()
                }
                .simultaneousGesture(TapGesture().onEnded { model.holdControls() })
                .accessibilityIdentifier("player.audio")
            }
            if !model.seasonEpisodes.isEmpty {
                Button("Episodes", systemImage: "rectangle.stack") { model.showEpisodes = true }
                    .accessibilityIdentifier("player.episodes")
            }

            Spacer(minLength: 8)

            // The small player over the pages (the Mac's small window).
            Button("Small player", systemImage: "rectangle.inset.bottomright.filled") { app.minimizePlayer() }
                .accessibilityIdentifier("player.minimize")
            // Play on another Jellyfin app or a Chromecast: the sheet of the
            // account menu; a choice hands the film over at this place.
            Button("Play on", systemImage: "tv") { model.holdControls(); model.showPlayOn = true }
                .accessibilityIdentifier("player.play-on")
            // AirPlay: the system's route button. MPVUI prepares the stream
            // for the receiver (the file as it is, or packaged on the phone).
            if let engine = model.engine {
                VStack(spacing: 2) {
                    MPVRoutePicker(player: engine)
                        .frame(width: 44, height: words ? 28 : 44)
                        .accessibilityLabel("AirPlay")
                        .accessibilityIdentifier("player.airplay")
                    if words {
                        Text("AirPlay").font(.caption2).foregroundStyle(.secondary).lineLimit(1).fixedSize()
                            .accessibilityHidden(true)
                    }
                }
            }
            if let engine = model.engine, engine.pictureInPicture.isSupported {
                Button("Picture in Picture", systemImage: engine.pictureInPicture.isActive ? "pip.exit" : "pip.enter") {
                    engine.pictureInPicture.toggle()
                }
                .disabled(!engine.pictureInPicture.isPossible)
                .accessibilityIdentifier("player.pip")
            }
            if app.enhanced?.bookmarks == true {
                Button("Bookmarks", systemImage: "bookmark") { model.showBookmarks = true }
                    .accessibilityIdentifier("player.bookmarks")
            }
            if SyncBridge.shared.group != nil || app.syncAllowed {
                Button("SyncPlay", systemImage: SyncBridge.shared.group != nil ? "person.2.fill" : "person.2") { model.showSyncPlay = true }
                    .accessibilityIdentifier("player.syncplay")
            }
            settings
        }
        .labelStyle(ToolLabelStyle(words: words))
        .font(.title3)
        .buttonStyle(ToolButtonStyle())
    }

    /// The quality chosen, in the words of its row.
    private var qualityName: String {
        guard let chosen = model.quality, let rung = model.info?.rungs.first(where: { $0.bitrate == chosen }) else { return "Auto" }
        return rung.label
    }

    private var settings: some View {
        Menu {
            // Quality first, as a row with its choice, like Speed: its
            // list of rates would push every other entry down.
            Menu {
                Button { model.releaseControls(); Task { await model.setQuality(nil) } } label: {
                    Checked(title: "Auto · " + (model.info?.transcoding == true ? "Lowered by the server" : "The file as it is"), on: model.quality == nil)
                }
                ForEach(model.info?.rungs ?? [], id: \.bitrate) { rung in
                    Button { model.releaseControls(); Task { await model.setQuality(rung.bitrate) } } label: {
                        Checked(title: "\(rung.label) · \(rung.height)", on: model.quality == rung.bitrate)
                    }
                }
            } label: {
                Label("Quality · \(qualityName)", systemImage: "sparkles.tv")
            }
            .accessibilityIdentifier("player.quality")
            Menu {
                ForEach([0.5, 0.75, 1, 1.25, 1.5, 2], id: \.self) { speed in
                    Button { model.setSpeed(speed); model.releaseControls() } label: {
                        Checked(title: speed == 1 ? "Normal" : "\(speed.formatted())×", on: model.speed == speed)
                    }
                }
            } label: {
                Label("Speed · \(model.speed == 1 ? "Normal" : "\(model.speed.formatted())×")", systemImage: "gauge.with.dots.needle.67percent")
            }
            Menu {
                DelayChoices(value: model.audioDelay) { model.setAudioDelay($0); model.releaseControls() }
            } label: {
                Label("Audio delay · \(PlayerView.delay(model.audioDelay))", systemImage: "waveform")
            }
            Menu {
                DelayChoices(value: model.subtitleDelay) {
                    model.setSubtitleDelay($0)
                    model.releaseControls()
                    app.say("Subtitles \(PlayerView.delay($0))")
                }
            } label: {
                Label("Subtitle timing · \(PlayerView.delay(model.subtitleDelay))", systemImage: "captions.bubble")
            }
            Button("Playback info", systemImage: "info.circle") { model.showInfo.toggle(); model.releaseControls() }
                .accessibilityIdentifier("player.info")
        } label: {
            Label("Settings", systemImage: "gearshape").toolTarget()
        }
        .simultaneousGesture(TapGesture().onEnded { model.holdControls() })
        .accessibilityIdentifier("player.settings")
    }
}

/// A tool's icon, with its word under it while `words` is on; the icon
/// alone keeps the word as its accessibility label.
struct ToolLabelStyle: LabelStyle {
    let words: Bool

    func makeBody(configuration: Configuration) -> some View {
        if words {
            VStack(spacing: 2) {
                configuration.icon.frame(height: 28)
                configuration.title.font(.caption2).foregroundStyle(.secondary).lineLimit(1).fixedSize()
            }
        } else {
            Label(configuration).labelStyle(.iconOnly)
        }
    }
}

/// The words under the player's tools show for the first three plays on
/// this device, then the icons stand alone (their names stay for
/// VoiceOver). `BLOOM_TOOL_WORDS=1` or `0` sets it for a test.
enum ToolWords {
    private static let key = "player.toolWordsPlays"
    static let plays = 3

    static func shown() -> Bool {
        switch ProcessInfo.processInfo.environment["BLOOM_TOOL_WORDS"] {
        case "1": return true
        case "0": return false
        default: return UserDefaults.standard.integer(forKey: key) < plays
        }
    }

    /// A player opened.
    static func opened() {
        UserDefaults.standard.set(UserDefaults.standard.integer(forKey: key) + 1, forKey: key)
    }
}

/// A tool of the bar: 44 points to touch whatever its icon's size (Apple's
/// smallest target).
struct ToolButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .toolTarget()
            .opacity(configuration.isPressed ? 0.5 : 1)
    }
}

extension View {
    func toolTarget() -> some View {
        frame(minWidth: 44, minHeight: 44).contentShape(.rect)
    }
}
