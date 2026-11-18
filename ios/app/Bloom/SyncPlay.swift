import BloomCore
import Foundation
import Synchronization
import SwiftUI

/// The player as SyncPlay drives it (the bridge's `SyncPlayer`). The
/// session calls it from its own thread; MPVUI's player lives on the main
/// actor, so each call hops there (a few milliseconds, which the drift
/// correction absorbs). A load, a stop and the news go to the app.
nonisolated final class SyncBridge: SyncPlayer, Sendable {
    static let shared = SyncBridge()

    /// What the session's thread and the main actor share, under one lock.
    private struct Shared {
        var session: SyncSession?
        var inGroup = false
        /// The load and the seek the session waits for, by token: the next
        /// `restart` of mpv answers them.
        var pendingLoad: UInt64?
        var pendingSeek: UInt64?
        /// The group's load the player is fetching, before mpv has it.
        var awaitingLoad: (token: UInt64, itemId: String)?
        var latestLoad: UInt64?
    }

    private let shared = Mutex(Shared())
    /// The player's mpv, from its start to its close (main actor).
    @MainActor private static weak var target: Mpv?

    /// The session while this device is in a group, for the player's buttons.
    var group: SyncSession? { shared.withLock { $0.inGroup ? $0.session : nil } }

    /// The session while this player follows its group: a pause of the
    /// system (a call, Siri) is then the group's (the Mac's `sync_intent`).
    var following: SyncSession? {
        guard let group, group.state().following else { return nil }
        return group
    }

    /// The settings of the SyncPlay sheet (the Mac's Settings › Playback ›
    /// SyncPlay), kept on the device.
    static let keepInStepKey = "syncKeepInStep"
    static let delayKey = "syncDelayMs"

    func use(_ session: SyncSession?) {
        shared.withLock {
            $0.session = session
            if session == nil { $0.inGroup = false }
        }
        // A new session starts with the settings, as the Mac's `start_sync`.
        let defaults = UserDefaults.standard
        session?.setCorrect(on: defaults.object(forKey: Self.keepInStepKey) as? Bool ?? true)
        session?.setDelay(ms: defaults.double(forKey: Self.delayKey))
    }

    /// The player's mpv, from its start to its close.
    @MainActor func attach(_ mpv: Mpv?) {
        Self.target = mpv
        let session = shared.withLock { $0.session }
        mpv?.listen { [weak self] event in self?.mpvEvent(event) }
        if mpv == nil { session?.closed() }
    }

    /// A call to the player, on the main actor.
    private func onPlayer(_ work: @escaping @MainActor @Sendable (Mpv) -> Void) {
        Task { @MainActor in
            if let mpv = Self.target { work(mpv) }
        }
    }

    private func mpvEvent(_ event: Mpv.Event) {
        let (session, load, seek): (SyncSession?, UInt64?, UInt64?) = shared.withLock { held in
            guard held.inGroup else { return (nil, nil, nil) }
            switch event {
            case .restart:
                defer { held.pendingLoad = nil; held.pendingSeek = nil }
                return (held.session, held.pendingLoad, held.pendingSeek)
            case .loadFailed:
                defer { held.pendingLoad = nil }
                return (held.session, held.pendingLoad, nil)
            default:
                return (held.session, nil, nil)
            }
        }
        guard let session else { return }
        switch event {
        case .position(let position, let paused, let duration):
            session.position(positionSecs: position, paused: paused, durationSecs: duration)
        case .restart:
            if let load { session.loaded(token: load) }
            if let seek { session.settled(token: seek) }
        case .loadFailed:
            if let load { session.loadFailed(token: load) }
        case .stalled(let stalled):
            if stalled { session.stalled() } else { session.recovered() }
        }
    }

    // MARK: SyncPlayer

    func load(itemId: String, positionSecs: Double, token: UInt64) {
        // Armed when mpv has the new file, not before: a seek of the old
        // item meanwhile must not answer for the new one.
        shared.withLock { $0.pendingLoad = nil; $0.awaitingLoad = nil; $0.latestLoad = token }
        Task { @MainActor in AppModel.current?.syncLoad(itemId: itemId, at: positionSecs, token: token) }
    }

    /// The group's item was found: the player's next load is for it.
    func armLoad(_ token: UInt64, itemId: String) { shared.withLock { $0.awaitingLoad = (token, itemId) } }

    /// The group's latest load: an older lookup that answers late is dropped.
    func isLatest(_ token: UInt64) -> Bool { shared.withLock { $0.latestLoad == token } }

    /// The player gave mpv the file: its next ready answers the group.
    /// Only for the group's item: a reload of another (a quality change,
    /// an older answer) does not answer for it.
    func loadIssued(itemId: String) {
        shared.withLock { held in
            guard let awaiting = held.awaitingLoad, awaiting.itemId == itemId else { return }
            held.pendingLoad = awaiting.token
            held.awaitingLoad = nil
        }
    }

    /// The player's load failed: the group stops waiting for this device.
    func loadFailedHere() {
        guard let token = shared.withLock({ $0.awaitingLoad?.token }) else { return }
        failLoad(token)
    }

    /// The group's item could not be found: the group stops waiting for
    /// this device.
    func failLoad(_ token: UInt64) {
        let session = shared.withLock { held in
            held.pendingLoad = nil
            held.awaitingLoad = nil
            return held.session
        }
        session?.loadFailed(token: token)
    }

    func seekPaused(positionSecs: Double, token: UInt64) {
        shared.withLock { $0.pendingSeek = token }
        onPlayer { mpv in
            mpv.setPaused(true)
            mpv.seekExact(to: positionSecs)
        }
    }

    func unpause() { onPlayer { $0.setPaused(false) } }
    func pause() { onPlayer { $0.setPaused(true) } }

    func pauseAndSeek(positionSecs: Double) {
        onPlayer { mpv in
            mpv.setPaused(true)
            mpv.seekExact(to: positionSecs)
        }
    }

    func setSyncSpeed(factor: Double) {
        onPlayer { $0.set(property: "speed", String(factor)) }
    }

    func stop() {
        Task { @MainActor in AppModel.current?.player?.finished = true }
    }

    func notice(notice: SyncNotice) {
        switch notice {
        case .joined: shared.withLock { $0.inGroup = true }
        case .left: shared.withLock { $0.inGroup = false }
        default: break
        }
        Task { @MainActor in AppModel.current?.syncNotice(notice) }
    }
}

