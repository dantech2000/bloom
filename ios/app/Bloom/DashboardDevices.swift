import BloomCore
import SwiftUI

struct DevicesPage: View {
    @Environment(AppModel.self) private var model
    @State private var deleting: AdminDevice?

    var body: some View {
        Loaded(title: "Devices", every: 30, load: { try await model.adminServer().adminDevices() }) { devices, reload in
            List(devices, id: \.id) { device in
                VStack(alignment: .leading, spacing: 2) {
                    Text(device.name)
                    Text([device.app, device.user, device.lastActive.map(When.text)].compactMap { $0 }.joined(separator: " · "))
                        .font(.caption).foregroundStyle(.secondary)
                }
                .swipeActions {
                    if device.id != model.deviceId {
                        Button("Delete", role: .destructive) { deleting = device }
                    }
                }
            }
            .emptyState(devices.isEmpty, "No devices", symbol: "ipad.and.iphone", "A device shows here once it signs in.")
            .confirmationDialog("Delete \(deleting?.name ?? "")?", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
                                titleVisibility: .visible, presenting: deleting) { device in
                Button("Delete device", role: .destructive) {
                    Task {
                        do { try await model.server?.adminDeleteDevice(deviceId: device.id); await reload() } catch { model.say(Words.error(error)) }
                    }
                }
            } message: { _ in
                Text("It is signed out and must sign in again.")
            }
        }
    }
}

/// The activity log, filtered and sorted by the server (Jellyfin 12.0):
/// the kind of event, the level, the user, how far back, newest or oldest
/// first. A page holds a hundred entries; Load More reads the next one.
struct ActivityPage: View {
    @Environment(AppModel.self) private var model
    @State private var kind = ActivityKind.all
    @State private var level = ActivityLevel.all
    /// "" for every user, `serverMark` for the server's own entries.
    @State private var who = ""
    @State private var period = ActivityPeriod.any
    @State private var oldestFirst = false
    @State private var entries: [ActivityLine] = []
    @State private var total: UInt32 = 0
    @State private var users: [String] = []
    @State private var error: String?
    @State private var loaded = false
    @State private var loadingMore = false
    @State private var filtering = false
    private static let serverMark = "\u{1}server"
    private static let page: UInt32 = 100

    private var filtered: Bool { kind != .all || level != .all || !who.isEmpty || period != .any }

    /// What the filters are: a change loads the first page again.
    private var key: String { "\(kind)|\(level)|\(who)|\(period)|\(oldestFirst)" }

