import BloomCore
import SwiftUI

/// The account sheet (spec §6.1): profiles, the server list, Settings,
/// Sign out.
struct AccountSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var confirmSignOut = false
    @State private var authorize = false

    var body: some View {
        NavigationStack {
            List {
                ForEach(Dictionary(grouping: model.accounts, by: \.serverName).sorted(by: { $0.key < $1.key }), id: \.key) { server, accounts in
                    Section(server) {
                        ForEach(accounts, id: \.self) { account in
                            Button {
                                dismiss()
                                Task { await model.open(account) }
                            } label: {
                                HStack {
                                    Label(account.userName, systemImage: "person.crop.circle.fill")
                                    Spacer()
                                    if account == model.active { Image(systemName: "checkmark") }
                                }
                            }
                        }
                    }
                }
                Section {
                    Button("Servers & profiles…", systemImage: "server.rack") {
                        dismiss()
                        model.showServers()
                    }
                    Button("Play on", systemImage: "tv") {
                        dismiss()
                        model.showPlayOn = true
                    }
                    if model.syncAllowed {
                        Button("SyncPlay", systemImage: "person.2") {
                            dismiss()
                            model.showSyncPlay = true
                        }
                    }
                    Button("Quick Connect", systemImage: "qrcode") { authorize = true }
                    Button("Settings", systemImage: "gearshape") {
                        dismiss()
                        model.settings = true
                    }
                    if model.prefs.isAdmin {
                        Button("Dashboard", systemImage: "gauge.with.needle") {
                            dismiss()
                            model.showDashboard = true
                        }
                        .accessibilityIdentifier("account.dashboard")
                    }
                }
                Section {
                    Button("Sign Out", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) {
                        confirmSignOut = true
                    }
                }
            }
            .navigationTitle("Account")
            .navigationBarTitleDisplayMode(.inline)
            .sheet(isPresented: $authorize) { AuthorizeSheet().bloomTheme().toastHost().environment(model) }
            .confirmationDialog("Sign out of \(model.active?.serverName ?? "the server")?", isPresented: $confirmSignOut, titleVisibility: .visible) {
                Button("Sign Out", role: .destructive) {
                    dismiss()
                    Task { await model.signOut() }
                }
            } message: {
                Text("Ends this session on the server and forgets it on this device.")
            }
        }
        .presentationDetents([.medium, .large])
    }
}

/// Settings as a large sheet with its own stack (spec §6.1, §8.6).
struct SettingsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section {
                    NavigationLink { PlaybackSettings() } label: { Label("Playback", systemImage: "play.rectangle") }
                    NavigationLink { AudioSettings() } label: { Label("Audio", systemImage: "speaker.wave.2") }
                    NavigationLink { SubtitleSettings() } label: { Label("Subtitles", systemImage: "captions.bubble") }
                    NavigationLink { HomeSettings() } label: { Label("Home", systemImage: "house") }
                    NavigationLink { DownloadSettings() } label: { Label("Downloads", systemImage: "arrow.down.circle") }
                    if model.enhanced?.hiddenContent == true {
                        NavigationLink { HiddenSettings() } label: { Label("Hidden content", systemImage: "eye.slash") }
                    }
                    if model.enhancedServer != nil {
                        NavigationLink { EnhancedSettingsPage() } label: { Label("Jellyfin Enhanced", systemImage: "sparkles") }
                    }
                    NavigationLink { AccountSettings() } label: { Label("Account", systemImage: "person.crop.circle") }
                }
                Section("About") {
                    LabeledContent("Version", value: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "—")
                    Text("Jellyfin client. Not an official Jellyfin app. Playback by mpv and FFmpeg (LGPL).")
                        .font(.footnote).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Settings")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        .task { await model.prefs.ensureLoaded(model.server) }
    }
}

/// A section that reads the server's settings: a spinner, or why they
/// could not load, until they are in.
private struct ServerSection<Content: View>: View {
    @Environment(AppModel.self) private var model
    let title: String
    /// An Edit button for a list of rows to move or remove.
    var editable = false
    @ViewBuilder let content: () -> Content

