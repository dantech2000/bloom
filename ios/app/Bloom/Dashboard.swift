import BloomCore
import SwiftUI

/// The server's dashboard (inventory §1.13), for administrators: a list of
/// sections, each pushed. The access of a user, the triggers of a task and
/// the branding are edited here; the other settings that edit the server's
/// JSON objects open in Jellyfin Web. Every action that cannot be undone
/// asks first.
struct DashboardSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    var body: some View {
        NavigationStack {
            List {
                Section("Server") {
                    NavigationLink { OverviewPage() } label: { Label("Overview", systemImage: "server.rack") }
                    NavigationLink { UsersPage() } label: { Label("Users", systemImage: "person.2") }
                }
                Section("Libraries") {
                    NavigationLink { LibrariesPage() } label: { Label("Libraries", systemImage: "books.vertical") }
                }
                Section("Devices") {
                    NavigationLink { DevicesPage() } label: { Label("Devices", systemImage: "ipad.and.iphone") }
                    NavigationLink { ActivityPage() } label: { Label("Activity", systemImage: "list.bullet.rectangle") }
                }
                Section("Advanced") {
                    NavigationLink { TasksPage() } label: { Label("Scheduled tasks", systemImage: "clock.arrow.circlepath") }
                    NavigationLink { PluginsPage() } label: { Label("Plugins", systemImage: "puzzlepiece.extension") }
                    NavigationLink { LogsPage() } label: { Label("Logs", systemImage: "doc.plaintext") }
                    NavigationLink { ApiKeysPage() } label: { Label("API keys", systemImage: "key") }
                }
                Section {
                    NavigationLink { BrandingPage() } label: { Label("Branding", systemImage: "paintbrush") }
                    ForEach([("General", "dashboard/general"),
                             ("Transcoding", "dashboard/playback/transcoding"), ("Networking", "dashboard/networking"),
                             ("Metadata", "dashboard/libraries/metadata")], id: \.1) { title, path in
                        Button {
                            if let web = try? model.server?.adminWebPage(path: path), let url = URL(string: web) { openURL(url) }
                        } label: {
                            Label(title, systemImage: "safari")
                        }
                    }
                } header: {
                    Text("Settings")
                } footer: {
                    Text("All but Branding open in Jellyfin Web.")
                }
            }
            .navigationTitle("Dashboard")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}

/// Loads a section's data, shows a spinner or why it failed, and loads
/// again every few seconds while it is on screen (the Mac's live reload).
struct Loaded<Data, Content: View>: View {
    @Environment(\.scenePhase) private var scenePhase
    let title: String
    var every: Double = 5
    let load: () async throws -> Data
    @ViewBuilder let content: (Data, @escaping () async -> Void) -> Content
    @State private var data: Data?
    @State private var error: String?

    var body: some View {
        Group {
            if let data {
                content(data, reload)
            } else if let error {
                ContentUnavailableView {
                    Label("\(title) could not load", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { Task { await reload() } }
                }
            } else {
                ProgressView()
            }
        }
        .navigationTitle(title)
        // The poll stops while the app is in the background.
        .task(id: scenePhase) {
            while !Task.isCancelled, scenePhase == .active {
                await reload()
                try? await Task.sleep(for: .seconds(every))
            }
        }
        .refreshable { await reload() }
    }

    private func reload() async {
        do {
            data = try await load()
            error = nil
        } catch {
            if data == nil { self.error = Words.error(error) }
        }
    }
}

/// A state as a dot before its line (DESIGN.md, "The Colour Is State
/// Rule"): the dot carries the colour, the words stay in the ink.
struct StateLine: View {
    let text: String
    let color: Color

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Circle().fill(color).frame(width: 8, height: 8).alignmentGuide(.firstTextBaseline) { $0[.bottom] + 1 }
                .accessibilityHidden(true)
            Text(text)
        }
    }
}

/// The server is gone (a sign-out or a switch while a page polls).
struct SignedOut: LocalizedError {
    var errorDescription: String? { "You are signed out of the server." }
}

extension AppModel {
    /// The server of the session, or `SignedOut`.
    func adminServer() throws -> Server {
        guard let server else { throw SignedOut() }
        return server
    }
}

/// Keys for rows the server sends without an id.
extension ActiveSession {
    var rowKey: String { [user, device, client].joined(separator: "|") }
}

extension ActivityEntry {
    var rowKey: String { [date, name, overview ?? ""].joined(separator: "|") }
}

extension StorageDisk {
    var rowKey: String { labels.joined(separator: "|") }
}

enum When {
    /// A server date, short and local.
    static func text(_ iso: String?) -> String {
        guard let iso, let date = (try? Date(iso, strategy: .iso8601.year().month().day().time(includingFractionalSeconds: true)))
                ?? (try? Date(iso, strategy: .iso8601)) else { return iso ?? "—" }
        return date.formatted(.relative(presentation: .named))
    }
}
