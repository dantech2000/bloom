import BloomCore
import SwiftUI

/// A home row in full, as a grid of wide cards: its "See all". The row's
/// cards show at once; the whole row loads behind them.
struct BrowseRowGrid: View {
    @Environment(AppModel.self) private var model
    let title: String
    /// What the row showed on Home.
    let first: [MediaItem]
    /// The row to load in full; none when Home had all of it (My Media).
    let full: HomeRow?
    @State private var items: [MediaItem]?

    init(title: String, first: [MediaItem], full: HomeRow?) {
        self.title = title
        self.first = first
        self.full = full
    }

    private static let columns = [GridItem(.adaptive(minimum: 160, maximum: 320), spacing: Metrics.cardSpacing, alignment: .top)]

    private var surface: HideSurface? {
        switch full {
        case .continueWatching: .continueWatching
        case .nextUp: .nextUp
        case nil: nil
        }
    }

    private var cardRow: CardLink<WideCard>.Row? {
        switch full {
        case .continueWatching: .continueWatching
        case .nextUp: .nextUp
        case nil: nil
        }
    }

    var body: some View {
        let shown = items ?? first
        ScrollView {
            LazyVGrid(columns: Self.columns, spacing: Metrics.gridRowSpacing) {
                ForEach(surface.map { HiddenTitles.shared.visible(shown, on: $0) } ?? shown, id: \.id) { item in
                    CardLink(item: item, row: cardRow) { WideCard(item: item) }
                }
            }
            .padding(Metrics.gutter)
        }
        .background(Color.page)
        .navigationTitle(title)
        .task {
            guard let full, items == nil, let server = model.server else { return }
            if let all = try? await server.homeRow(row: full) { items = all }
        }
        .refreshable {
            guard let full, let server = model.server else { return }
            if let all = try? await server.homeRow(row: full) { items = all }
        }
    }
}

/// Home for a user with nothing started (no Continue Watching, no Next
/// Up): unwatched titles to pick from, then the genres to browse.
struct BrowseStarter: View {
    @Environment(AppModel.self) private var model
    @State private var picks: [MediaItem] = []
    @State private var genres: [String] = []

    var body: some View {
        VStack(alignment: .leading, spacing: Metrics.rowSpacing) {
            PosterRow(title: "Pick Something", items: HiddenTitles.shared.visible(picks, on: .library))
            if !genres.isEmpty {
                VStack(alignment: .leading, spacing: Metrics.shelfSpacing) {
                    Text("Browse by Genre").font(.title3.weight(.semibold)).accessibilityAddTraits(.isHeader)
                        .padding(.horizontal, Metrics.gutter)
                    ScrollView(.horizontal) {
                        // Two lines of pills, side by side: a long list of
                        // genres stays one short row of the page.
                        LazyHGrid(rows: [GridItem(.fixed(44), spacing: 10), GridItem(.fixed(44))], spacing: 10) {
                            ForEach(genres, id: \.self) { BrowseGenrePill(genre: $0) }
                        }
                    }
                    .contentMargins(.horizontal, Metrics.gutter, for: .scrollContent)
                    .scrollIndicators(.hidden)
                }
            }
        }
        .task {
            guard picks.isEmpty, let server = model.server else { return }
            async let found = try? server.picks(limit: 20)
            async let choices = try? server.filterChoices(libraryId: nil, collectionType: nil)
            picks = await found ?? []
            genres = await choices?.genres ?? []
        }
    }
}