    var body: some View {
        Group {
            switch model.prefs.load {
            case .loaded:
                List { content() }
                    .toolbar { if editable { EditButton() } }
            case .failed(let why):
                ContentUnavailableView {
                    Label("The settings could not load", systemImage: "wifi.exclamationmark")
                } description: {
                    Text(why)
                } actions: {
                    Button("Try Again") { Task { await model.prefs.ensureLoaded(model.server) } }
                }
            default:
                ProgressView()
            }
        }
        .navigationTitle(title)
        .task { await model.prefs.ensureLoaded(model.server) }
    }
}

/// A choice of the server's languages, "Any language" first.
private struct LanguagePicker: View {
    @Environment(AppModel.self) private var model
    let title: String
    let key: String

    var body: some View {
        let prefs = model.prefs
        Picker(title, selection: Binding(get: { prefs.text(key) }, set: { prefs.set(key, $0) })) {
            Text("Any language").tag("")
            ForEach(prefs.languages, id: \.code) { language in
                Text(language.name).tag(language.code)
            }
            // A code the list lacks still shows as chosen.
            let current = prefs.text(key)
            if !current.isEmpty, !prefs.languages.contains(where: { $0.code == current }) {
                Text(current).tag(current)
            }
        }
    }
}

private struct ServerToggle: View {
    @Environment(AppModel.self) private var model
    let title: String
    let key: String
    let fallback: Bool

    var body: some View {
        let prefs = model.prefs
        Toggle(title, isOn: Binding(get: { prefs.bool(key, default: fallback) }, set: { prefs.set(key, $0) }))
    }
}

struct PlaybackSettings: View {
    @Environment(AppModel.self) private var model
    @Bindable private var local = Preferences.shared
    private static let skipLengths = [5, 10, 15, 20, 25, 30]

    var body: some View {
        ServerSection(title: "Playback") {
            let prefs = model.prefs
            Section {
                Picker("Maximum quality", selection: $local.maxBitrate) {
                    Text("Auto").tag(UInt64(0))
                    ForEach(qualityLadder(), id: \.bitrate) { rung in
                        Text("\(rung.label) · \(rung.height)").tag(rung.bitrate)
                    }
                }
                Picker("Maximum quality on cellular", selection: $local.cellularMaxBitrate) {
                    Text("Original").tag(UInt64(0))
                    ForEach(qualityLadder(), id: \.bitrate) { rung in
                        Text("\(rung.label) · \(rung.height)").tag(rung.bitrate)
                    }
                }
                .accessibilityIdentifier("settings.cellularQuality")
                Toggle("Lower the quality on a slow connection", isOn: $local.lowerOnSlowLink)
                    .onChange(of: local.lowerOnSlowLink) { if local.lowerOnSlowLink { model.measureLink() } }
            } header: {
                Text("Quality")
            } footer: {
                Text("Auto plays the file as it is when the connection carries it; on a slower one, the highest quality that fits. On cellular, the lower of the two maximums applies.")
            }
            Section("Next episode") {
                ServerToggle(title: "Play the next episode automatically", key: "EnableNextEpisodeAutoPlay", fallback: true)
                Toggle("Show the Up next card", isOn: Binding(get: { prefs.upNextCard },
                                                             set: { prefs.setCustom(["enableNextVideoInfoOverlay": $0 ? "true" : "false"]) }))
            }
            Section("Skip buttons") {
                ForEach([("Skip back length", "skipBackLength"), ("Skip forward length", "skipForwardLength")], id: \.1) { title, key in
                    Picker(title, selection: Binding(get: { Int(prefs.skipSecs(key)) },
                                                     set: { prefs.setCustom([key: String($0 * 1000)]) })) {
                        ForEach(Self.skipLengths, id: \.self) { Text("\($0) seconds").tag($0) }
                    }
                }
            }
            Section("Segments") {
                Toggle("Skip intros automatically", isOn: $local.autoSkipIntro)
                Toggle("Skip credits automatically", isOn: $local.autoSkipCredits)
            }
            Section {
                Toggle("Allow remote control", isOn: $local.allowRemoteControl)
                    .onChange(of: local.allowRemoteControl) { model.postCapabilities() }
            } header: {
                Text("Remote control")
            } footer: {
                Text("Your other Jellyfin apps can play on this device and control it.")
            }
        }
    }
}

