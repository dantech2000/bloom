import BloomCore
import SwiftUI

/// Starts or resumes a download, with a toast when it does not start; the
/// same on a card, the detail page and the downloads list.
enum DownloadAction {
    static func start(_ item: MediaItem, model: AppModel) {
        Task {
            guard let server = model.server else { return }
            if let failure = await Downloads.shared.download(item, server: server) {
                model.say("The download did not start: \(failure)")
            } else {
                model.say("Downloading \(item.name)")
            }
        }
    }

    static func resume(_ entry: DownloadEntry, model: AppModel) {
        Task {
            if let failure = await Downloads.shared.resume(entry.id, server: model.offline ? nil : model.server) {
                model.say("The download did not start: \(failure)")
            }
        }
    }
}

/// The downloads (spec §8.5): those of the active server grouped by
/// series, or Movies; then those of other servers, which open only through
/// their own server (the Mac's rule). A tap plays a finished one from this
/// device; a swipe or a long press has the rest.
struct DownloadsView: View {
    @Environment(AppModel.self) private var model
    @State private var confirmRemoveAll = false
    /// The sections and their order, made again only when a download comes
    /// or goes or the server changes, not at each tick of a progress.
    @State private var layout = Layout()
    private var downloads: Downloads { Downloads.shared }

    private static func bytes(_ count: Int64) -> String { ByteCountFormatter.string(fromByteCount: count, countStyle: .file) }

    /// A section of the list: its title and the keys of its entries, in order.
    struct DownloadGroup: Identifiable {
        let id: String
        let title: String
        let keys: [String]
    }

    struct Layout {
        var mine: [DownloadGroup] = []
        var others: [DownloadGroup] = []
    }

    var body: some View {
        // The entries as they are now, for the rows and the sizes.
        let live = Dictionary(downloads.entries.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        List {
            if model.offline {
                Section {
                    Label("The server cannot be reached. Your downloads play from this device.", systemImage: "wifi.slash")
                        .foregroundStyle(.secondary)
                    Button("Try the server again") { Task { await model.reload() } }
                }
            }
            ForEach(layout.mine) { group in
                let entries = group.keys.compactMap { live[$0] }
                Section("\(group.title) · \(Self.bytes(entries.reduce(0) { $0 + $1.bytes }))") {
                    ForEach(entries) { entry in row(entry, mine: true) }
                }
            }
            ForEach(layout.others) { group in
                Section {
                    ForEach(group.keys.compactMap { live[$0] }) { entry in row(entry, mine: false) }
                } header: {
                    Text(group.title)
                } footer: {
                    Text("Sign in to that server to play these.")
                }
            }
        }
        .pageList()
        .overlay {
            if downloads.entries.isEmpty {
                ContentUnavailableView("Nothing is downloaded", systemImage: "arrow.down.circle",
                                       description: Text("Use the download button on a movie, an episode or a season."))
            }
        }
        .navigationTitle("Downloads")
        .toolbar {
            // Apart from the account: a slip of the finger must not land
            // on Remove all.
            if !downloads.entries.isEmpty {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Remove all", systemImage: "trash") { confirmRemoveAll = true }
                }
                ToolbarSpacer(.fixed, placement: .topBarTrailing)
            }
            ToolbarItem(placement: .topBarTrailing) { AccountButton() }
        }
        .confirmationDialog("Remove every download?", isPresented: $confirmRemoveAll, titleVisibility: .visible) {
            Button("Remove all", role: .destructive) { downloads.entries.map(\.id).forEach { downloads.remove(key: $0) } }
        } message: {
            Text("\(Self.bytes(downloads.usedBytes)) on this device.")
        }
        // Downloads made again from the folder get their names back.
        .task(id: model.offline) { if !model.offline { await downloads.refreshRecovered(model.server) } }
        .task(id: LayoutKey(keys: downloads.entries.map(\.id), names: downloads.entries.map { $0.item.seriesName ?? "" },
                            server: model.active?.serverId)) {
            layout = makeLayout()
        }
    }

    /// What the layout depends on: the entries, their series, the server.
    private struct LayoutKey: Equatable {
        let keys: [String]
        let names: [String]
        let server: String?
    }

    /// The active server's downloads by series, Movies last; then those of
    /// each other server, by its name.
    private func makeLayout() -> Layout {
        let mine = downloads.entries.filter(downloads.isOfActiveServer)
        let groups = Dictionary(grouping: mine) { $0.item.seriesName ?? "Movies" }
            .sorted { ($0.key == "Movies" ? 1 : 0, $0.key) < ($1.key == "Movies" ? 1 : 0, $1.key) }
        let others = Dictionary(grouping: downloads.entries.filter { !downloads.isOfActiveServer($0) }) { $0.server ?? "" }
            .sorted { $0.key < $1.key }
        let accounts = Accounts.load()
        return Layout(
            mine: groups.map { DownloadGroup(id: $0.key, title: $0.key, keys: Self.ordered($0.value).map(\.id)) },
            others: others.map { server, entries in
                DownloadGroup(id: server, title: "From \(accounts.first { $0.serverId == server }?.serverName ?? "another server")",
                      keys: Self.ordered(entries).map(\.id))
            })
    }

