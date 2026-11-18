import BloomCore
import SwiftUI

/// A size in the unit that fits: "194 GB", "11.7 TB".
private func bytes(_ n: UInt64) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(clamping: n), countStyle: .file)
}

struct OverviewPage: View {
    @Environment(AppModel.self) private var model
    @State private var power: Bool?
    /// The disks and paths load once, as on the Mac: they hardly change.
    @State private var storage: ServerStorage?

    var body: some View {
        Loaded(title: "Overview", load: { try await model.adminServer().adminOverview() }) { o, _ in
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
        .task { if storage == nil { storage = try? await model.server?.adminStorage() } }
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
            if storage.disks.isEmpty { Text("This server does not report disk space.").foregroundStyle(.secondary) }
        }
    }
}
