import BloomCore
import SwiftUI

/// A row of Seerr titles (a detail page's recommendations, the Discover row
/// of Search). A card opens the title's page on Seerr. Its menu has
/// Request…, which asks first: a request starts downloads.
struct SeerrRow: View {
    @Environment(\.openURL) private var openURL
    @Environment(AppModel.self) private var model
    let title: String
    let cards: [SeerrCard]
    @State private var asking: SeerrCard?

    var body: some View {
        if !cards.isEmpty {
            Shelf(title: title) {
                ForEach(cards, id: \.id) { card in
                    Button {
                        if let page = card.page, let url = URL(string: page) { openURL(url) }
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Picture(url: card.poster, corner: 12)
                                .aspectRatio(2 / 3, contentMode: .fit)
                                .overlay(alignment: .topTrailing) {
                                    if let status = Self.status(card.status) {
                                        Text(status)
                                            .font(.caption2.weight(.semibold))
                                            .padding(.horizontal, 8).padding(.vertical, 4)
                                            .glassEffect(.regular, in: .capsule)
                                            .padding(6)
                                    }
                                }
                            Text(card.title).font(.footnote).lineLimit(1)
                            Text(card.year ?? " ").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .buttonStyle(.plain)
                    .contextMenu {
                        if card.status == nil, card.jellyfinId == nil {
                            Button("Request…", systemImage: "plus.circle") { asking = card }
                        }
                        if let page = card.page, let url = URL(string: page) {
                            Button("Open on Seerr", systemImage: "safari") { openURL(url) }
                        }
                    }
                    .containerRelativeFrame(.horizontal) { width, _ in Metrics.cardWidth(width, minimum: 110) }
                    .accessibilityLabel([card.title, card.year, Self.status(card.status)].compactMap { $0 }.joined(separator: ", "))
                    .accessibilityHint("Opens it on Seerr")
                }
            }
            .confirmationDialog("Request \(asking?.title ?? "")?", isPresented: Binding(get: { asking != nil }, set: { if !$0 { asking = nil } }),
                                titleVisibility: .visible, presenting: asking) { card in
                Button(card.mediaType == .tv ? "Request every season" : "Request") {
                    Task { await model.seerrRequest(tmdbId: card.id, mediaType: card.mediaType, title: card.title) }
                }
            } message: { _ in
                Text("Seerr asks the server's tools to download it.")
            }
        }
    }

    /// Seerr's media status, in words; none for a title nobody asked for.
    static func status(_ status: Int32?) -> String? {
        switch status {
        case 2: "Requested"
        case 3: "Processing"
        case 4: "Partly available"
        case 5: "Available"
        default: nil
        }
    }
}

/// Where a list shows, for the hidden-content filter (the Mac's `Surface`):
/// libraries, the "latest" rows, the hero and a detail page's rows are
/// `library`.
enum HideSurface {
    case library, search, nextUp, continueWatching

    /// The plugin's scope of the surface alone.
    var scope: HideScope {
        switch self {
        case .library: .library
        case .search: .search
        case .nextUp: .nextUp
        case .continueWatching: .continueWatching
        }
    }
}

/// The hidden titles the app leaves out of the lists it holds (the Mac's
/// `HideSet` and `drop_hidden_from_page`). The plugin filters the server's
/// answers, but a page that does not load again (Search, a library, a
/// detail page's rows) would keep a title just hidden: a list goes through
/// `visible` and loses it at once. Read once per session, at the first look
/// with the feature on, and again after a Hide or a "Show again".
@Observable
final class HiddenTitles {
    static let shared = HiddenTitles()

    private(set) var filter: HiddenFilter?
    /// The server the filter was read from: a switch of profile reads again.
    @ObservationIgnored private var owner: ObjectIdentifier?
    @ObservationIgnored private var loading = false

    /// The items a list shows on a surface.
    func visible(_ items: [MediaItem], on surface: HideSurface) -> [MediaItem] {
        // `filter` is read before `active`: a list drawn before the first
        // read must observe it, or the read changes nothing on screen and
        // the hidden title goes at the list's next change (2026-11-14: a
        // library grid lost a card on its in-place refresh).
        let filter = self.filter
        guard active else { return items }
        guard let filter, !(filter.byItem.isEmpty && filter.bySeries.isEmpty) else { return items }
        return items.filter { !Self.hidden($0, on: surface, in: filter) }
    }

    func isHidden(_ item: MediaItem, on surface: HideSurface) -> Bool {
        let filter = self.filter // observed first, as in `visible`
        guard active, let filter else { return false }
        return Self.hidden(item, on: surface, in: filter)
    }