    /// Episodes in their order, then by name.
    private static func ordered(_ entries: [DownloadEntry]) -> [DownloadEntry] {
        entries.sorted {
            ($0.item.seasonNumber ?? 0, $0.item.episodeNumber ?? 0, $0.item.name)
                < ($1.item.seasonNumber ?? 0, $1.item.episodeNumber ?? 0, $1.item.name)
        }
    }

    private func row(_ entry: DownloadEntry, mine: Bool) -> some View {
        Button {
            if !mine {
                model.say(Downloads.otherServer)
            } else if entry.state == .done {
                model.play(entry.item)
            }
        } label: {
            DownloadRow(entry: entry)
        }
        .buttonStyle(.plain)
        .opacity(mine ? 1 : 0.6)
        .swipeActions {
            Button("Remove", role: .destructive) { downloads.remove(key: entry.id) }
        }
        .contextMenu {
            if mine, entry.state == .done {
                Button("Play", systemImage: "play.fill") { model.play(entry.item) }
            }
            switch entry.state {
            case .downloading:
                Button("Pause", systemImage: "pause") { downloads.pause(entry.id) }
            case .paused:
                Button("Resume", systemImage: "arrow.down") { DownloadAction.resume(entry, model: model) }
            case .failed:
                Button("Try again", systemImage: "arrow.clockwise") { DownloadAction.resume(entry, model: model) }
            case .done:
                EmptyView()
            }
            Button("Remove", systemImage: "trash", role: .destructive) { downloads.remove(key: entry.id) }
        }
    }
}

struct DownloadRow: View {
    let entry: DownloadEntry

    var body: some View {
        HStack(spacing: 12) {
            Picture(url: entry.item.showPoster ?? entry.item.poster ?? entry.item.wide, corner: 8).frame(width: 54, height: 80)
            VStack(alignment: .leading, spacing: 4) {
                Text([entry.item.episodeCode, entry.item.name].compactMap { $0 }.joined(separator: " · "))
                    .font(.subheadline.weight(.medium)).lineLimit(2)
                switch entry.state {
                case .done:
                    Text(ByteCountFormatter.string(fromByteCount: entry.total, countStyle: .file))
                        .font(.caption).foregroundStyle(.secondary)
                case .downloading:
                    ProgressView(value: entry.progress)
                    Text("\(ByteCountFormatter.string(fromByteCount: entry.received, countStyle: .file)) of \(ByteCountFormatter.string(fromByteCount: entry.total, countStyle: .file))")
                        .font(.caption).foregroundStyle(.secondary).monospacedDigit()
                case .paused:
                    Text("Paused").font(.caption).foregroundStyle(.secondary)
                case .failed:
                    Text(entry.reason ?? "Failed").font(.caption).foregroundStyle(Color.fail).lineLimit(3)
                }
            }
            Spacer(minLength: 0)
            DownloadButton(item: entry.item, entry: entry)
        }
        .contentShape(.rect)
    }
}

/// Download, pause, resume, try again, or the state of a download; the same
/// button on the detail page and in the list (which passes its entry, as
/// a download of another server is not the detail page's).
struct DownloadButton: View {
    @Environment(AppModel.self) private var model
    let item: MediaItem
    var entry: DownloadEntry?
    /// On the detail page's row of discs: the done mark sits on one too.
    var disc = false
    private var downloads: Downloads { Downloads.shared }

    var body: some View {
        // The list's entry as it is now; on the detail page, the active server's.
        let entry = self.entry.map { shown in downloads.entries.first { $0.id == shown.id } } ?? downloads.entry(item.id)
        Group {
            switch entry?.state {
            case nil:
                Button("Download", systemImage: "arrow.down") { DownloadAction.start(item, model: model) }
            case .downloading:
                Button { if let entry { downloads.pause(entry.id) } } label: {
                    ProgressView(value: entry?.progress ?? 0).progressViewStyle(.circular).frame(width: 22, height: 22)
                }
                .accessibilityLabel("Pause the download")
            case .paused:
                Button("Resume the download", systemImage: "arrow.down.circle") { if let entry { DownloadAction.resume(entry, model: model) } }
            case .failed:
                Button("Try again", systemImage: "arrow.clockwise") { if let entry { DownloadAction.resume(entry, model: model) } }
            case .done:
                if disc {
                    Image(systemName: "checkmark.circle.fill").glassDisc().accessibilityLabel("Downloaded")
                } else {
                    Image(systemName: "checkmark.circle.fill").toolTarget().accessibilityLabel("Downloaded")
                }
            }
        }
        .labelStyle(.iconOnly)
        .accessibilityIdentifier("download.\(item.id)")
    }
}
