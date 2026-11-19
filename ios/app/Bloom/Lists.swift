import BloomCore
import SwiftUI

/// Which items a list takes (the Mac's `can_add_to_playlist` and
/// `can_add_to_collection`).
extension ListKind {
    var noun: String { self == .playlist ? "playlist" : "collection" }

    func takes(_ item: MediaItem) -> Bool {
        switch self {
        case .playlist: [.movie, .series, .season, .episode, .video].contains(item.kind)
        case .collection: [.movie, .series].contains(item.kind)
        }
    }
}

/// What the "Add to" sheet adds, and to which kind of list.
struct AddRequest: Identifiable {
    let item: MediaItem
    let kind: ListKind
    var id: String { "\(item.id)\(kind)" }
}

/// Add to a playlist or a collection: the lists, or a new one by name.
struct AddToListSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let request: AddRequest
    @State private var lists: [MediaItem]?
    @State private var name = ""
    @State private var busy = false
    @FocusState private var naming: Bool

    var body: some View {
        NavigationStack {
            List {
                Section {
                    HStack {
                        TextField("New \(request.kind.noun)", text: $name)
                            .focused($naming)
                            .submitLabel(.done)
                            .onSubmit { Task { await create() } }
                        Button("Create") { Task { await create() } }
                            .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                }
                Section(request.kind == .playlist ? "Your playlists" : "Collections") {
                    if let lists {
                        ForEach(lists, id: \.id) { list in
                            Button { Task { await add(to: list) } } label: {
                                HStack {
                                    Picture(url: list.poster, corner: 6).frame(width: 36, height: 54)
                                    Text(list.name)
                                }
                            }
                        }
                        if lists.isEmpty {
                            Text("None yet. Name one above.").foregroundStyle(.secondary)
                        }
                    } else {
                        ProgressView()
                    }
                }
            }
            .disabled(busy)
            .navigationTitle("Add to \(request.kind.noun)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
            .task { lists = (try? await model.server?.lists(kind: request.kind)) ?? [] }
        }
        .presentationDetents([.medium, .large])
    }

    private func add(to list: MediaItem) async {
        guard let server = model.server else { return }
        busy = true
        defer { busy = false }
        do {
            try await server.addToList(kind: request.kind, listId: list.id, itemIds: [request.item.id])
            model.say("Added to \(list.name)")
            dismiss()
        } catch {
            model.say("Not added: \(Words.error(error))")
        }
    }

    private func create() async {
        let name = name.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, let server = model.server else { return }
        busy = true
        defer { busy = false }
        do {
            _ = try await server.createList(kind: request.kind, name: name, itemIds: [request.item.id])
            model.say("Added to \(name)")
            dismiss()
        } catch {
            model.say("Not created: \(Words.error(error))")
        }
    }
}

/// A playlist: its entries in order, Play and Shuffle, play from a place;
/// Edit moves and removes entries; rename and delete in the menu.
struct PlaylistPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let playlist: MediaItem
    @State private var entries: [ListEntry]?
    @State private var title: String
    @State private var error: String?
    @State private var renaming = false
    @State private var newName = ""
    @State private var confirmDelete = false

    init(playlist: MediaItem) {
        self.playlist = playlist
        _title = State(initialValue: playlist.name)
    }

    var body: some View {
        List {
            if let entries, !entries.isEmpty {
                Section {
                    HStack(spacing: 10) {
                        Button("Play", systemImage: "play.fill") { model.play(all: entries.map(\.item)) }
                            .primaryButton()
                        Button("Shuffle", systemImage: "shuffle") { model.play(all: entries.map(\.item), shuffled: true) }
                            .quietButton()
                    }
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets())
                }
                Section {
                    ForEach(Array(entries.enumerated()), id: \.element.entryId) { index, entry in
                        Button {
                            model.play(all: entries.dropFirst(index).map(\.item))
                        } label: {
                            HStack(spacing: 12) {
                                Picture(url: entry.item.wide ?? entry.item.poster, corner: 8).frame(width: 96, height: 54)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(entry.item.title).lineLimit(2)
                                    if let runtime = Words.runtime(entry.item.runtimeSecs) {
                                        Text(runtime).font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                            }
                        }
                        .accessibilityHint("Plays from here")
                        .swipeActions {
                            Button("Remove", role: .destructive) { Task { await remove(entry) } }
                        }
                    }
                    .onMove { from, to in Task { await move(from: from, to: to) } }
                }
            }
        }
        .overlay {
            if let error {
                ContentUnavailableView("The playlist could not load", systemImage: "exclamationmark.triangle", description: Text(error))
            } else if entries == nil {
                ProgressView()
            } else if entries?.isEmpty == true {
                ContentUnavailableView("This playlist is empty", systemImage: "list.bullet",
                                       description: Text("Add titles from their page or a card's menu."))
            }
        }
        .scrollContentBackground(.hidden)
        .background(Color.page)
        .navigationTitle(title)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                EditButton()
                Menu {
                    Button("Rename…", systemImage: "pencil") {
                        newName = title
                        renaming = true
                    }
                    Button("Delete playlist…", systemImage: "trash", role: .destructive) { confirmDelete = true }
                } label: {
                    Label("More", systemImage: "ellipsis")
                }
            }
        }
        .alert("Rename playlist", isPresented: $renaming) {
            TextField("Name", text: $newName)
            Button("Cancel", role: .cancel) {}
            Button("Rename") { Task { await rename() } }
        }
        .confirmationDialog("Delete \(title)?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete playlist", role: .destructive) { Task { await delete() } }
        } message: {
            Text("The playlist goes; its titles stay in the library.")
        }
        .task { await load() }
        .refreshable { await load() }
    }

    private func load() async {
        guard let server = model.server else { return }
        do {
            entries = try await server.playlistEntries(listId: playlist.id)
            error = nil
        } catch {
            self.error = Words.error(error)
        }
    }

    private func remove(_ entry: ListEntry) async {
        guard let server = model.server else { return }
        entries?.removeAll { $0.entryId == entry.entryId }
        do {
            try await server.removeFromList(kind: .playlist, listId: playlist.id, ids: [entry.entryId])
        } catch {
            model.say("Not removed: \(Words.error(error))")
            await load()
        }
    }

    /// One step at a time, as the server moves one entry per request.
    private func move(from: IndexSet, to: Int) async {
        guard let server = model.server, var list = entries, let source = from.first else { return }
        list.move(fromOffsets: from, toOffset: to)
        entries = list
        let entry = list[source < to ? to - 1 : to]
        let index = list.firstIndex { $0.entryId == entry.entryId } ?? 0
        do {
            try await server.moveInPlaylist(listId: playlist.id, entryId: entry.entryId, newIndex: UInt32(index))
        } catch {
            model.say("Not moved: \(Words.error(error))")
            await load()
        }
    }

    private func rename() async {
        let name = newName.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, name != title, let server = model.server else { return }
        do {
            try await server.renamePlaylist(listId: playlist.id, name: name)
            title = name
        } catch {
            model.say("Not renamed: \(Words.error(error))")
        }
    }

    private func delete() async {
        guard let server = model.server else { return }
        do {
            try await server.deleteList(listId: playlist.id)
            model.say("Deleted \(title)")
            dismiss()
        } catch {
            model.say("Not deleted: \(Words.error(error))")
        }
    }
}