extension AppModel {
    /// The group plays an item: open the player paused at its place, or
    /// load it in the player that is open.
    func syncLoad(itemId: String, at secs: Double, token: UInt64) {
        Task {
            let detail = try? await server?.detail(itemId: itemId)
            // The group moved on meanwhile: a newer load is the one to answer.
            guard SyncBridge.shared.isLatest(token) else { return }
            guard let detail else {
                SyncBridge.shared.failLoad(token)
                say("The group's item could not be found.")
                return
            }
            SyncBridge.shared.armLoad(token, itemId: detail.item.id)
            if let player {
                await player.play(detail.item, at: secs, paused: true)
            } else {
                playing = PlayRequest(item: detail.item, fromStart: false, startSecs: secs, paused: true)
            }
        }
    }

    func syncNotice(_ notice: SyncNotice) {
        syncState = syncSession?.state()
        switch notice {
        case .joined(let group): say("Joined \(group)")
        case .left: say("Left the group")
        case .userJoined(let user): say("\(user) joined the group")
        case .userLeft(let user): say("\(user) left the group")
        case .state(let state, let reason):
            if state == .waiting, reason == .buffer {
                say("The group waits for someone")
            }
        case .queueChanged: break
        case .denied(let reason): say(Self.syncDenied(reason))
        case .steppedAside: say("Out of step with the group; it plays on without this device.")
        case .failed(let what): say("Did not reach the server: \(what)")
        }
    }

    static func syncDenied(_ reason: SyncDenial) -> String {
        switch reason {
        case .groupDoesNotExist: "The group no longer exists."
        case .libraryAccessDenied: "Someone in the group cannot see this item."
        case .createGroupDenied: "You may not create a group."
        case .joinGroupDenied: "You may not join this group."
        case .syncPlayIsDisabled: "SyncPlay is off on this server."
        case .other(let name): "The server refused it (\(name))."
        }
    }

    /// Whether this user may use SyncPlay, and make groups. A name this
    /// app does not know may join: only `off` keeps a user out, and only
    /// `createAndJoinGroups` makes groups.
    var syncAllowed: Bool { prefs.syncplayAccess != .off }
}

extension SyncGroupState {
    /// The server's word (`GroupStateType`), as the sheet shows it.
    var name: String {
        switch self {
        case .idle: "Idle"
        case .waiting: "Waiting"
        case .paused: "Paused"
        case .playing: "Playing"
        }
    }
}