struct AudioSettings: View {
    var body: some View {
        ServerSection(title: "Audio") {
            Section {
                LanguagePicker(title: "Audio language", key: "AudioLanguagePreference")
                ServerToggle(title: "Play default audio track regardless of language", key: "PlayDefaultAudioTrack", fallback: false)
                ServerToggle(title: "Remember audio selections", key: "RememberAudioSelections", fallback: true)
            } footer: {
                Text("Every Jellyfin app of yours uses these settings.")
            }
        }
    }
}

struct SubtitleSettings: View {
    @Environment(AppModel.self) private var model
    @Bindable private var look = Preferences.shared
    static let modes: [(key: String, name: String, about: String)] = [
        ("Default", "Default", "Follows the default and forced marks of the file. Your language counts when the file has more than one choice."),
        ("Smart", "Smart", "Shows subtitles in your language when the audio is in another language."),
        ("OnlyForced", "Only forced", "Shows only the subtitles the file marks as forced."),
        ("Always", "Always", "Shows subtitles in your language, whatever the audio language is."),
        ("None", "None", "Shows no subtitles at the start. You can turn them on in the player."),
    ]

    /// Where a value left at Default comes from (spec §7.3).
    static func lookSources(enhanced: Bool) -> String {
        let system = "the caption style of Settings › Accessibility › Subtitles & Captioning"
        return enhanced
            ? "A value left at Default comes from \(system), then from the Jellyfin Enhanced subtitle style. Styled (ASS) and image subtitles keep their own colours."
            : "A value left at Default comes from \(system). Styled (ASS) and image subtitles keep their own colours."
    }

    var body: some View {
        ServerSection(title: "Subtitles") {
            let prefs = model.prefs
            let mode = prefs.text("SubtitleMode").nilIfEmpty ?? "Default"
            Section {
                Picker("Text size", selection: $look.subtitleScale) {
                    Text("Default").tag(0.0)
                    Text("Small").tag(0.8)
                    Text("Normal").tag(1.0)
                    Text("Large").tag(1.3)
                    Text("Extra large").tag(1.6)
                }
                Picker("Text colour", selection: $look.subtitleColor) {
                    Text("Default").tag("")
                    Text("White").tag("#FFFFFFFF")
                    Text("Yellow").tag("#FFFFE000")
                    Text("Green").tag("#FF7CFC00")
                    Text("Cyan").tag("#FF00E5FF")
                }
                Picker("Background", selection: $look.subtitleBox) {
                    Text("Default").tag(0)
                    Text("Dark box").tag(1)
                    Text("No box").tag(2)
                }
                Picker("Position", selection: $look.subtitlePosition) {
                    Text("Default").tag(0)
                    Text("Bottom").tag(100)
                    Text("Raised").tag(90)
                    Text("High").tag(80)
                }
            } header: {
                Text("Appearance in this app")
            } footer: {
                Text(Self.lookSources(enhanced: model.enhanced?.subtitle != nil))
            }
            Section {
                LanguagePicker(title: "Subtitle language", key: "SubtitleLanguagePreference")
                Picker("Subtitle mode", selection: Binding(get: { mode }, set: { prefs.set("SubtitleMode", $0) })) {
                    ForEach(Self.modes, id: \.key) { Text($0.name).tag($0.key) }
                }
                ServerToggle(title: "Remember subtitle selections", key: "RememberSubtitleSelections", fallback: true)
            } footer: {
                Text(Self.modes.first { $0.key == mode }?.about ?? "")
            }
        }
    }
}

struct HomeSettings: View {
    @Environment(AppModel.self) private var model
    /// The kinds of the web client.
    static let kinds: [(key: String, name: String)] = [
        ("latestmedia", "Recently Added"), ("smalllibrarytiles", "My Media"),
        ("librarybuttons", "My Media (small)"), ("resume", "Continue Watching"), ("nextup", "Next Up"),
        ("resumeaudio", "Continue Listening"), ("resumebook", "Continue Reading"),
        ("livetv", "Live TV"), ("activerecordings", "Active Recordings"),
    ]
    /// Rows Jellyfin Web shows and Bloom has no page for.
    static let webOnly: Set<String> = ["resumeaudio", "resumebook", "livetv", "activerecordings"]
    static let slots = 10

