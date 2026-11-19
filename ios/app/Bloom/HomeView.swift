import BloomCore
import SwiftUI

struct HomeView: View {
    @Environment(AppModel.self) private var model
    @Namespace private var namespace
    /// The page has gone up past the hero's picture: the bar behind the
    /// clock and the account button turns solid, so a row's heading never
    /// runs under the clock.
    @State private var scrolled = false

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Metrics.rowSpacing) {
                if let home = model.home {
                    if !home.hero.isEmpty {
                        Hero(items: Array(home.hero.prefix(20)))
                    }
                    // A user with nothing started: titles to pick from and
                    // the genres, in place of the personal rows.
                    if newcomer(home) {
                        BrowseStarter()
                    }
                    // The rows in the user's order (Settings › Home, or the
                    // web client); a kind this app has no row for is skipped.
                    ForEach(sections(home), id: \.self) { kind in
                        switch kind {
                        case .resume:
                            WideRow(title: "Continue Watching", items: home.resume, row: .continueWatching,
                                    seeAll: home.resume.count > WideRow.shown
                                        ? AnyView(BrowseRowGrid(title: "Continue Watching", first: home.resume, full: .continueWatching)) : nil)
                        case .nextUp:
                            WideRow(title: "Next Up", items: home.nextUp, row: .nextUp,
                                    seeAll: home.nextUp.count > WideRow.shown
                                        ? AnyView(BrowseRowGrid(title: "Next Up", first: home.nextUp, full: .nextUp)) : nil)
                        case .latestMedia:
                            // By the library's id: two libraries may share a name.
                            ForEach(home.latest, id: \.id) { row in
                                PosterRow(title: "Recently Added in \(row.title)", items: row.items,
                                          seeAll: row.items.count > PosterRow.shown ? latestGrid(row) : nil)
                            }
                        case .smallLibraryTiles, .libraryButtons:
                            WideRow(title: "My Media", items: model.libraries,
                                    seeAll: model.libraries.count > WideRow.shown
                                        ? AnyView(BrowseRowGrid(title: "My Media", first: model.libraries, full: nil)) : nil)
                        default:
                            EmptyView()
                        }
                    }
                    if home.hero.isEmpty, home.resume.isEmpty, home.latest.allSatisfy({ $0.items.isEmpty }) {
                        ContentUnavailableView("Nothing here yet", systemImage: "film", description: Text("Start watching something and it will show up on your home screen."))
                    }
                }
            }
            .padding(.bottom, 32)
        }
        .background(Color.page)
        // The hero runs under the clock; a Home without one starts under
        // the bar.
        .ignoresSafeArea(edges: model.home?.hero.isEmpty == false ? .top : [])
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentOffset.y + geometry.contentInsets.top > 160
        } action: { _, past in
            withAnimation(.easeOut(duration: 0.2)) { scrolled = past }
        }
        .toolbarBackground(Color.page, for: .navigationBar)
        // A Home with no hero starts with a heading: solid at once.
        .toolbarBackgroundVisibility(scrolled || model.home?.hero.isEmpty != false ? .visible : .hidden, for: .navigationBar)
        .refreshable { await model.reload() }
        .toolbar { ToolbarItem(placement: .topBarTrailing) { AccountButton() } }
        .modifier(Destinations(namespace: namespace))
    }

    /// No Continue Watching and no Next Up: a new user, or one who has
    /// finished everything.
    private func newcomer(_ home: Home) -> Bool {
        HiddenTitles.shared.visible(home.resume, on: .continueWatching).isEmpty
            && HiddenTitles.shared.visible(home.nextUp, on: .nextUp).isEmpty
    }

    /// Recently Added in full: its library, newest first.
    private func latestGrid(_ row: Row) -> AnyView? {
        guard let library = model.libraries.first(where: { $0.id == row.id }) else { return nil }
        return AnyView(LibraryPage(source: GridSource(library), sort: .dateAdded, descending: true))
    }

    /// Each kind once, in order.
    private func sections(_ home: Home) -> [HomeSection] {
        var seen = Set<HomeSection>()
        return home.order.filter { kind in
            let key = kind == .libraryButtons ? .smallLibraryTiles : kind
            return seen.insert(key).inserted
        }
    }
}