    /// Reads the file's filter for the server in use.
    func load() async {
        loading = true
        defer { loading = false }
        guard let model = AppModel.current, let server = model.server,
              model.enhanced?.hiddenContent == true else { return }
        let id = ObjectIdentifier(server)
        let read = try? await server.hiddenFilter()
        guard model.server.map(ObjectIdentifier.init) == id else { return }
        // A failed read filters nothing, and is not tried at each frame.
        owner = id
        filter = read
    }

    /// A title hidden just now, before the file is read again.
    func hid(_ item: MediaItem, scope: HideScope) {
        guard let server = AppModel.current?.server else { return }
        let id = ObjectIdentifier(server)
        // Until the file is read, the plugin's defaults.
        var next = owner == id ? filter ?? Self.defaults : Self.defaults
        let key = hiddenId(id: item.id)
        next.byItem[key, default: []].append(scope)
        if item.kind == .series { next.bySeries[key, default: []].append(scope) }
        owner = id
        filter = next
    }

    private static let defaults = HiddenFilter(enabled: true, filterLibrary: true, filterSearch: false, filterNextUp: true,
                                               filterContinueWatching: true, byItem: [:], bySeries: [:])

    /// The feature is on and the filter is this session's; a first look
    /// reads the file.
    private var active: Bool {
        guard let model = AppModel.current, let server = model.server,
              model.enhanced?.hiddenContent == true else { return false }
        if owner != ObjectIdentifier(server) {
            if !loading {
                loading = true
                Task { await load() }
            }
            return false
        }
        return true
    }

    /// `IsHiddenById`: the item, its series, or the item as a series.
    private static func hidden(_ item: MediaItem, on surface: HideSurface, in filter: HiddenFilter) -> Bool {
        let any = { (scopes: [HideScope]?) in scopes?.contains { applies($0, surface, filter) } ?? false }
        let id = hiddenId(id: item.id)
        return any(filter.byItem[id])
            || item.seriesId.map { any(filter.bySeries[hiddenId(id: $0)]) } ?? false
            || any(filter.bySeries[id])
    }

    /// `ScopeAppliesToSurface`: the surface must filter, and the scope must
    /// be that surface, global, or the home sections on the two home rows.
    private static func applies(_ scope: HideScope, _ surface: HideSurface, _ filter: HiddenFilter) -> Bool {
        let filters = switch surface {
        case .library: filter.filterLibrary
        case .search: filter.filterSearch
        case .nextUp: filter.filterNextUp
        case .continueWatching: filter.filterContinueWatching
        }
        guard filter.enabled, filters else { return false }
        return scope == surface.scope || scope == .global
            || (scope == .homeSections && (surface == .nextUp || surface == .continueWatching))
    }
}

/// The Jellyfin Enhanced quality tags of a card ("4K", "Dolby Vision",
/// "ATMOS 7.1"), stacked at its top left as the plugin shows them: quiet
/// pills in the one ink, no colour per kind. VoiceOver reads them as the
/// card's value instead.
struct QualityTags: View {
    let labels: [String]
    @ScaledMetric(relativeTo: .caption2) private var size: CGFloat = 10

    var body: some View {
        ZStack(alignment: .topLeading) {
            Color.clear.frame(width: 0, height: 0)
            if !labels.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    // Three at most: more would cover a phone's poster.
                    ForEach(labels.prefix(3), id: \.self) { label in
                        Text(label)
                            .font(.system(size: size, weight: .bold))
                            .lineLimit(1)
                            .foregroundStyle(Color.ink)
                            .padding(.horizontal, 5).padding(.vertical, 1)
                            .background(.black.opacity(0.7), in: .rect(cornerRadius: 5))
                            .overlay(RoundedRectangle(cornerRadius: 5).stroke(Color.ink.opacity(0.2), lineWidth: 0.5))
                    }
                }
                .padding(6)
            }
        }
        .accessibilityHidden(true)
    }
}

extension AppModel {
    /// Puts the quality tags on the cards as the plugin and this device say
    /// (Settings › Jellyfin Enhanced): this device's choice, else the
    /// user's plugin setting. A change loads the home rows again, because
    /// only a query with the tags on brings the streams they come from.
    func applyQualityTags() async {
        let on = Preferences.shared.qualityTags ?? enhancedServer?.qualityTags ?? false
        if setQualityTags(on: on) { await refresh() }
    }
}