    static func name(_ kind: String) -> String { kinds.first { $0.key == kind }?.name ?? kind }

    var body: some View {
        ServerSection(title: "Home", editable: true) {
            let rows = (0..<Self.slots).map { model.prefs.homeSection($0) }.filter { $0 != "none" }
            Section {
                ForEach(Array(rows.enumerated()), id: \.offset) { _, kind in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(Self.name(kind))
                        if Self.webOnly.contains(kind) {
                            Text("Jellyfin Web only; Bloom skips it").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityIdentifier("home.row.\(kind)")
                }
                .onMove { from, to in
                    var moved = rows
                    moved.move(fromOffsets: from, toOffset: to)
                    save(moved)
                }
                .onDelete { gone in
                    var kept = rows
                    kept.remove(atOffsets: gone)
                    save(kept)
                }
            } header: {
                Text("Rows, top to bottom")
            } footer: {
                Text("Tap Edit to move or remove a row. Jellyfin Web uses the same list.")
            }
            let free = Self.kinds.filter { !rows.contains($0.key) && !Self.webOnly.contains($0.key) }
            if rows.count < Self.slots, !free.isEmpty {
                Section("Add a row") {
                    ForEach(free, id: \.key) { kind in
                        Button { save(rows + [kind.key]) } label: {
                            Label(kind.name, systemImage: "plus.circle.fill").foregroundStyle(Color.ink)
                        }
                        .accessibilityIdentifier("home.add.\(kind.key)")
                    }
                }
            }
        }
    }

    /// The list in its slots, the rest empty, in one save.
    private func save(_ rows: [String]) {
        var changes: [String: String] = [:]
        for slot in 0..<Self.slots { changes["homesection\(slot)"] = slot < rows.count ? rows[slot] : "none" }
        model.prefs.setCustom(changes)
    }
}

struct DownloadSettings: View {
    @Environment(AppModel.self) private var model
    @Bindable private var local = Preferences.shared

    private static func bytes(_ count: Int64) -> String { ByteCountFormatter.string(fromByteCount: count, countStyle: .file) }

    var body: some View {
        List {
            if !model.prefs.canDownload {
                Section {
                    Text("The server does not allow this account to download files.").foregroundStyle(.secondary)
                }
            }
            Section {
                Toggle("Download over cellular", isOn: $local.downloadOverCellular)
            } footer: {
                Text("Off: downloads wait for Wi-Fi.")
            }
            Section {
                Picker("At the same time", selection: $local.downloadsAtOnce) {
                    Text("1").tag(1)
                    Text("2").tag(2)
                }
                Picker("Storage limit", selection: $local.downloadLimitGB) {
                    Text("No limit").tag(0)
                    ForEach([10, 25, 50, 100, 250, 500], id: \.self) { gb in Text("\(gb) GB").tag(gb) }
                }
            } footer: {
                Text("The space all downloads may take: a download that would go over does not start. \(Self.bytes(Downloads.reserve)) always stay free.")
            }
            Section("Space") {
                LabeledContent("Downloads", value: Self.bytes(Downloads.shared.usedBytes))
                if let free = Downloads.freeBytes {
                    LabeledContent("Free on this device", value: Self.bytes(free))
                }
            }
            Section {
                Button("Open Downloads") {
                    model.settings = false
                    model.account = false
                    model.tab = .downloads
                }
            }
        }
        .navigationTitle("Downloads")
        .onChange(of: local.downloadsAtOnce) { Downloads.shared.applyParallel() }
        .task { await model.prefs.ensureLoaded(model.server) }
    }
}

struct AccountSettings: View {
    @Environment(AppModel.self) private var model
    @State private var changing = false

