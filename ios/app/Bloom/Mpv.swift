import Foundation
import MPVUI

/// mpv through MPVUI, with the AVFoundation sample-buffer output: the
/// picture Picture in Picture and AirPlay take (spike S2, spec §17). The
/// player model keeps the interface it had over MPVKit; this maps it onto
/// MPVUI's `MPVPlayer`, which runs on the main actor.
///
/// Hardware decoding with subtitles drawn by mpv stops MPVUI 0.10.0 ("no
/// color space for native CI overlay", 2026-11-14 on the iPhone Air):
/// decoding goes to software while a subtitle track is selected, and back
/// to hardware when none is. Should the error come anyway, the item loads
/// again at its place with software decoding.
final class Mpv {
    struct State: Sendable {
        var position: Double = 0
        var duration: Double = 0
        var paused = false
        var buffering = true
        var tracks: [Track] = []
        /// Why mpv stopped, when it did.
        var failure: String?
    }

    /// One of mpv's tracks.
    struct Track: Sendable, Hashable, Identifiable {
        enum Kind: String, Sendable { case video, audio, sub }
        let id: Int
        let kind: Kind
        let title: String
        let selected: Bool
        let external: Bool
        /// The title the file or `sub-add` gave, as it is.
        var name: String = ""
    }

    /// What SyncPlay listens to.
    enum Event: Sendable {
        case position(Double, paused: Bool, duration: Double)
        /// A seek or a load is done and the player can play.
        case restart
        case loadFailed
        case stalled(Bool)
    }

    let player: MPVPlayer
    private var state = State()
    private let onChange: (State) -> Void
    private let onEnd: () -> Void
    private var listener: ((Event) -> Void)?
    /// A pause for data began (true) or ended (false) while playing: Auto
    /// counts these (the player model; SyncPlay has the listener).
    var onStall: ((Bool) -> Void)?
    /// mpv loads the item again by itself (the software fallback): the
    /// files added with `sub-add` must go back.
    var onReload: (() -> Void)?
    private var stalled = false
    private var tasks: [Task<Void, Never>] = []
    /// The source of the item, to load it again after a failure of the
    /// hardware path.
    private var source: URL?
    private var softwareOnly = false
    private var recovered = false
    private var hwdec = "auto-safe"

    init(muted: Bool, onChange: @escaping (State) -> Void, onEnd: @escaping () -> Void) {
        self.onChange = onChange
        self.onEnd = onEnd
        var options: [String: String] = [
            "deinterlace": "auto",
            "sub-auto": "no",
            "audio-display": "no",
            // The app draws its own controls: no on-screen text from mpv.
            "osd-level": "0",
            "demuxer-max-bytes": "64MiB",
            "demuxer-max-back-bytes": "16MiB",
            "tls-verify": "yes",
        ]
        if let roots = Bundle.main.path(forResource: "cacert", ofType: "pem") {
            options["tls-ca-file"] = roots
        }
        player = MPVPlayer(configuration: .init(additionalOptions: options, videoOutput: .sampleBuffer))
        player.pictureInPicture.allowsAutomaticStartFromInline = true
        if muted { player.setMuted(true) }
        watch()
    }

    // MARK: Events of MPVUI

    private func watch() {
        let events = player.makeEventStream()
        let timing = player.makeTimingStream()
        tasks.append(Task { [weak self] in
            for await event in events {
                guard let self else { return }
                self.handle(event.change)
            }
        })
        tasks.append(Task { [weak self] in
            for await tick in timing {
                guard let self else { return }
                self.state.position = tick.position.seconds
                if tick.duration > .zero { self.state.duration = tick.duration.seconds }
                self.onChange(self.state)
                self.listener?(.position(self.state.position, paused: self.state.paused, duration: self.state.duration))
            }
        })
    }

    private func handle(_ change: MPVPlaybackEvent.Change) {
        switch change {
        case .stateChanged(let s): NSLog("MPV: state %@", String(describing: s))
        case .error(let e, let fatal): NSLog("MPV: error %@ fatal %d", String(describing: e), fatal ? 1 : 0)
        case .readinessChanged(let r): NSLog("MPV: ready %d", r ? 1 : 0)
        default: break
        }
        switch change {
        case .stateChanged(let playback):
            switch playback {
            case .ended:
                onEnd()
            case .buffering:
                state.buffering = true
                listener?(.stalled(true))
                if !state.paused, !stalled {
                    stalled = true
                    onStall?(true)
                }
            case .loading, .seeking:
                state.buffering = true
                endStall()
            case .playing, .paused, .ready:
                if state.buffering, playback == .playing { listener?(.stalled(false)) }
                state.buffering = false
                endStall()
            case .failed(let error):
                if case .nativeVideoOutputUnavailable = error, !recovered {
                    recoverInSoftware()
                } else {
                    state.failure = String(describing: error)
                    listener?(.loadFailed)
                }
            default:
                break
            }
        case .pauseChanged(let paused):
            state.paused = paused
            // A pause of the user ends the wait for data that counts.
            if paused { endStall() }
        case .readinessChanged(let ready):
            if ready {
                state.buffering = false
                listener?(.restart)
            }
        case .mediaInformationChanged(let info):
            state.tracks = info.tracks.map(Self.track)
            followSubtitles()
        case .seekCompleted:
            listener?(.restart)
        case .error(let error, let fatal):
            if fatal, case .nativeVideoOutputUnavailable = error, !recovered { recoverInSoftware() }
        default:
            return
        }
        onChange(state)
    }