/// SyncPlay (inventory §4.6): the groups, or the group this device is in.
struct SyncPlaySheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var groups: [SyncGroup]?
    @State private var titles: [String: String] = [:]
    @AppStorage(SyncBridge.keepInStepKey) private var keepInStep = true
    @AppStorage(SyncBridge.delayKey) private var delayMs = 0.0

    var body: some View {
        NavigationStack {
            List {
                if let state = model.syncState, let group = state.group {
                    inGroup(state, group)
                } else {
                    notInGroup
                }
            }
            .navigationTitle("SyncPlay")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .task {
                // The state and the list, while the sheet is open and the
                // app is in front (the audio of a group plays on behind).
                while !Task.isCancelled {
                    if model.foreground {
                        model.syncState = model.syncSession?.state()
                        if model.syncState?.group == nil {
                            groups = (try? await model.server?.syncplayGroups()) ?? []
                        } else {
                            await loadTitles()
                        }
                    }
                    try? await Task.sleep(for: .seconds(2))
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    @ViewBuilder
    private var notInGroup: some View {
        if model.syncSession == nil {
            Section {
                Text("SyncPlay needs the live connection to the server, which is not open now.")
                    .foregroundStyle(.secondary)
            }
        } else {
            Section {
                if let groups {
                    ForEach(groups, id: \.id) { group in
                        Button {
                            model.syncSession?.user(intent: .join(groupId: group.id))
                        } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(group.name)
                                Text(group.members.joined(separator: ", ")).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    if groups.isEmpty {
                        Text("No group on the server now.").foregroundStyle(.secondary)
                    }
                } else {
                    ProgressView()
                }
            } header: {
                Text("Groups")
            } footer: {
                Text("Watch together: everyone in a group plays the same thing at the same time.")
            }
            if model.prefs.syncplayAccess == .createAndJoinGroups {
                Section {
                    Button("New group", systemImage: "plus") {
                        let name = "\(model.active?.userName ?? "My")'s group"
                        model.syncSession?.user(intent: .create(name: name))
                    }
                    .accessibilityIdentifier("syncplay.new")
                }
            }
        }
    }

    @ViewBuilder
    private func inGroup(_ state: SyncState, _ group: SyncGroup) -> some View {
        Section(group.name) {
            LabeledContent("State", value: group.state.name)
            LabeledContent("Members", value: group.members.joined(separator: ", "))
        }
        if !state.queue.isEmpty {
            Section("Queue") {
                ForEach(Array(state.queue.enumerated()), id: \.element.playlistItemId) { index, entry in
                    Button {
                        model.syncSession?.user(intent: .jump(playlistItemId: entry.playlistItemId))
                    } label: {
                        HStack {
                            Text(titles[entry.itemId] ?? "…")
                            Spacer()
                            if index == state.playingIndex.map(Int.init) { Image(systemName: "speaker.wave.2.fill") }
                        }
                    }
                }
                HStack(spacing: 24) {
                    Button(state.shuffle ? "Shuffle on" : "Shuffle off", systemImage: "shuffle") {
                        model.syncSession?.user(intent: .toggleShuffle)
                    }
                    .foregroundStyle(state.shuffle ? Color.ink : .secondary)
                    let mode = state.repeat
                    Button(Self.repeatName(mode), systemImage: mode == .one ? "repeat.1" : "repeat") {
                        model.syncSession?.user(intent: .cycleRepeat)
                    }
                    .foregroundStyle(mode == .off ? .secondary : Color.ink)
                }
                .buttonStyle(.borderless)
            }
        }
        // The group's actions come before the settings of this player: the
        // sheet opens at half height, and a List does not make the rows
        // below its fold, so a Leave under the settings was out of reach.
        Section {
            if state.following {
                Button("Play on this device alone") { model.syncSession?.user(intent: .stopFollowing) }
            } else {
                Button("Follow the group again") { model.syncSession?.user(intent: .follow) }
            }
            Button("Leave the group", role: .destructive) { model.syncSession?.user(intent: .leave) }
                .accessibilityIdentifier("syncplay.leave")
        } footer: {
            // How well this player is in step (the Mac panel's last line).
            Text(Self.stats(state))
                .monospacedDigit()
                .accessibilityIdentifier("syncplay.stats")
        }
        Section {
            Toggle("Keep in step with the group", isOn: $keepInStep)
                .onChange(of: keepInStep) { model.syncSession?.setCorrect(on: keepInStep) }
            Picker("Delay of this player", selection: $delayMs) {
                ForEach(Self.delays, id: \.self) { ms in Text(Self.delayName(ms)).tag(ms) }
            }
            .onChange(of: delayMs) { model.syncSession?.setDelay(ms: delayMs) }
        } header: {
            Text("This player")
        } footer: {
            Text("Keep in step: when this player drifts from the group, it plays a little faster or slower for a moment. Delay: for a sound output with a delay of its own, such as Bluetooth, play earlier by that delay.")
        }
    }

    /// The Mac's choices, in milliseconds; positive plays later.
    static let delays: [Double] = [-250, -200, -150, -100, -50, 0, 50, 100, 150, 200, 250]

    static func delayName(_ ms: Double) -> String {
        if ms == 0 { return "None" }
        return ms > 0 ? String(format: "%.0f ms later", ms) : String(format: "%.0f ms earlier", -ms)
    }

    static func stats(_ state: SyncState) -> String {
        let drift = state.driftMs.map { String(format: "%+.0f ms", $0) } ?? "–"
        return String(format: "Server clock %+.0f ms · ping %.0f ms · drift %@ · %@", state.clockMs, state.pingMs, drift, state.phase.name())
    }

    static func repeatName(_ mode: SyncRepeat) -> String {
        switch mode {
        case .one: "Repeat one"
        case .all: "Repeat all"
        case .off: "Repeat off"
        }
    }

    private func loadTitles() async {
        guard let state = model.syncState, let server = model.server else { return }
        for entry in state.queue where titles[entry.itemId] == nil {
            if let detail = try? await server.detail(itemId: entry.itemId) {
                titles[entry.itemId] = detail.item.title
            }
        }
    }
}
