import BloomCore
import SwiftUI

/// The user's playlists as a grid of posters, with New playlist in the
/// toolbar. With none yet, the page says what a playlist is and offers to
/// make one, instead of "No items found".
struct PlaylistsPage: View {
    @Environment(AppModel.self) private var model
    @State private var lists: [MediaItem]?
    @State private var error: String?
    @State private var naming = false
    @State private var name = ""
    @State private var busy = false

    var body: some View {
        ScrollView {
            LazyVGrid(columns: Metrics.posterGrid, spacing: Metrics.gridRowSpacing) {
                if let lists {
                    ForEach(lists, id: \.id) { list in
                        CardLink(item: list) { PosterCard(item: list) }
                    }
                } else if error == nil {
                    ForEach(0..<6, id: \.self) { _ in PosterSkeleton() }
                }
            }
            .padding(Metrics.gutter)
        }
        .overlay {
            if let error {
                ContentUnavailableView {
                    Label("Your playlists could not load", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { Task { await load() } }.quietButton()
                }
            } else if lists?.isEmpty == true {
                ContentUnavailableView {
                    Label("No playlists yet", systemImage: "music.note.list")
                } description: {
                    Text("A playlist plays titles in the order you choose. Make one here, or add a title from its page or a card's menu.")
                } actions: {
                    Button("New Playlist") { startNaming() }
                        .buttonStyle(MainButtonStyle())
                        .accessibilityIdentifier("playlists.new")
                }
            }
        }
        .disabled(busy)
        .background(Color.page)
        .navigationTitle("Playlists")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("New Playlist", systemImage: "plus") { startNaming() }
                    .accessibilityIdentifier("playlists.add")
            }
        }
        .alert("New playlist", isPresented: $naming) {
            TextField("Name", text: $name)
            Button("Cancel", role: .cancel) {}
            Button("Create") { Task { await create() } }
        } message: {
            Text("It starts empty. Add titles from their page or a card's menu.")
        }
        .onChange(of: model.newsRefreshes) { Task { await load() } }
        .task { await load() }
        .refreshable { await load() }
    }

    private func startNaming() {
        name = ""
        naming = true
    }

    private func load() async {
        guard let server = model.server else { return }
        do {
            lists = try await server.lists(kind: .playlist)
            error = nil
        } catch {
            if lists == nil { self.error = Words.error(error) }
        }
    }

    /// The same request as "Add to playlist", with no title in it yet.
    private func create() async {
        let name = name.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, let server = model.server else { return }
        busy = true
        defer { busy = false }
        do {
            _ = try await server.createList(kind: .playlist, name: name, itemIds: [])
            model.say("Created \(name)")
            await load()
        } catch {
            model.say("Not created: \(Words.error(error))")
        }
    }
}