/// "Hide…" for a title: where it is hidden (the plugin's scopes).
struct HideMenu: View {
    @Environment(AppModel.self) private var model
    let item: MediaItem

    var body: some View {
        Menu {
            Button("Everywhere") { hide(.global) }
            Button("From the home rows") { hide(.homeSections) }
            Button("From Continue Watching") { hide(.continueWatching) }
            Button("From Next Up") { hide(.nextUp) }
        } label: {
            Label("Hide…", systemImage: "eye.slash")
        }
    }

    private func hide(_ scope: HideScope) {
        Task {
            do {
                try await model.server?.hide(itemId: item.id, scope: scope)
                // The lists on screen lose it now; the file follows.
                HiddenTitles.shared.hid(item, scope: scope)
                model.say("Hidden. Settings › Hidden content brings it back.")
                await model.refresh()
                await HiddenTitles.shared.load()
            } catch {
                model.say("Not hidden: \(Words.error(error))")
            }
        }
    }
}

/// Settings › Hidden content: what the user hid, and a swipe to bring it back.
struct HiddenSettings: View {
    @Environment(AppModel.self) private var model
    @State private var entries: [HiddenEntry]?

    var body: some View {
        List {
            if let entries {
                ForEach(entries, id: \.key) { entry in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.title)
                        Text(Self.scope(entry.scope)).font(.caption).foregroundStyle(.secondary)
                    }
                    .swipeActions {
                        Button("Show again") { Task { await unhide(entry) } }.tint(.gray)
                    }
                }
            }
        }
        .overlay {
            if entries == nil {
                ProgressView()
            } else if entries?.isEmpty == true {
                ContentUnavailableView("Nothing is hidden", systemImage: "eye",
                                       description: Text("Hide a title from its menu; it shows here."))
            }
        }
        .navigationTitle("Hidden content")
        .task { entries = (try? await model.server?.hidden()) ?? [] }
    }

    static func scope(_ scope: HideScope) -> String {
        switch scope {
        case .nextUp: "Next Up only"
        case .continueWatching: "Continue Watching only"
        case .homeSections: "Home rows only"
        default: "Everywhere"
        }
    }

    private func unhide(_ entry: HiddenEntry) async {
        entries?.removeAll { $0.key == entry.key }
        do {
            try await model.server?.unhide(keyOrId: entry.key)
            await HiddenTitles.shared.load()
            await model.refresh()
        } catch {
            model.say("Not shown again: \(Words.error(error))")
            entries = (try? await model.server?.hidden()) ?? []
        }
    }
}

/// The bookmarks of the item playing: Add here, jump, delete (the web
/// client sees the same ones).
struct BookmarksSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let model: PlayerModel
    @State private var list: [Bookmark]?
    @State private var label = ""

    var body: some View {
        NavigationStack {
            List {
                Section {
                    HStack {
                        TextField("Label (optional)", text: $label)
                        Button("Add at \(Words.clock(model.state.position))") { Task { await add() } }
                            .accessibilityIdentifier("bookmark.add")
                    }
                }
                Section {
                    if let list {
                        ForEach(list, id: \.id) { bookmark in
                            Button {
                                model.seek(to: bookmark.secs)
                                dismiss()
                            } label: {
                                HStack {
                                    Text(Words.clock(bookmark.secs)).monospacedDigit().foregroundStyle(.secondary)
                                    Text(bookmark.label.isEmpty ? "Bookmark" : bookmark.label)
                                }
                            }
                            .swipeActions {
                                Button("Delete", role: .destructive) { Task { await delete(bookmark) } }
                            }
                        }
                        if list.isEmpty {
                            Text("No bookmark in this title yet.").foregroundStyle(.secondary)
                        }
                    } else {
                        ProgressView()
                    }
                }
            }
            .navigationTitle("Bookmarks")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .task { await load() }
        }
        .presentationDetents([.medium, .large])
    }

    private func load() async {
        guard let id = model.info?.itemId else { return }
        list = (try? await app.server?.bookmarks(itemId: id)) ?? []
    }

    private func add() async {
        guard let id = model.info?.itemId, let server = app.server else { return }
        do {
            _ = try await server.addBookmark(itemId: id, secs: model.state.position, label: label)
            label = ""
            await load()
        } catch {
            app.say("Not added: \(Words.error(error))")
        }
    }

    private func delete(_ bookmark: Bookmark) async {
        list?.removeAll { $0.id == bookmark.id }
        try? await app.server?.deleteBookmark(id: bookmark.id)
    }
}