    var body: some View {
        List {
            if filtered || oldestFirst {
                Section {
                    Text(summary).font(.footnote).foregroundStyle(.secondary)
                        .accessibilityIdentifier("activity.summary")
                }
            }
            ForEach(entries, id: \.id) { entry in
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
            if entries.count < Int(total) {
                Button {
                    Task { await load(from: UInt32(entries.count)) }
                } label: {
                    HStack {
                        Text("Load More")
                        Spacer()
                        if loadingMore { ProgressView() } else { Text("\(entries.count) of \(total)").foregroundStyle(.secondary) }
                    }
                }
                .disabled(loadingMore)
                .accessibilityIdentifier("activity.more")
            }
        }
        .overlay {
            if !loaded {
                if let error {
                    ContentUnavailableView {
                        Label("Activity could not load", systemImage: "exclamationmark.triangle")
                    } description: { Text(error) } actions: {
                        Button("Try Again") { Task { await load(from: 0) } }
                    }
                } else {
                    ProgressView()
                }
            } else if entries.isEmpty {
                ContentUnavailableView {
                    Label(filtered ? "No matching activity" : "No activity", systemImage: "list.bullet.rectangle")
                } description: {
                    Text(filtered ? "No entries match these filters." : "Sign-ins, playback and changes show here.")
                } actions: {
                    if filtered { Button("Clear Filters") { clear() } }
                }
            }
        }
        .navigationTitle("Activity")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { filtering = true } label: {
                    Label("Filter", systemImage: filtered ? "line.3.horizontal.decrease.circle.fill" : "line.3.horizontal.decrease.circle")
                }
                .accessibilityIdentifier("activity.filter")
            }
        }
        // The filters as a form in a sheet: each a row with its value that
        // opens the list of its choices.
        .sheet(isPresented: $filtering) {
            NavigationStack {
                Form {
                    Section {
                        Picker("Kind", selection: $kind) {
                            ForEach(Self.kinds, id: \.0) { item in Text(item.1).tag(item.0) }
                        }
                        Picker("Level", selection: $level) {
                            ForEach(Self.levels, id: \.0) { item in Text(item.1).tag(item.0) }
                        }
                        Picker("User", selection: $who) {
                            Text("Every user").tag("")
                            Text("The server").tag(Self.serverMark)
                            ForEach(users, id: \.self) { Text($0).tag($0) }
                        }
                        Picker("Time", selection: $period) {
                            ForEach(Self.periods, id: \.0) { item in Text(item.1).tag(item.0) }
                        }
                    } footer: {
                        Text("The server filters the log.")
                    }
                    Section {
                        Picker("Sort", selection: $oldestFirst) {
                            Text("Newest first").tag(false)
                            Text("Oldest first").tag(true)
                        }
                    }
                    if filtered {
                        Section { Button("Clear Filters", role: .destructive) { clear() } }
                    }
                }
                .pickerStyle(.navigationLink)
                .navigationTitle("Filter")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { filtering = false } } }
            }
            .presentationDetents([.medium, .large])
        }
        .task(id: key) { await load(from: 0) }
        .refreshable { await load(from: 0) }
    }

    private static let kinds: [(ActivityKind, String)] = [
        (.all, "Every kind"), (.playback, "Playback"), (.signIn, "Sign-ins"), (.session, "Sessions"),
        (.user, "Users"), (.plugin, "Plugins"), (.task, "Tasks"), (.subtitle, "Subtitles"),
    ]
    private static let levels: [(ActivityLevel, String)] = [
        (.all, "Every level"), (.information, "Information"), (.warning, "Warnings"), (.error, "Errors"),
    ]
    private static let periods: [(ActivityPeriod, String)] = [
        (.any, "Any time"), (.day, "Last 24 hours"), (.week, "Last 7 days"), (.month, "Last 30 days"),
    ]

    /// "Errors · The server · Last 7 days · Oldest first · 37 entries".
    private var summary: String {
        var parts: [String] = []
        if kind != .all { parts.append(Self.kinds.first { $0.0 == kind }?.1 ?? "") }
        if level != .all { parts.append(Self.levels.first { $0.0 == level }?.1 ?? "") }
        if who == Self.serverMark { parts.append("The server") } else if !who.isEmpty { parts.append(who) }
        if period != .any { parts.append(Self.periods.first { $0.0 == period }?.1 ?? "") }
        if oldestFirst { parts.append("Oldest first") }
        parts.append("\(total) \(total == 1 ? "entry" : "entries")")
        return parts.joined(separator: " · ")
    }

    private func clear() {
        kind = .all
        level = .all
        who = ""
        period = .any
    }

    /// Reads a page: from 0 replaces the list, else adds to it.
    private func load(from start: UInt32) async {
        let filter = ActivityFilter(
            kind: kind, level: level, serverOnly: who == Self.serverMark,
            user: who.isEmpty || who == Self.serverMark ? nil : who,
            period: period, oldestFirst: oldestFirst, start: start, limit: Self.page)
        if start > 0 { loadingMore = true }
        defer { loadingMore = false }
        do {
            let page = try await model.adminServer().adminActivityPage(filter: filter)
            if start == 0 { entries = page.entries } else { entries += page.entries.filter { e in !entries.contains { $0.id == e.id } } }
            total = page.total
            users = page.users
            error = nil
            loaded = true
        } catch {
            if !loaded { self.error = Words.error(error) } else { model.say(Words.error(error)) }
        }
    }
}