/// The hero: one slide a page, a swipe steps it, and it steps by itself
/// every 7 s unless Reduce Motion or Low Power Mode is on (spec §8.2).
struct Hero: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.accessibilityVoiceOverEnabled) private var voiceOver
    let items: [MediaItem]
    @State private var current: String?

    /// The room of a slide; the text sits over its lower part.
    private static let height: CGFloat = 520

    var body: some View {
        ScrollView(.horizontal) {
            LazyHStack(spacing: 0) {
                ForEach(items, id: \.id) { item in
                    ZStack(alignment: .bottomLeading) {
                        Picture(url: item.backdrop, corner: 0, symbol: nil)
                            .frame(height: Self.height)
                            .backgroundExtensionEffect()
                            // The picture and its mirrored copies take no
                            // touch: the copies of the next slide lay over
                            // this one's info button.
                            .allowsHitTesting(false)
                        HeroShade()
                        slide(item)
                    }
                    .containerRelativeFrame(.horizontal)
                }
            }
            .scrollTargetLayout()
        }
        .scrollTargetBehavior(.paging)
        .scrollPosition(id: $current)
        .scrollIndicators(.hidden)
        .frame(height: Self.height)
        .accessibilityIdentifier("hero")
        .task(id: "\(current ?? "")|\(stepping)") {
            // Restarts after each step, a swipe included.
            guard stepping else { return }
            try? await Task.sleep(for: .seconds(7))
            guard !Task.isCancelled, !ProcessInfo.processInfo.isLowPowerModeEnabled, items.count > 1 else { return }
            let index = items.firstIndex { $0.id == current } ?? 0
            withAnimation(.easeOut(duration: 0.5)) { current = items[(index + 1) % items.count].id }
        }
    }

    /// The title art or the title, the facts, the description, and the
    /// one white Play with More info beside it, on the page's gutter.
    private func slide(_ item: MediaItem) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            if let logo = item.logo, let url = URL(string: logo) {
                AsyncImage(url: url) { image in
                    image.resizable().scaledToFit()
                } placeholder: {
                    Text(item.name).font(.display(34))
                }
                .frame(maxWidth: 260, maxHeight: 90, alignment: .leading)
                .accessibilityLabel(item.name)
            } else {
                Text(item.name).font(.display(34)).lineLimit(2)
            }
            MetaChips(item: item)
            if !item.genres.isEmpty {
                Text(item.genres.prefix(3).joined(separator: " · "))
                    .font(.footnote).foregroundStyle(Color.ink.opacity(0.7)).lineLimit(1)
            }
            if let overview = item.overview {
                Text(overview).font(.subheadline).foregroundStyle(Color.ink.opacity(0.7)).lineLimit(2)
            }
            GlassEffectContainer {
                HStack(spacing: 12) {
                    Button(item.resumeSecs > 0 ? "Resume" : "Play", systemImage: "play.fill") { model.play(item) }
                        .buttonStyle(MainButtonStyle())
                    NavigationLink(value: Route.item(item)) {
                        Image(systemName: "info").glassDisc(Metrics.control)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("More info")
                    .accessibilityIdentifier("hero.info")
                    Spacer(minLength: 12)
                    counter(item)
                }
            }
            .padding(.top, 4)
        }
        .padding(.horizontal, Metrics.gutter)
        .padding(.bottom, 20)
        .frame(maxWidth: 560, alignment: .leading)
    }

    /// Which slide shows, at the right of the buttons (DESIGN.md, "The
    /// hero"): its number and a thin line of how far the row is.
    @ViewBuilder private func counter(_ item: MediaItem) -> some View {
        if items.count > 1, let index = items.firstIndex(where: { $0.id == item.id }) {
            VStack(alignment: .trailing, spacing: 6) {
                Text("\(index + 1) / \(items.count)")
                    .font(.caption.weight(.medium)).monospacedDigit().foregroundStyle(Color.ink.opacity(0.7))
                Capsule().fill(Color.ink.opacity(0.25))
                    .frame(width: 56, height: 3)
                    .overlay(alignment: .leading) {
                        Capsule().fill(Color.ink).frame(width: 56 * CGFloat(index + 1) / CGFloat(items.count))
                    }
            }
            .accessibilityHidden(true)
        }
    }

    /// The hero holds still under the player (full screen or small), with
    /// VoiceOver (a slide must not move under the cursor) and Reduce Motion.
    private var stepping: Bool {
        !reduceMotion && !voiceOver && model.playing == nil && !model.miniPlayer
    }
}

struct PosterRow: View {
    let title: String
    let items: [MediaItem]
    var seeAll: AnyView? = nil

    /// The posters a phone shows across: a row with more has "See all".
    static let shown = 3

    var body: some View {
        if !items.isEmpty {
            Shelf(title: title, seeAll: seeAll) {
                ForEach(items, id: \.id) { item in
                    CardLink(item: item) { PosterCard(item: item) }
                        .containerRelativeFrame(.horizontal) { width, _ in Metrics.cardWidth(width, minimum: 110) }
                }
            }
        }
    }
}

struct WideRow: View {
    let title: String
    let items: [MediaItem]
    var row: CardLink<WideCard>.Row? = nil
    var seeAll: AnyView? = nil

    /// A phone shows one wide card and the edge of the next; a row of more
    /// than two has "See all".
    static let shown = 2

    var body: some View {
        if !items.isEmpty {
            Shelf(title: title, seeAll: seeAll) {
                ForEach(items, id: \.id) { item in
                    CardLink(item: item, row: row) { WideCard(item: item) }
                        .containerRelativeFrame(.horizontal) { width, _ in
                            // A wide card: a peek of the next on a phone, more on an iPad.
                            width < 600 ? width * 0.6 : Metrics.cardWidth(width, minimum: 280)
                        }
                }
            }
        }
    }
}
