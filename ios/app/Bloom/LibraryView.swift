import BloomCore
import SwiftUI

struct LibraryTab: View {
    @Environment(AppModel.self) private var model
    @Namespace private var namespace

    var body: some View {
        List {
            Section {
                ForEach(model.libraries, id: \.id) { library in
                    NavigationLink(value: Route.library(library)) {
                        row(library.name, picture: library.wideArt, symbol: library.librarySymbol)
                    }
                }
            }
            Section {
                NavigationLink(value: Route.favorites) { row("Favorites", picture: nil, symbol: "heart") }
                NavigationLink(value: Route.playlists) { row("Playlists", picture: nil, symbol: "music.note.list") }
            }
        }
        .pageList()
        .navigationTitle("Library")
        .toolbar { ToolbarItem(placement: .topBarTrailing) { AccountButton() } }
        .modifier(Destinations(namespace: namespace))
    }

    /// A library with its own picture, or a list of the app with its glyph
    /// on the same quiet pane: one shape down the page.
    private func row(_ name: String, picture: String?, symbol: String) -> some View {
        HStack(spacing: 14) {
            Picture(url: picture, corner: 8, symbol: symbol).frame(width: 80, height: 45)
            Text(name).font(.body.weight(.medium))
        }
        .padding(.vertical, 2)
    }
}

/// A library, a folder, or one of the app's lists, as a grid that loads
/// the next 100 at its end (spec §8.2, infinite scroll).
struct LibraryPage: View {
    @Environment(AppModel.self) private var model
    let source: GridSource
    @State private var items: [MediaItem] = []
    @State private var total = 0
    @State private var loading = false
    /// A first answer came (or failed): until then no "No items found".
    @State private var loaded = false
    /// Bumped by a new sort or filter: a page asked for before it is dropped.
    @State private var generation = 0
    @State private var error: String?
    @State private var sort: LibrarySort
    @State private var descending: Bool
    /// None shows every item.
    @State private var filter: LibraryFilter?
    /// Genre, decade and rating (BrowseFilters.swift).
    @State private var narrow = BrowseFilters()
    /// What the library can be narrowed to; loaded with the page.
    @State private var choices: FilterChoices?

    private let pageSize: UInt32 = 100

    /// `sort` and `descending` set the first order: Recently Added's
    /// "See all" opens its library newest first.
    init(source: GridSource, sort: LibrarySort = .name, descending: Bool = false) {
        self.source = source
        _sort = State(initialValue: sort)
        _descending = State(initialValue: descending)
    }

    private static let sorts: [LibrarySort] = [.name, .dateAdded, .releaseDate, .communityRating, .datePlayed, .random]
    private static let filters: [LibraryFilter?] = [nil, .unplayed, .played, .favorite, .resumable]

    private static func title(_ sort: LibrarySort) -> String {
        switch sort {
        case .name: "Name"
        case .dateAdded: "Date Added"
        case .releaseDate: "Release Date"
        case .communityRating: "Community Rating"
        case .datePlayed: "Date Played"
        case .random: "Random"
        }
    }

    private static func title(_ filter: LibraryFilter?) -> String {
        switch filter {
        case nil: "All"
        case .unplayed: "Unplayed"
        case .played: "Played"
        case .favorite: "Favorites"
        case .resumable: "In progress"
        }
    }

    /// The grid narrows by genre, decade and rating: a library, a folder
    /// of films and shows, the favourites and a genre's grid; not the
    /// playlists or a collection's few titles.
    private var narrows: Bool {
        source.id != "playlists" && source.kind != .boxSet && source.kind != .folder
    }

    /// The page's order and filters, as one key: a change loads again.
    private var key: String {
        "\(sort)|\(descending)|\(String(describing: filter))|\(narrow.key)"
    }

