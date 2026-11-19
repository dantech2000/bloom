import BloomCore
import SwiftUI

/// The spacing the pages share.
enum Metrics {
    /// The margin at a page's sides.
    static let gutter: CGFloat = 16
    /// Between two cards of a row or a grid.
    static let cardSpacing: CGFloat = 14
    /// Between a row's title and its cards.
    static let shelfSpacing: CGFloat = 10
    /// Between two rows of a grid.
    static let gridRowSpacing: CGFloat = 18
    /// Between two rows of Home or a detail page.
    static let rowSpacing: CGFloat = 30
    /// A field or a main button on a phone (DESIGN.md).
    static let control: CGFloat = 50
    /// The corners of a control, a field or a button (DESIGN.md, "Shapes").
    static let controlCorner: CGFloat = 14
    /// A round icon button.
    static let disc: CGFloat = 48
    /// A grid of posters: as many as fit at 105 points or more.
    static let posterGrid = [GridItem(.adaptive(minimum: 105, maximum: 180), spacing: cardSpacing, alignment: .top)]

    /// As many cards as fit at `minimum` points or more, three at least
    /// (a phone shows three posters across; an iPad shows more, spike S3).
    static func cardWidth(_ width: CGFloat, minimum: CGFloat, spacing: CGFloat = cardSpacing) -> CGFloat {
        let count = max(3, Int((width + spacing) / (minimum + spacing)))
        return (width - spacing * CGFloat(count - 1)) / CGFloat(count)
    }
}

extension View {
    /// The one white primary button of a page (DESIGN.md).
    func primaryButton() -> some View {
        buttonStyle(.glassProminent).buttonBorderShape(.roundedRectangle(radius: Metrics.controlCorner))
            .foregroundStyle(.black)
    }

    /// A quiet glass button with a label (DESIGN.md, "Buttons"): the
    /// second action beside a primary one, at the control radius.
    func quietButton() -> some View {
        buttonStyle(.glass).buttonBorderShape(.roundedRectangle(radius: Metrics.controlCorner))
    }

    /// An icon on a round pane of glass: the quiet actions beside a
    /// page's main one (DESIGN.md, "Icon buttons"). 44 points or more.
    func glassDisc(_ size: CGFloat = Metrics.disc) -> some View {
        font(.body.weight(.medium))
            .frame(width: size, height: size)
            .glassEffect(.regular.interactive(), in: .circle)
            .contentShape(.circle)
    }

    /// A list on the page's ground, not the system's black: Library and
    /// Downloads sit on the same Theatre Black as Home.
    func pageList() -> some View {
        scrollContentBackground(.hidden).background(Color.page)
    }
}

/// The main action of a page at the phone's size (DESIGN.md, "The Phone
/// Reads at 17 Rule"): 50 points tall, the white fill, the label in the
/// dark ink. `wide` takes the width of its column.
struct MainButtonStyle: ButtonStyle {
    var wide = false
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.headline)
            .foregroundStyle(Color.inkOnWhite)
            .padding(.horizontal, 24)
            .frame(maxWidth: wide ? .infinity : nil, minHeight: Metrics.control)
            .background(Color.ink, in: .rect(cornerRadius: Metrics.controlCorner))
            .contentShape(.rect(cornerRadius: Metrics.controlCorner))
            .opacity(configuration.isPressed ? 0.88 : 1)
            .opacity(enabled ? 1 : 0.5)
    }
}

/// A button whose label is an icon on a glass disc (`glassDisc`).
struct DiscButtonStyle: ButtonStyle {
    var size = Metrics.disc

    func makeBody(configuration: Configuration) -> some View {
        // A small disc keeps a 44-point target around it.
        configuration.label.labelStyle(.iconOnly).glassDisc(size)
            .frame(minWidth: 44, minHeight: 44).contentShape(.rect)
    }
}

/// A quiet pill for a fact: the year, the age rating, the length, the
/// rating with its star (DESIGN.md, "Chips"). Never a colour.
struct Chip: View {
    let text: String
    var symbol: String? = nil
    /// An outline alone, for a tag of the file (4K, HDR).
    var outlined = false

    var body: some View {
        HStack(spacing: 4) {
            if let symbol { Image(systemName: symbol).imageScale(.small) }
            Text(text)
        }
        .font(.caption.weight(.medium))
        .lineLimit(1)
        .padding(.horizontal, 9).padding(.vertical, 5)
        .background(Color.ink.opacity(outlined ? 0 : 0.12), in: .capsule)
        .overlay { if outlined { Capsule().strokeBorder(Color.ink.opacity(0.3), lineWidth: 1) } }
    }
}