    private func endStall() {
        guard stalled else { return }
        stalled = false
        onStall?(false)
    }

    private static func track(_ track: MPVMediaTrack) -> Track {
        let kind: Track.Kind = switch track.type {
        case .video: .video
        case .audio: .audio
        case .subtitle: .sub
        }
        let language = track.language.flatMap { Locale.current.localizedString(forLanguageCode: $0) }
        let parts = [track.title, language, track.codec?.uppercased()].compactMap { $0 }
        var seen = Set<String>()
        let title = parts.filter { seen.insert($0.lowercased()).inserted }.joined(separator: " · ")
        return Track(id: track.mpvID, kind: kind, title: title.isEmpty ? "Track \(track.mpvID)" : title,
                     selected: track.isSelected, external: track.isExternal, name: track.title ?? "")
    }

    /// Software decoding while mpv draws a subtitle (see the type's note).
    private func followSubtitles() {
        let drawing = state.tracks.contains { $0.kind == .sub && $0.selected }
        let wanted = drawing || softwareOnly ? "no" : "auto-safe"
        if wanted != hwdec {
            hwdec = wanted
            player.setProperty("hwdec", to: wanted)
        }
    }

    /// The hardware path failed under a subtitle: the item again, at its
    /// place, decoded in software. Once per item.
    private func recoverInSoftware() {
        guard let source else { return }
        onReload?()
        recovered = true
        softwareOnly = true
        hwdec = "no"
        player.setProperty("hwdec", to: "no")
        let at = state.position
        // Not ready until the reload is: the subtitle files wait for it.
        state.duration = 0
        state.buffering = true
        NSLog("PLAYER: the native output failed; again in software at %.1f", at)
        player.load(source, playAutomatically: !state.paused, startTime: .seconds(at))
    }

    // MARK: The interface of the player model

    func listen(_ listener: ((Event) -> Void)?) {
        self.listener = listener
    }

    func load(url: String, header: String, start: Double, paused: Bool = false) {
        let target = url.hasPrefix("/") ? URL(fileURLWithPath: url) : (URL(string: url) ?? URL(fileURLWithPath: url))
        source = target
        recovered = false
        // Software decoding after a failure is for that item alone.
        softwareOnly = false
        state.failure = nil
        state.position = start
        state.duration = 0
        state.buffering = true
        player.setProperty("http-header-fields", to: header)
        player.load(target, playAutomatically: !paused, startTime: .seconds(start))
    }

    /// A property by its text value. MPVUI keeps pause, speed and mute for
    /// its own calls.
    func set(property name: String, _ value: String) {
        switch name {
        case "pause": setPaused(value == "yes")
        case "speed": if let rate = Double(value) { player.setPlaybackRate(rate) }
        case "mute": player.setMuted(value == "yes")
        default: player.setProperty(name, to: value)
        }
    }

    /// Adds a subtitle file mpv reads from this device and selects it.
    func addSubtitle(path: String, title: String) {
        player.executeCommand("sub-add", arguments: [path, "select", title])
    }

    func setPaused(_ paused: Bool) {
        if paused { player.pause() } else { player.play() }
    }

    func seek(to secs: Double) {
        player.seek(to: .seconds(secs))
    }

    /// A seek to the frame (SyncPlay); `restart` comes when it is done.
    func seekExact(to secs: Double) {
        Task { [weak self] in
            guard let self else { return }
            _ = try? await self.player.seekAndWait(to: .seconds(secs))
        }
    }

    func skip(_ secs: Double) {
        player.seek(by: .seconds(secs))
    }

    func stop() {
        tasks.forEach { $0.cancel() }
        tasks = []
        listener = nil
        onStall = nil
        player.stop()
    }

    /// The lines of the playback info card.
    func infoLines() -> [(String, String)] {
        let info = player.mediaInformation
        var lines: [(String, String)] = []
        if let size = info.dimensions {
            lines.append(("Video", [info.videoCodec, "\(size.width)×\(size.height)"].compactMap { $0 }.joined(separator: " · ")))
        }
        lines.append(("Decoder", info.hardwareDecoder.map { "Hardware (\($0))" } ?? "Software"))
        if let audio = info.audioCodec { lines.append(("Audio", audio)) }
        if let container = info.container { lines.append(("Container", String(container.split(separator: ",").first ?? ""))) }
        let ahead = player.bufferStatus.bufferedDurationAhead.seconds
        if ahead > 0 { lines.append(("Buffered", String(format: "%.0f s", ahead))) }
        lines.append(("Output", "Sample buffer\(player.pictureInPicture.isActive ? " · PiP" : "")"))
        return lines
    }
}

extension Duration {
    var seconds: Double { Double(components.seconds) + Double(components.attoseconds) / 1e18 }
}