    var body: some View {
        List {
            Section {
                HStack(spacing: 14) {
                    Group {
                        if let picture = model.prefs.picture {
                            Picture(url: picture, corner: 32)
                        } else {
                            Image(systemName: "person.crop.circle.fill").resizable().foregroundStyle(.secondary)
                        }
                    }
                    .frame(width: 64, height: 64)
                    .clipShape(.circle)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(model.active?.userName ?? "—").font(.title3.weight(.semibold))
                        Text(model.active?.serverName ?? "").foregroundStyle(.secondary)
                    }
                }
                .listRowBackground(Color.clear)
            }
            Section("Profile") {
                LabeledContent("Signed in as", value: model.active?.userName ?? "—")
                LabeledContent("Server", value: model.active?.serverName ?? "—")
                LabeledContent("Address", value: model.active?.url ?? "—")
                if model.prefs.isAdmin { LabeledContent("Role", value: "Administrator") }
            }
            Section {
                Button("Change password") { changing = true }
            }
        }
        .navigationTitle("Account")
        .task { await model.prefs.ensureLoaded(model.server) }
        .sheet(isPresented: $changing) { PasswordSheet().bloomTheme().toastHost().environment(model) }
    }
}

/// Change password: the current one (when the user has one), the new one
/// twice. A secure field each; the passwords never leave this sheet but
/// for the request.
struct PasswordSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var current = ""
    @State private var new = ""
    @State private var again = ""
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                if model.prefs.hasPassword {
                    SecureField("Current password", text: $current).textContentType(.password)
                }
                Section {
                    SecureField("New password", text: $new).textContentType(.newPassword)
                    SecureField("New password again", text: $again).textContentType(.newPassword)
                } footer: {
                    if !again.isEmpty, again != new { Text("The two new passwords differ.").foregroundStyle(Color.fail) }
                }
                if let error { Section { Text(error).foregroundStyle(Color.fail) } }
            }
            .navigationTitle("Change password")
            .navigationBarTitleDisplayMode(.inline)
            .disabled(busy)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { Task { await save() } }.disabled(new.isEmpty || new != again)
                }
            }
        }
    }

    private func save() async {
        guard let server = model.server else { return }
        busy = true
        defer { busy = false }
        do {
            try await server.changePassword(current: current, new: new)
            model.say("Password changed")
            dismiss()
        } catch BloomError.Answered, BloomError.SignInAgain {
            error = "The server refused it. Check the current password."
        } catch {
            self.error = Words.error(error)
        }
    }
}

/// Approves another device's Quick Connect code for this user.
struct AuthorizeSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var code = ""
    @State private var message: String?
    @State private var done = false
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Code", text: $code)
                        .keyboardType(.numberPad)
                        .textContentType(.oneTimeCode)
                        .font(.system(.title2, design: .monospaced))
                        .focused($focused)
                    Button("Sign In the Device") { Task { await approve() } }
                        .disabled(code.filter(\.isNumber).count != 6)
                } footer: {
                    Text(message ?? "Enter the 6-digit code the other device shows.")
                        .foregroundStyle(done ? Color.live : (message == nil ? .secondary : Color.fail))
                }
            }
            .navigationTitle("Quick Connect")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
            .onAppear { focused = true }
        }
        .presentationDetents([.medium])
    }

    private func approve() async {
        do {
            try await model.server?.quickConnectAuthorize(code: code.filter(\.isNumber))
            done = true
            message = "The device is signed in."
        } catch BloomError.Answered(_) {
            done = false
            message = "The server does not know this code, or it expired."
        } catch {
            done = false
            message = Words.error(error)
        }
    }
}