/// The facts of a title as chips, on as many lines as they need.
struct MetaChips: View {
    let item: MediaItem
    /// The tags of the file after the facts (the detail page).
    var tags = false

    var body: some View {
        Flow(spacing: 6) {
            // A season's number is no episode code ("E1" on Season 1).
            if item.kind == .episode, let code = item.episodeCode { Chip(text: code) }
            if let rating = item.communityRating {
                Chip(text: rating.formatted(.number.precision(.fractionLength(1))), symbol: "star.fill")
                    .accessibilityLabel("Rated \(rating.formatted(.number.precision(.fractionLength(1))))")
            }
            if let critics = item.criticRating { Chip(text: "Critics \(Int(critics))%") }
            if let year = item.year { Chip(text: year) }
            if let age = item.officialRating { Chip(text: age) }
            if let runtime = Words.runtime(item.runtimeSecs) { Chip(text: runtime) }
            if tags {
                ForEach(item.qualityTags, id: \.self) { Chip(text: $0, outlined: true) }
            }
        }
    }
}

/// Lays its views in a line and starts a new line when one is full.
struct Flow: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        place(subviews, in: proposal.width ?? .infinity).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        for (subview, point) in zip(subviews, place(subviews, in: bounds.width).points) {
            subview.place(at: CGPoint(x: bounds.minX + point.x, y: bounds.minY + point.y), proposal: .unspecified)
        }
    }

    private func place(_ subviews: Subviews, in width: CGFloat) -> (points: [CGPoint], size: CGSize) {
        var points: [CGPoint] = []
        var (x, y, line, widest) = (CGFloat(0), CGFloat(0), CGFloat(0), CGFloat(0))
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                x = 0
                y += line + spacing
                line = 0
            }
            points.append(CGPoint(x: x, y: y))
            x += size.width + spacing
            line = max(line, size.height)
            widest = max(widest, x - spacing)
        }
        return (points, CGSize(width: widest, height: y + line))
    }
}

/// The shade over the lower part of a backdrop (DESIGN.md, "Hero Shade"):
/// the text over it stays readable, and the picture ends in the page's
/// ground, not at a hard line.
struct HeroShade: View {
    var body: some View {
        LinearGradient(stops: [.init(color: .clear, location: 0.3),
                               .init(color: Color.page.opacity(0.55), location: 0.68),
                               .init(color: Color.page, location: 1)],
                       startPoint: .top, endPoint: .bottom)
            .allowsHitTesting(false)
    }
}

extension MediaItem {
    /// Played and favourite apply to it: a title, an episode, a series or a
    /// season, not a library or a folder.
    var markable: Bool { playable || isSeries || kind == .season }

    /// A card's title: an episode's show, else the name (the Mac's
    /// `card_titles`).
    var cardTitle: String { kind == .episode ? seriesName ?? name : name }

    /// The line under it: an episode's code and name, else the year.
    var subtitle: String {
        kind == .episode ? [episodeCode, name].compactMap { $0 }.joined(separator: " · ") : year ?? " "
    }

    /// Its wide picture: an episode's show (Continue Watching, Next Up),
    /// as on the Mac's home rows; a library has its wide picture as its
    /// primary (the Mac's tile).
    var wideArt: String? { showWide ?? wide ?? backdrop ?? poster }

    /// The glyph of a library, for a tile without a picture.
    var librarySymbol: String {
        switch collectionType {
        case .tvShows: "tv"
        case .music, .musicVideos: "music.note"
        case .boxSets: "square.stack"
        case .books: "book"
        case .photos, .homeVideos: "photo"
        case .playlists: "music.note.list"
        case .folders: "folder"
        default: "film"
        }
    }
}

/// A titled row of cards that scrolls sideways: the rows of Home, More
/// Like This, Cast & Crew, the rows of Seerr.
struct Shelf<Content: View>: View {
    let title: String
    @ViewBuilder let content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: Metrics.shelfSpacing) {
            Text(title).font(.title3.weight(.semibold)).padding(.horizontal, Metrics.gutter)
            ScrollView(.horizontal) {
                LazyHStack(alignment: .top, spacing: Metrics.cardSpacing) { content() }
                    .scrollTargetLayout()
            }
            .contentMargins(.horizontal, Metrics.gutter, for: .scrollContent)
            .scrollTargetBehavior(.viewAligned)
            .scrollIndicators(.hidden)
        }
    }
}

