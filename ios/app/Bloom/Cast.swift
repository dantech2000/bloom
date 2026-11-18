import BloomCore
import Network
import SwiftUI
import UIKit

/// Whether iOS lets the app use the local network (spec §8.4). Without
/// the permission the Bonjour browse finds nothing and says nothing, so a
/// browse of the system's own (`NWBrowser`) watches for the refusal while
/// the Play on sheet is open. The Mac has no such case.
/// `BLOOM_LOCAL_NETWORK_DENIED=1` shows the refusal for a test: the
/// simulator never refuses.
@Observable
final class LocalNetworkCheck {
    private(set) var denied = false
    @ObservationIgnored private var browser: NWBrowser?
    @ObservationIgnored private var reading: Task<Void, Never>?

    /// What the browser tells, from its queue to the main actor.
    private enum News: Sendable {
        case state(NWBrowser.State)
        case found
    }

    /// `kDNSServiceErr_PolicyDenied` of dns_sd.h: the user said no, or
    /// was not asked yet.
    private static let policyDenied: DNSServiceErrorType = -65570

    func start() {
        guard browser == nil else { return }
        if ProcessInfo.processInfo.environment["BLOOM_LOCAL_NETWORK_DENIED"] == "1" {
            denied = true
            return
        }
        let browser = NWBrowser(for: .bonjour(type: "_googlecast._tcp", domain: nil), using: NWParameters())
        let (news, tell) = AsyncStream.makeStream(of: News.self)
        browser.stateUpdateHandler = { state in tell.yield(.state(state)) }
        // A device found proves the permission.
        browser.browseResultsChangedHandler = { results, _ in
            if !results.isEmpty { tell.yield(.found) }
        }
        reading = Task { [weak self] in
            for await item in news {
                switch item {
                case .state(let state): self?.changed(state)
                case .found: self?.denied = false
                }
            }
        }
        browser.start(queue: .global(qos: .utility))
        self.browser = browser
    }

    func stop() {
        browser?.cancel()
        browser = nil
        reading?.cancel()
        reading = nil
    }

    private func changed(_ state: NWBrowser.State) {
        switch state {
        case .waiting(let error), .failed(let error):
            if case .dns(let code) = error, code == Self.policyDenied { denied = true }
        case .ready:
            denied = false
        default:
            break
        }
    }
}

/// The refusal in plain words, with the way to the switch.
struct LocalNetworkDenied: View {
    @Environment(\.openURL) private var openURL

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Bloom may not use the local network", systemImage: "wifi.exclamationmark")
                .font(.subheadline.weight(.medium))
            Text("Without it, no Chromecast shows here. Allow it in Settings › Privacy & Security › Local Network.")
                .font(.footnote).foregroundStyle(.secondary)
            Button("Open Settings") {
                if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
            }
            .font(.footnote.weight(.medium))
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
    }
}

/// Keeps a cast going while the app is away (spec §8.4). The phone plays
/// no audio during a cast, so iOS suspends the app and the socket stops.
/// When the app goes to the background, it asks for the background time
/// iOS gives (about 30 s): a short trip to another app costs nothing. At
/// the return, `onReturn` asks the device for its state again; the
/// session connects again by itself when the socket died meanwhile.
final class CastLife {
    private var watches: [Task<Void, Never>] = []
    private var task = UIBackgroundTaskIdentifier.invalid
    private let onReturn: () -> Void

    init(onReturn: @escaping () -> Void) {
        self.onReturn = onReturn
        watches = [
            onAppNotification(UIApplication.didEnterBackgroundNotification) { [weak self] in self?.away() },
            onAppNotification(UIApplication.willEnterForegroundNotification) { [weak self] in self?.returned() },
        ]
    }

    /// Stops the watch; the cast ended.
    func end() {
        watches.forEach { $0.cancel() }
        watches = []
        endTask()
    }

    private func returned() {
        endTask()
        onReturn()
    }