/// Play on (inventory §4.1): this device, or another Jellyfin app of this
/// user. While one is chosen, every play goes to it. Polled every 2 s
/// while the sheet is open, in the foreground only (spec §16.7).
struct PlayOnSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var sessions: [RemoteSession] = []
    @State private var searched = false
    @State private var casts: [CastDevice] = []
    /// Discovery runs while the sheet is open (spec §8.4); a test uses the
    /// core's fake Chromecast instead and finds no real one.
    @State private var discovery: CastDiscovery?
    @State private var mock: CastMock?
    /// "media": a fake device without the Jellyfin receiver app.
    private let useMock = ProcessInfo.processInfo.environment["BLOOM_CHROMECAST_MOCK"]
    /// Watches for the refusal of the local network (spec §8.4).
    @State private var network = LocalNetworkCheck()

    var body: some View {
        NavigationStack {
            List {
                if let remote = model.remote {
                    Section(remote.deviceName) {
                        Text(remote.nowPlaying ?? "Nothing plays").font(.headline)
                        if remote.nowPlaying != nil {
                            HStack(spacing: 28) {
                                Button("Back 10 seconds", systemImage: "gobackward.10") {
                                    model.remoteCommand(.seek(secs: max(0, remote.positionSecs - 10)))
                                }
                                Button(remote.paused ? "Play" : "Pause", systemImage: remote.paused ? "play.fill" : "pause.fill") {
                                    model.remoteCommand(.playPause)
                                }
                                Button("Forward 30 seconds", systemImage: "goforward.30") {
                                    model.remoteCommand(.seek(secs: remote.positionSecs + 30))
                                }
                                Button("Stop", systemImage: "stop.fill") { model.remoteCommand(.stop) }
                            }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.borderless)
                            .font(.title3)
                            .frame(maxWidth: .infinity)
                        }
                    }
                }
                if let device = model.castDevice {
                    Section(device.name) {
                        CastControls()
                    }
                }
                Section("Play on") {
                    Button {
                        model.remote = nil
                        model.endCast()
                    } label: {
                        HStack {
                            Label("This device", systemImage: "iphone")
                            Spacer()
                            if model.remote == nil, model.castDevice == nil { Image(systemName: "checkmark") }
                        }
                    }
                    ForEach(sessions, id: \.id) { session in
                        Button {
                            model.endCast()
                            model.remote = session
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Label(session.deviceName, systemImage: "tv")
                                    Text([session.client, session.nowPlaying ?? "Nothing plays"].joined(separator: " · "))
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                if model.remote?.id == session.id { Image(systemName: "checkmark") }
                            }
                        }
                    }
                    if searched, sessions.isEmpty {
                        Text("No other Jellyfin app of yours takes commands now. Open one, and it shows here.")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
                Section("Chromecast") {
                    ForEach(casts, id: \.id) { device in
                        Button {
                            model.choose(device)
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Label(device.name, systemImage: "tv.and.mediabox")
                                    Text(device.model).font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                if model.castDevice?.id == device.id { Image(systemName: "checkmark") }
                            }
                        }
                    }
                    if casts.isEmpty {
                        if network.denied {
                            LocalNetworkDenied()
                        } else {
                            Text(searched ? "No Chromecast found on this network." : "Searching…")
                                .font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .navigationTitle("Play on")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .task {
                switch useMock {
                case "1": mock = CastMock.start()
                case "media": mock = CastMock.mediaReceiver()
                default:
                    // A test of the refusal (BLOOM_LOCAL_NETWORK_DENIED)
                    // looks for no device.
                    network.start()
                    if !network.denied { discovery = CastDiscovery.start() }
                }
            }
            // The poll stops while the app is in the background.
            .task(id: scenePhase) {
                while !Task.isCancelled, scenePhase == .active {
                    sessions = await model.remoteSessions()
                    casts = mock.map { [$0.device()] } ?? discovery?.devices() ?? []
                    searched = true
                    try? await Task.sleep(for: .seconds(2))
                }
            }
            // Discovery stops with the sheet (spec §8.4).
            .onDisappear {
                discovery = nil
                network.stop()
            }
        }
        .presentationDetents([.medium, .large])
    }
}

/// The chosen device in the tab bar's accessory: what it plays, pause.
struct RemoteAccessory: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "tv")
            VStack(alignment: .leading, spacing: 0) {
                Text(model.remote?.deviceName ?? "").font(.caption).foregroundStyle(.secondary)
                Text(model.remote?.nowPlaying ?? "Nothing plays").font(.subheadline.weight(.medium)).lineLimit(1)
            }
            Spacer()
            if model.remote?.nowPlaying != nil {
                Button(model.remote?.paused == true ? "Play" : "Pause",
                       systemImage: model.remote?.paused == true ? "play.fill" : "pause.fill") {
                    model.remoteCommand(.playPause)
                }
                .labelStyle(.iconOnly)
            }
        }
        .padding(.horizontal)
        .contentShape(.rect)
        .onTapGesture { model.showPlayOn = true }
        // The device's state, every 5 s while the accessory shows and the
        // app is in the foreground.
        .task(id: scenePhase) {
            while !Task.isCancelled, scenePhase == .active {
                _ = await model.remoteSessions()
                try? await Task.sleep(for: .seconds(5))
            }
        }
    }
}