/// A picture from the server, filling a frame its container sizes: a
/// fill-scaled picture must not widen its parent (spike S3). Until it
/// loads the frame is a quiet pane; a picture that is missing or fails
/// leaves a glyph there, and one that arrives fades in.
struct Picture: View {
    let url: String?
    var corner: CGFloat = 12
    /// The glyph of a frame with no picture; none on a backdrop.
    var symbol: String? = "film"

    var body: some View {
        Color.ink.opacity(0.06)
            .overlay {
                if let url, let address = URL(string: url) {
                    AsyncImage(url: address, transaction: Transaction(animation: .easeOut(duration: 0.2))) { phase in
                        switch phase {
                        case .success(let image): image.resizable().scaledToFill().transition(.opacity)
                        case .failure: glyph
                        default: Color.clear
                        }
                    }
                } else {
                    glyph
                }
            }
            .clipShape(.rect(cornerRadius: corner))
            // A dark poster keeps its shape on the dark page.
            .overlay {
                if corner > 0 { RoundedRectangle(cornerRadius: corner).strokeBorder(Color.ink.opacity(0.08), lineWidth: 0.5) }
            }
    }

    @ViewBuilder private var glyph: some View {
        if let symbol {
            Image(systemName: symbol).font(.title3).foregroundStyle(Color.ink.opacity(0.28)).accessibilityHidden(true)
        }
    }
}

/// A poster: the width from the container, the height from the 2:3 picture.
struct PosterCard: View {
    let item: MediaItem

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // An episode shows its show's poster (Recently Added), as on
            // the Mac's poster cards.
            Picture(url: item.showPoster ?? item.poster)
                .aspectRatio(2 / 3, contentMode: .fit)
                .overlay(alignment: .topLeading) { QualityTags(labels: item.qualityTags) }
                .overlay(alignment: .topTrailing) { Badge(item: item) }
                .overlay(alignment: .bottom) { ProgressLine(progress: item.progress) }
            Text(item.cardTitle).font(.footnote.weight(.medium)).lineLimit(1)
            Text(item.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(item.qualityTags.joined(separator: ", "))
        .accessibilityIdentifier(item.kind == .collectionFolder ? "library.\(item.id)" : "card.\(item.id)")
    }
}

/// A wide card for Continue Watching and Next Up: the still, the series and
/// the episode.
struct WideCard: View {
    let item: MediaItem

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picture(url: item.wideArt, symbol: item.kind == .collectionFolder ? item.librarySymbol : "film")
                .aspectRatio(16 / 9, contentMode: .fit)
                .overlay(alignment: .topLeading) { QualityTags(labels: item.qualityTags) }
                .overlay(alignment: .bottom) { ProgressLine(progress: item.progress) }
            Text(item.cardTitle).font(.footnote.weight(.medium)).lineLimit(1)
            // A library has its name alone: no empty line under it.
            if item.kind != .collectionFolder {
                Text(item.subtitle)
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(item.qualityTags.joined(separator: ", "))
        .accessibilityIdentifier(item.kind == .collectionFolder ? "library.\(item.id)" : "card.\(item.id)")
    }
}

/// Watched, or the count of unplayed episodes: a quiet disc, no colour.
struct Badge: View {
    let item: MediaItem
    /// The disc grows with the text size.
    @ScaledMetric(relativeTo: .caption2) private var disc: CGFloat = 20

    var body: some View {
        Group {
            if let count = item.unplayedCount, count > 0, item.isSeries {
                Text("\(count)").font(.caption2.weight(.semibold)).padding(.horizontal, 6).frame(minWidth: disc, minHeight: disc)
            } else if item.played {
                Image(systemName: "checkmark").font(.caption2.weight(.bold)).frame(width: disc, height: disc)
            }
        }
        .background(.black.opacity(0.55), in: .capsule)
        .padding(6)
    }
}

/// How far an item is played, as a thin line over the bottom of its picture.
struct ProgressLine: View {
    let progress: Float?

    var body: some View {
        if let progress, progress > 0 {
            Capsule().fill(.black.opacity(0.5))
                .overlay(alignment: .leading) {
                    GeometryReader { box in
                        Capsule().fill(Color.ink).frame(width: max(4, box.size.width * CGFloat(min(progress, 1))))
                    }
                }
                .frame(height: 4)
                .padding(8)
                .accessibilityHidden(true)
        }
    }
}