    var body: some View {
        ScrollView {
            if narrow.active || filter != nil {
                BrowseActiveFilters(narrow: $narrow, filter: filter.map { Self.title($0) }) { filter = nil }
            }
            LazyVGrid(columns: Metrics.posterGrid, spacing: Metrics.gridRowSpacing) {
                // A title hidden a moment ago goes at once (Jellyfin Enhanced).
                ForEach(HiddenTitles.shared.visible(items, on: .library), id: \.id) { item in
                    CardLink(item: item) { PosterCard(item: item) }
                }
                // Until the first answer: the shape of the grid to come.
                if !loaded, items.isEmpty, error == nil {
                    ForEach(0..<12, id: \.self) { _ in PosterSkeleton() }
                }
                if items.count < total {
                    // Keyed by the count: each page that lands asks for the
                    // next while the spinner stays in view.
                    ProgressView().task(id: items.count) { await loadMore() }
                }
            }
            .padding(Metrics.gutter)
        }
        .environment(\.inCollection, source.kind == .boxSet ? source.id : nil)
        .onChange(of: model.collectionChanged) { Task { await reload() } }
        // News of the server (spec §4.7): the grid loads again in place.
        .onChange(of: model.newsRefreshes) { Task { await refreshInPlace() } }
        .overlay {
            if let error {
                ContentUnavailableView("The library could not load", systemImage: "exclamationmark.triangle", description: Text(error))
            } else if loaded, !loading, items.isEmpty, total == 0 {
                if narrow.active || filter != nil {
                    ContentUnavailableView {
                        Label("Nothing matches", systemImage: "line.3.horizontal.decrease")
                    } description: {
                        Text("No title matches these filters.")
                    } actions: {
                        Button("Clear filters") { narrow = BrowseFilters(); filter = nil }.quietButton()
                    }
                } else {
                    ContentUnavailableView("No items found", systemImage: "film")
                }
            }
        }
        .background(Color.page)
        .navigationTitle(source.title)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                if source.kind == .boxSet || source.kind == .folder {
                    Menu {
                        Button("Play All", systemImage: "play.fill") { Task { await playAll(shuffled: false) } }
                        Button("Shuffle", systemImage: "shuffle") { Task { await playAll(shuffled: true) } }
                    } label: {
                        Label("Play", systemImage: "play.circle")
                    }
                    .accessibilityIdentifier("library.play")
                }
                Menu {
                    Picker("Sort by", selection: $sort) {
                        ForEach(Self.sorts, id: \.self) { Text(Self.title($0)).tag($0) }
                    }
                    Picker("Order", selection: $descending) {
                        Text("Ascending").tag(false)
                        Text("Descending").tag(true)
                    }
                } label: {
                    Label("Sort", systemImage: "arrow.up.arrow.down")
                }
                .accessibilityIdentifier("library.sort")
                Menu {
                    Picker("Show", selection: $filter) {
                        ForEach(Self.filters, id: \.self) { Text(Self.title($0)).tag($0) }
                    }
                    if narrows {
                        BrowseFilterMenus(narrow: $narrow, choices: choices, genreFixed: source.id.hasPrefix("genre:"))
                    }
                    if narrow.active || filter != nil {
                        Divider()
                        Button("Clear filters", systemImage: "xmark.circle") { narrow = BrowseFilters(); filter = nil }
                    }
                } label: {
                    Label("Filter", systemImage: narrow.active || filter != nil ? "line.3.horizontal.decrease.circle.fill" : "line.3.horizontal.decrease")
                }
                .accessibilityIdentifier("library.filter")
            }
        }
        .task(id: key) { await reload() }
        .task {
            guard narrows, choices == nil, let server = model.server else { return }
            let library = source.id.contains(":") || source.id == "favorites" ? nil : source.id
            choices = try? await server.filterChoices(libraryId: library, collectionType: source.collectionType)
        }
        .refreshable { await reload() }
    }

    /// One page of the grid in the page's order and filters.
    private func page(start: UInt32, limit: UInt32) async throws -> Page? {
        guard let server = model.server else { return nil }
        return try await server.browsePage(libraryId: source.id, collectionType: source.collectionType, start: start, limit: limit,
                                           sortBy: sort, descending: descending, filter: filter,
                                           genres: narrow.genre.map { [$0] } ?? [], years: narrow.years(choices?.years ?? []),
                                           minRating: narrow.minRating)
    }

    /// Every item of the folder, in the page's order, into the queue.
    private func playAll(shuffled: Bool) async {
        let all = (try? await page(start: 0, limit: 500))?.items ?? items
        model.play(all: all, shuffled: shuffled)
    }

    private func reload() async {
        generation += 1
        loading = false
        loaded = false
        items = []
        total = 0
        await loadMore()
    }

    /// The items loaded so far again, in one request, replacing them in
    /// place: the grid keeps its scroll position, since the cards keep
    /// their ids. A random order keeps its page (a new one would shuffle
    /// it); a failure keeps what shows.
    private func refreshInPlace() async {
        guard sort != .random, !loading else { return }
        loading = true
        // A reload meanwhile owns `loading` and the items (as in `loadMore`).
        let asked = generation
        defer { if generation == asked { loading = false } }
        let order = key
        guard let page = try? await page(start: 0, limit: UInt32(max(items.count, Int(pageSize)))),
              generation == asked, order == key else { return }
        items = page.items
        total = Int(page.total)
        error = nil
        ViewLog.pages.info("LIBRARY: refreshed \(source.id, privacy: .public) (\(items.count) items)")
    }

    private func loadMore() async {
        guard !loading, model.server != nil else { return }
        loading = true
        let asked = generation
        defer { if generation == asked { loading = false } }
        do {
            // Random keeps one fixed page: the server shuffles again for
            // each page, which would repeat titles (spec §8.2).
            let limit = sort == .random ? 200 : pageSize
            guard let page = try await page(start: UInt32(items.count), limit: limit) else { return }
            guard generation == asked else { return }
            if sort == .random, !items.isEmpty { return }
            items += page.items
            total = sort == .random ? items.count : Int(page.total)
            error = nil
            loaded = true
        } catch {
            guard generation == asked else { return }
            self.error = Words.error(error)
            loaded = true
        }
    }
}