    private func away() {
        // A test of the return: the fake device drops its socket, as a
        // socket that died while the phone slept.
        if ProcessInfo.processInfo.environment["BLOOM_CAST_DROP_AWAY"] == "1" {
            castMockDropConnections()
        }
        guard task == .invalid else { return }
        task = UIApplication.shared.beginBackgroundTask(withName: "cast") { [weak self] in
            MainActor.assumeIsolated { self?.endTask() }
        }
    }

    private func endTask() {
        guard task != .invalid else { return }
        UIApplication.shared.endBackgroundTask(task)
        task = .invalid
    }
}

/// The Chromecast's controls in the Play on sheet (the Mac's cast panel,
/// inventory §4.1): what plays, the seek bar, the transport, the volume,
/// and the audio and subtitle tracks of the item this app loaded.
struct CastControls: View {
    @Environment(AppModel.self) private var model
    /// The place under the finger while the seek bar is dragged.
    @State private var seeking: Double?
    @State private var volume: Double?
    @State private var streams: [StreamOption] = []
    /// Asked only when the device does not answer: a refusal of the local
    /// network is the likely cause on a phone, and says so (spec §8.4).
    @State private var network = LocalNetworkCheck()

    var body: some View {
        let status = model.castStatus
        let cast = model.chromecast
        let unreachable = status.map { !$0.connected && $0.error != nil } ?? false
        VStack(alignment: .leading, spacing: 4) {
            Text(headline(status)).font(.headline)
            if unreachable, network.denied {
                // Never "offline": the device may play on; this app may not
                // reach it.
                LocalNetworkDenied()
            } else if let error = status?.error {
                Text(status?.connected == true ? error : "Not connected: \(error). Bloom tries again.")
                    .font(.footnote).foregroundStyle(Color.fail)
            }
        }
        .task(id: unreachable) {
            if unreachable { network.start() } else { network.stop() }
        }
        .onDisappear { network.stop() }
        // The tracks of the item this app loaded, read once per item.
        .task(id: status?.itemId) {
            guard let id = status?.itemId else {
                streams = []
                return
            }
            streams = (try? await model.server?.streams(itemId: id)) ?? []
        }
        if let status, status.loaded {
            let duration = max(status.durationSecs ?? 0, status.positionSecs, 1)
            VStack(spacing: 2) {
                Slider(value: Binding(get: { seeking ?? status.positionSecs }, set: { seeking = $0 }), in: 0...duration) { editing in
                    if !editing, let secs = seeking {
                        cast?.seek(secs: secs)
                        seeking = nil
                    }
                }
                .accessibilityLabel("Position")
                .accessibilityValue(Words.clock(seeking ?? status.positionSecs))
                HStack {
                    Text(Words.clock(seeking ?? status.positionSecs))
                    Spacer()
                    Text(status.durationSecs.map(Words.clock) ?? "")
                }
                .font(.caption).monospacedDigit().foregroundStyle(.secondary)
            }
            HStack(spacing: 28) {
                Button("Back 10 seconds", systemImage: "gobackward.10") {
                    cast?.seek(secs: max(0, status.positionSecs - 10))
                }
                Button(status.paused ? "Play" : "Pause", systemImage: status.paused ? "play.fill" : "pause.fill") {
                    if status.paused { cast?.play() } else { cast?.pause() }
                }
                Button("Forward 30 seconds", systemImage: "goforward.30") {
                    cast?.seek(secs: status.positionSecs + 30)
                }
                Button("Stop", systemImage: "stop.fill") { cast?.stop() }
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.borderless)
            .font(.title3)
            .frame(maxWidth: .infinity)
        }
        if let status, status.connected {
            HStack(spacing: 12) {
                Button(status.muted ? "Unmute" : "Mute", systemImage: status.muted ? "speaker.slash.fill" : "speaker.wave.2.fill") {
                    cast?.setMuted(muted: !status.muted)
                }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                Slider(value: Binding(get: { volume ?? status.volume }, set: { volume = $0 }), in: 0...1) { editing in
                    if !editing, let level = volume {
                        cast?.setVolume(level: level)
                        volume = nil
                    }
                }
                .accessibilityLabel("Volume")
                .accessibilityValue("\(Int(((volume ?? status.volume) * 100).rounded())) percent")
            }
        }
        if let status, status.canTracks, !streams.isEmpty {
            tracks(status)
        }
    }