/// A poster-shaped pane with two bars under it: a card of a grid that
/// still loads.
struct PosterSkeleton: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picture(url: nil, symbol: nil).aspectRatio(2 / 3, contentMode: .fit)
            Capsule().fill(Color.ink.opacity(0.08)).frame(height: 10).padding(.trailing, 18).padding(.vertical, 3)
            Capsule().fill(Color.ink.opacity(0.05)).frame(width: 36, height: 9).padding(.vertical, 3)
        }
        .accessibilityHidden(true)
    }
}

/// A card that opens its page, with the card menu on a long press.
struct CardLink<Card: View>: View {
    /// The home row a card is in, for its "Remove from" entry.
    enum Row { case continueWatching, nextUp }

    @Environment(AppModel.self) private var model
    let item: MediaItem
    var row: Row? = nil
    @Environment(\.zoomSpace) private var zoomSpace
    /// The collection whose page shows the card, for "Remove from collection".
    @Environment(\.inCollection) private var inCollection
    @Environment(\.openURL) private var openURL
    @ViewBuilder let card: () -> Card

    var body: some View {
        NavigationLink(value: Route.item(item)) { card() }
            .buttonStyle(.plain)
            .modifier(ZoomSource(id: item.id, space: zoomSpace))
            .contextMenu {
                if item.playable || item.isSeries {
                    Button("Play", systemImage: "play.fill") { model.play(item) }
                }
                // Not on a library or a folder: Mark played would mark all of it.
                if item.markable {
                    Button(item.played ? "Mark unplayed" : "Mark played", systemImage: item.played ? "arrow.uturn.backward.circle" : "checkmark.circle") {
                        Task { await model.togglePlayed(item) }
                    }
                    Button(item.favorite ? "Remove from favorites" : "Add to favorites", systemImage: item.favorite ? "heart.slash" : "heart") {
                        Task { await model.toggleFavorite(item) }
                    }
                }
                if ListKind.playlist.takes(item) || (model.prefs.canCollect && ListKind.collection.takes(item)) {
                    Divider()
                    if ListKind.playlist.takes(item) {
                        Button("Add to playlist…", systemImage: "text.badge.plus") { model.addRequest = AddRequest(item: item, kind: .playlist) }
                    }
                    if model.prefs.canCollect, ListKind.collection.takes(item) {
                        Button("Add to collection…", systemImage: "square.stack.3d.up") { model.addRequest = AddRequest(item: item, kind: .collection) }
                    }
                }
                if model.enhanced?.hiddenContent == true {
                    HideMenu(item: item)
                }
                if let group = SyncBridge.shared.group, item.playable {
                    Button("Play next in the group", systemImage: "text.line.first.and.arrowtriangle.forward") {
                        group.user(intent: .enqueue(itemIds: [item.id], next: true))
                        model.say("Next in the group: \(item.name)")
                    }
                    Button("Add to the group's queue", systemImage: "text.badge.plus") {
                        group.user(intent: .enqueue(itemIds: [item.id], next: false))
                        model.say("Added to the group: \(item.name)")
                    }
                }
                if item.playable, model.prefs.canDownload {
                    if Downloads.shared.entry(item.id) == nil {
                        Button("Download", systemImage: "arrow.down.circle") { DownloadAction.start(item, model: model) }
                    } else {
                        Button("Remove the download", systemImage: "trash", role: .destructive) { Downloads.shared.remove(item.id) }
                    }
                }
                if let web = item.webUrl, let url = URL(string: web) {
                    Button("Open in Jellyfin Web", systemImage: "safari") { openURL(url) }
                }
                if let collection = inCollection, model.prefs.canCollect {
                    Button("Remove from collection", systemImage: "minus.circle", role: .destructive) {
                        Task {
                            guard let server = model.server else { return }
                            do {
                                try await server.removeFromList(kind: .collection, listId: collection, ids: [item.id])
                                model.say("Removed from the collection")
                                model.collectionChanged += 1
                            } catch {
                                model.say("Not removed: \(Words.error(error))")
                            }
                        }
                    }
                }
                if let row {
                    Divider()
                    Button(row == .nextUp ? "Remove from Next Up" : "Remove from Continue Watching", systemImage: "eye.slash", role: .destructive) {
                        Task { await model.hideFromRow(item, nextUp: row == .nextUp) }
                    }
                }
            } preview: {
                Picture(url: item.showPoster ?? item.poster ?? item.wide, corner: 0).frame(width: 260, height: 390)
            }
    }
}
