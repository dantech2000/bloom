import BloomCore
import SwiftUI

struct SearchView: View {
    @Environment(AppModel.self) private var model
    @State private var query = ""
    @State private var results: [MediaItem] = []
    @State private var people: [PersonCard] = []
    @State private var discover: [SeerrCard] = []
    @State private var searched = false
    @State private var recent = BrowseRecentSearches()
    @FocusState private var searching: Bool
    @Namespace private var namespace

    var body: some View {
        ScrollView {
            if query.isEmpty {
                BrowseSearchStart(recent: $recent) { query = $0 }
            } else {
                BrowsePeopleRow(people: people)
                LazyVGrid(columns: Metrics.posterGrid, spacing: Metrics.gridRowSpacing) {
                    ForEach(HiddenTitles.shared.visible(results, on: .search), id: \.id) { item in
                        CardLink(item: item) { PosterCard(item: item) }
                    }
                }
                .padding(Metrics.gutter)
                // Titles the library lacks, on Seerr (Jellyfin Enhanced).
                SeerrRow(title: "Discover on Seerr", cards: discover.filter { $0.jellyfinId == nil })
                    .padding(.bottom, 24)
            }
        }
        .overlay {
            if !query.isEmpty, searched, results.isEmpty, people.isEmpty, discover.isEmpty {
                ContentUnavailableView.search(text: query)
            }
        }
        .background(Color.page)
        .navigationTitle("Search")
        .searchable(text: $query, prompt: "Titles and people")
        // The field takes the focus when the tab opens, as iOS's own apps do.
        .searchFocused($searching)
        .onSubmit(of: .search) { recent.add(query) }
        .onAppear {
            searching = true
            // A title index older than ten minutes loads again (the Mac's rule).
            model.loadTitleIndex()
        }
        .task(id: query) {
            let text = query.trimmingCharacters(in: .whitespaces)
            searched = false
            guard !text.isEmpty else {
                results = []
                people = []
                discover = []
                return
            }
            // The title index answers at each key, ranked by core. Without
            // an index the old results stay until the server answers.
            let local = model.titleIndex?.search(text: text)
            if let local {
                results = local
                discover = []
            }
            guard text.count >= 2 else {
                if local == nil { results = []; discover = [] }
                people = []
                return
            }
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled, let server = model.server else { return }
            async let titles = try? server.search(text: text)
            async let names = try? server.searchPeople(query: text)
            let found = await titles ?? []
            let persons = await names
            guard !Task.isCancelled else { return }
            // The server adds what the index does not know (episodes, titles
            // added since); the local matches keep their places, and a
            // failed search leaves them.
            let known = Set((local ?? []).map(\.id))
            results = (local ?? []) + found.filter { !known.contains($0.id) }
            if let persons { people = persons }
            searched = true
            if model.enhanced?.seerrUrl != nil {
                let cards = (try? await server.seerrSearch(query: text)) ?? []
                // A key typed meanwhile owns `discover`.
                guard !Task.isCancelled else { return }
                discover = cards
            } else {
                discover = []
            }
            // A search the user stopped at with something found is a recent
            // one; a key typed meanwhile cancels this.
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, !results.isEmpty || !people.isEmpty else { return }
            recent.add(text)
        }
        .modifier(Destinations(namespace: namespace))
    }
}