/// The Chromecast in the tab bar's accessory; the model keeps its status.
struct CastAccessory: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let status = model.castStatus
        HStack(spacing: 12) {
            Image(systemName: "tv.and.mediabox")
            VStack(alignment: .leading, spacing: 0) {
                Text([model.castDevice?.name, status?.title].compactMap { $0 }.joined(separator: " · "))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                Text(line(status))
                    .font(.subheadline.weight(.medium)).monospacedDigit()
            }
            Spacer()
            if status?.loaded == true {
                Button(status?.paused == true ? "Play" : "Pause", systemImage: status?.paused == true ? "play.fill" : "pause.fill") {
                    if status?.paused == true { model.chromecast?.play() } else { model.chromecast?.pause() }
                }
                .labelStyle(.iconOnly)
            }
        }
        .padding(.horizontal)
        .contentShape(.rect)
        .onTapGesture { model.showPlayOn = true }
    }

    /// The place and the length while an item plays, else the state.
    private func line(_ status: CastStatus?) -> String {
        guard let status, status.connected else { return "Connecting…" }
        guard status.loaded else { return "Nothing plays" }
        let at = Words.clock(status.positionSecs)
        return status.durationSecs.map { "\(at) / \(Words.clock($0))" } ?? at
    }
}


/// The plugin's features, each on as the server says, or off on this
/// device alone.
struct EnhancedSettingsPage: View {
    @Environment(AppModel.self) private var model
    @Bindable private var local = Preferences.shared

    var body: some View {
        List {
            if let server = model.enhancedServer {
                Section {
                    feature("seerr_rows", "Seerr recommendations", server.seerrSimilar || server.seerrRecommended)
                    feature("links", "Seerr, Sonarr and Radarr links", server.seerrLink || server.arrLinks)
                    feature("rating_in_player", "Ratings in the player", server.ratingInPlayer)
                    feature("auto_skip_intro", "Skip intros automatically", server.autoSkipIntro)
                    feature("auto_skip_outro", "Skip credits automatically", server.autoSkipOutro)
                    feature("bookmarks", "Bookmarks", server.bookmarks)
                    feature("hidden_content", "Hidden content", server.hiddenContent)
                    feature("request_more", "Request more seasons", server.requestMore)
                    feature("subtitle_style", "Custom subtitle style", server.subtitle != nil)
                } footer: {
                    Text("A feature your server or your plugin settings turned off does not show. Off here, it is off on this device only.")
                }
                Section {
                    Toggle("Quality tags", isOn: Binding(get: { local.qualityTags ?? server.qualityTags }, set: { on in
                        // The plugin's value again clears this device's choice.
                        local.qualityTags = on == server.qualityTags ? nil : on
                        Task { await model.applyQualityTags() }
                    }))
                } header: {
                    Text("Cards")
                } footer: {
                    Text("4K, HDR, Dolby Vision, Atmos and the like on posters, as your plugin settings choose them. Off here, it is off on this device only.")
                }
            }
        }
        .navigationTitle("Jellyfin Enhanced")
    }

    @ViewBuilder
    private func feature(_ key: String, _ title: String, _ onServer: Bool) -> some View {
        if onServer {
            Toggle(title, isOn: Binding(get: { !local.enhancedOff.contains(key) }, set: { on in
                if on { local.enhancedOff.remove(key) } else { local.enhancedOff.insert(key) }
                model.applyEnhanced()
            }))
            .accessibilityIdentifier("enhanced.\(key)")
        }
    }
}