    private func headline(_ status: CastStatus?) -> String {
        guard let status, status.connected else { return "Connecting…" }
        return status.title ?? status.appName ?? "Connected"
    }

    @ViewBuilder private func tracks(_ status: CastStatus) -> some View {
        let audio = streams.filter { $0.kind == .audio }
        let subtitles = streams.filter { $0.kind == .subtitle }
        if audio.count > 1 {
            Picker("Audio", selection: Binding(
                get: { status.audioIndex ?? audio.first { $0.isDefault }?.index ?? audio[0].index },
                set: { model.chromecast?.setAudio(index: $0) }
            )) {
                ForEach(audio, id: \.index) { Text($0.title).tag($0.index) }
            }
            .pickerStyle(.menu)
        }
        if !subtitles.isEmpty {
            Picker("Subtitles", selection: Binding(
                get: { status.subtitleIndex ?? subtitles.first { $0.isDefault }?.index ?? -1 },
                set: { model.chromecast?.setSubtitle(index: $0) }
            )) {
                Text("Off").tag(Int64(-1))
                ForEach(subtitles, id: \.index) { Text($0.title).tag($0.index) }
            }
            .pickerStyle(.menu)
        }
        // The Default Media Receiver plays one stream with one audio track
        // and the subtitle in the picture: a change starts it again.
        if !status.jellyfinApp, audio.count > 1 || !subtitles.isEmpty {
            Text("A change of track starts the stream again on this device.")
                .font(.footnote).foregroundStyle(.secondary)
        }
    }
}

/// AirPlay that fails comes back to this phone with a notice (the Mac's
/// AirPlay hardening, inventory §4.5). MPVUI's external playback already
/// keeps the play or pause asked while it prepares, coalesces the seeks,
/// hands the receiver the player's volume and mute, and gives the picture
/// back to mpv when the route fails; it tells only through `lastError`,
/// which nothing shows once the AirPlay view is gone. This watches the
/// player that plays and says it.
final class AirPlayNotice {
    static let shared = AirPlayNotice()
    private weak var app: AppModel?
    /// The external playback was on (preparing or playing) at the last look.
    private var wasOn = false
    /// The error there was while it was on: a failed seek leaves one that
    /// a later return to the phone by the user must not tell as a failure.
    private var errorWhileOn: String?

    func watch(_ app: AppModel) {
        guard self.app == nil else { return }
        self.app = app
        look()
    }

    private func look() {
        guard let app else { return }
        let (on, error) = withObservationTracking {
            let external = app.player?.engine?.externalPlayback
            return ((external?.isPreparing ?? false) || (external?.isActive ?? false),
                    external?.lastError.map(\.message))
        } onChange: { [weak self] in
            Task { @MainActor in self?.look() }
        }
        if wasOn, !on, let error, error != errorWhileOn {
            NSLog("AIRPLAY: the route failed: %@", error)
            app.say("AirPlay stopped: \(Self.reason(error)) The video plays on this iPhone.")
        }
        if on, !wasOn { errorWhileOn = nil }
        if on { errorWhileOn = error }
        wasOn = on
    }

    /// MPVUI's message less its prefix, as a sentence.
    private static func reason(_ message: String) -> String {
        var text = message
        if text.hasPrefix("External playback: ") { text.removeFirst("External playback: ".count) }
        text = text.trimmingCharacters(in: .whitespaces)
        if !text.hasSuffix(".") { text += "." }
        return text
    }
}
