import BloomCore
import SwiftUI

/// A size in the unit that fits: "194 GB", "11.7 TB".
private func bytes(_ n: UInt64) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(clamping: n), countStyle: .file)
}

struct OverviewPage: View {
    @Environment(AppModel.self) private var model
    @State private var power: Bool?
    /// What loads once beside the polled overview. An observed object, not
    /// plain @State: the list is built inside `Loaded`'s body, and an
    /// answer that came after the overview's first render never showed
    /// there (the disks the tester missed on the container).
    @Observable final class Extras {
        /// The disks and paths load once, as on the Mac: they hardly change.
        var storage: ServerStorage?
        /// Why the disks did not load, in words.
        var storageError: String?
        /// What came in the last 7 days.
        var added: [RecentAddition]?
    }
    @State private var extras = Extras()
    private var storage: ServerStorage? {
        get { extras.storage }
        nonmutating set { extras.storage = newValue }
    }
    private var storageError: String? {
        get { extras.storageError }
        nonmutating set { extras.storageError = newValue }
    }
    private var added: [RecentAddition]? {
        get { extras.added }
        nonmutating set { extras.added = newValue }
    }

    /// The overview and the playbacks the server converts, polled together.
    struct Snapshot {
        let overview: ServerOverview
        let transcodes: [ActiveTranscode]
    }

    var body: some View {
        Loaded(title: "Overview", load: {
            let server = try model.adminServer()
            async let overview = server.adminOverview()
            // A server that refuses the sessions still shows its overview.
            async let transcodes = (try? server.adminTranscodes()) ?? []
            return Snapshot(overview: try await overview, transcodes: await transcodes)
        }) { data, _ in
            let o = data.overview
            List {
                Section {
                    LabeledContent("Name", value: o.name)
                    LabeledContent("Version", value: o.version)
                    if !o.os.isEmpty { LabeledContent("System", value: o.os) }
                    LabeledContent("Active devices", value: "\(o.activeDevices)")
                }
                alerts(o)
                Section("Library") {
                    LabeledContent("Movies", value: "\(o.movies)")
                    LabeledContent("Series", value: "\(o.series)")
                    LabeledContent("Episodes", value: "\(o.episodes)")
                    Button("Scan all libraries", systemImage: "arrow.clockwise") {
                        Task {
                            do { try await model.server?.adminScanLibraries(); model.say("Scan started") } catch { model.say(Words.error(error)) }
                        }
                    }
                }
                addedThisWeek
                if !o.runningTasks.isEmpty {
                    Section("Running") {
                        ForEach(o.runningTasks, id: \.id) { task in
                            VStack(alignment: .leading) {
                                Text(task.name)
                                ProgressView(value: (task.progress ?? 0) / 100)
                            }
                        }
                    }
                }
                Section("Watching now") {
                    ForEach(o.nowPlaying, id: \.rowKey) { session in
                        VStack(alignment: .leading, spacing: 2) {
                            StateLine(text: session.title ?? "—", color: session.paused ? Color.ink.opacity(0.4) : .live)
                            Text([session.user, session.device, session.client, session.method, session.paused ? "Paused" : nil]
                                .compactMap { $0 }.joined(separator: " · "))
                                .font(.caption).foregroundStyle(.secondary).padding(.leading, 16)
                        }
                    }
                    if o.nowPlaying.isEmpty { Text("Nobody plays anything.").foregroundStyle(.secondary) }
                }
                if !data.transcodes.isEmpty { transcodes(data.transcodes) }
                if let storageError {
                    Section("Storage") {
                        Text("Disk space could not load: \(storageError)").foregroundStyle(.secondary)
                    }
                }
                if let storage {
                    disks(storage)
                    if !storage.paths.isEmpty {
                        Section("Paths") {
                            ForEach(storage.paths, id: \.label) { path in
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(path.label).font(.caption).foregroundStyle(.secondary)
                                    Text(path.path).font(.system(.footnote, design: .monospaced)).textSelection(.enabled)
                                }
                            }
                        }
                    }
                }
                Section {
                    Button("Restart server…", systemImage: "restart") { power = false }
                    Button("Shut down server…", systemImage: "power", role: .destructive) { power = true }
                }
            }
            .confirmationDialog(power == true ? "Shut down the server?" : "Restart the server?",
                                isPresented: Binding(get: { power != nil }, set: { if !$0 { power = nil } }), titleVisibility: .visible) {
                Button(power == true ? "Shut down" : "Restart", role: .destructive) {
                    let shutdown = power == true
                    Task {
                        do {
                            try await model.server?.adminPower(shutdown: shutdown)
                            model.say(shutdown ? "The server shuts down" : "The server restarts")
                        } catch {
                            model.say(Words.error(error))
                        }
                    }
                }
            } message: {
                Text(power == true ? "Nobody can watch until someone starts it again on its machine." : "Playback stops for everyone for a minute.")
            }
        }
        .task { await loadOnce() }
    }

    /// The disks and the week's additions, once per visit of the page.
    private func loadOnce() async {
        guard let server = model.server else { return }
        // Side by side: a slow disk answer does not hold the week's list.
        async let week: Void = loadAdded(server)
        if storage == nil {
            let started = Date.now
            do {
                storage = try await server.adminStorage()
                storageError = nil
            } catch {
                storageError = Words.error(error)
            }
            ViewLog.pages.info("OVERVIEW: storage in \(Date.now.timeIntervalSince(started), format: .fixed(precision: 2)) s")
        }
        await week
    }

    private func loadAdded(_ server: Server) async {
        guard added == nil else { return }
        let started = Date.now
        added = (try? await server.adminRecentAdditions(days: 7, limit: 100)) ?? []
        ViewLog.pages.info("OVERVIEW: added in \(Date.now.timeIntervalSince(started), format: .fixed(precision: 2)) s")
    }

    /// A count of what came in the last 7 days, and the list behind it.
    @ViewBuilder private var addedThisWeek: some View {
        Section("Added this week") {
            if let added {
                if added.isEmpty {
                    Text("Nothing new in the last 7 days.").foregroundStyle(.secondary)
                } else {
                    NavigationLink {
                        AddedThisWeekPage(added: added)
                    } label: {
                        LabeledContent(added.count >= 100 ? "100 or more titles" : added.count == 1 ? "1 title" : "\(added.count) titles",
                                       value: Self.split(added))
                    }
                    .accessibilityIdentifier("overview.added")
                }
            } else {
                ProgressView()
            }
        }
    }

    /// "3 films, 12 episodes"
    private static func split(_ added: [RecentAddition]) -> String {
        let films = added.filter { $0.item.kind == .movie }.count
        let episodes = added.filter { $0.item.kind == .episode }.count
        return [films > 0 ? (films == 1 ? "1 film" : "\(films) films") : nil,
                episodes > 0 ? (episodes == 1 ? "1 episode" : "\(episodes) episodes") : nil]
            .compactMap { $0 }.joined(separator: ", ")
    }

    /// The playbacks the server converts: what, for whom, and why.
    @ViewBuilder private func transcodes(_ list: [ActiveTranscode]) -> some View {
        Section {
            ForEach(Array(list.enumerated()), id: \.offset) { _, t in
                VStack(alignment: .leading, spacing: 4) {
                    StateLine(text: t.title.isEmpty ? "—" : t.title, color: t.paused ? Color.ink.opacity(0.4) : .live)
                    Group {
                        Text([t.user, t.device, t.client].filter { !$0.isEmpty }.joined(separator: " · "))
                        Text([t.what, t.encoder].compactMap { $0 }.joined(separator: " · "))
                        if !t.reasons.isEmpty {
                            Text("Why: " + t.reasons.joined(separator: "; "))
                        }
                    }
                    .font(.caption).foregroundStyle(.secondary).padding(.leading, 16)
                    if let progress = t.progress {
                        ProgressView(value: progress / 100).tint(Color.ink).padding(.leading, 16)
                    }
                }
                .accessibilityElement(children: .combine)
            }
        } header: {
            Text("Converting now")
        } footer: {
            Text("The server converts these playbacks for the device they play on, which costs it work.")
        }
    }

    /// What needs a look: a restart, an update, failed tasks, then what the
    /// server did by itself (the Mac's Alerts panel).
    @ViewBuilder private func alerts(_ o: ServerOverview) -> some View {
        Section("Alerts") {
            if o.pendingRestart { StateLine(text: "Restart needed to finish a change", color: .warn) }
            if o.updateAvailable { StateLine(text: "An update of the server is available", color: .warn) }
            ForEach(o.failedTasks, id: \.id) { task in
                VStack(alignment: .leading, spacing: 2) {
                    StateLine(text: "\(task.name) failed", color: .fail)
                    Text(When.text(task.lastEnd)).font(.caption).foregroundStyle(.secondary).padding(.leading, 16)
                }
            }
            ForEach(o.alerts, id: \.rowKey) { entry in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    if entry.severity != .information {
                        Circle().fill(entry.severity == .error ? Color.fail : Color.warn).frame(width: 8, height: 8)
                    }
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.name)
                        Text([entry.overview, When.text(entry.date)].compactMap { $0 }.joined(separator: " · "))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            if !o.pendingRestart && !o.updateAvailable && o.failedTasks.isEmpty && o.alerts.isEmpty {
                Text("No alerts.").foregroundStyle(.secondary)
            }
        }
    }

    /// One row per disk; colour only when it is nearly full.
    @ViewBuilder private func disks(_ storage: ServerStorage) -> some View {
        Section("Storage") {
            ForEach(storage.disks, id: \.rowKey) { disk in
                let total = disk.used + disk.free
                let full = total > 0 ? Double(disk.used) / Double(total) : 0
                VStack(alignment: .leading, spacing: 6) {
                    HStack(alignment: .firstTextBaseline) {
                        Label(disk.labels.joined(separator: ", "), systemImage: disk.network ? "network" : "internaldrive")
                            .lineLimit(2)
                        Spacer()
                        Text(full, format: .percent.precision(.fractionLength(0))).font(.caption).foregroundStyle(.secondary)
                    }
                    ProgressView(value: full).tint(full >= 0.95 ? Color.fail : full >= 0.85 ? Color.warn : Color.ink)
                    Text("\(bytes(disk.used)) used of \(bytes(total)) · \(bytes(disk.free)) free")
                        .font(.caption).foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
            if storage.disks.isEmpty {
                Text("The server sent no disk space. Jellyfin 10.11 and later report it; an older server does not.")
                    .foregroundStyle(.secondary)
            }
        }
    }
}

/// What came into the libraries in the last 7 days, newest first.
struct AddedThisWeekPage: View {
    let added: [RecentAddition]

    var body: some View {
        List(added, id: \.item.id) { entry in
            HStack(spacing: 12) {
                Picture(url: entry.item.showPoster ?? entry.item.poster, corner: 6).frame(width: 40, height: 60)
                VStack(alignment: .leading, spacing: 2) {
                    Text(entry.item.seriesName ?? entry.item.name).lineLimit(2)
                    Text(detail(entry)).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
            }
            .accessibilityElement(children: .combine)
        }
        .navigationTitle("Added this week")
    }

    /// "S1E2 Pilot · 2 days ago", or "2024 · 2 days ago" for a film.
    private func detail(_ entry: RecentAddition) -> String {
        let what = entry.item.seriesName != nil
            ? [entry.item.episodeCode, entry.item.name].compactMap { $0 }.joined(separator: " ")
            : entry.item.year
        return [what, When.text(entry.added)].compactMap { $0 }.joined(separator: " · ")
    }
}
