import BloomCore
import SwiftUI

/// The searches of this device, newest first, kept in the user defaults:
/// eight at most, and a search that grows from an older one replaces it
/// ("bok", then "bokma").
struct BrowseRecentSearches {
    private static let key = "browse.recentSearches"
    private static let most = 8

    private(set) var list: [String] = UserDefaults.standard.stringArray(forKey: key) ?? []

    mutating func add(_ text: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard text.count >= 2 else { return }
        let lower = text.lowercased()
        list.removeAll { lower.hasPrefix($0.lowercased()) || $0.lowercased() == lower }
        // An older, longer search that holds this one stays.
        list.insert(text, at: 0)
        list = Array(list.prefix(Self.most))
        UserDefaults.standard.set(list, forKey: Self.key)
    }

    mutating func remove(_ text: String) {
        list.removeAll { $0 == text }
        UserDefaults.standard.set(list, forKey: Self.key)
    }

    mutating func clear() {
        list = []
        UserDefaults.standard.removeObject(forKey: Self.key)
    }
}

/// The Search page before a key: the recent searches, then genres to
/// start from.
struct BrowseSearchStart: View {
    @Environment(AppModel.self) private var model
    @Binding var recent: BrowseRecentSearches
    /// Puts a recent search back in the field.
    let pick: (String) -> Void
    @State private var genres: [String] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            if !recent.list.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(alignment: .firstTextBaseline) {
                        Text("Recent").font(.title3.weight(.semibold)).accessibilityAddTraits(.isHeader)
                        Spacer()
                        Button("Clear") { withAnimation(.easeOut(duration: 0.15)) { recent.clear() } }
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(Color.ink.opacity(0.7))
                            .frame(minHeight: 44)
                            .accessibilityLabel("Clear recent searches")
                            .accessibilityIdentifier("search.recent.clear")
                    }
                    ForEach(recent.list, id: \.self) { text in
                        HStack(spacing: 12) {
                            Button { pick(text) } label: {
                                HStack(spacing: 12) {
                                    Image(systemName: "clock.arrow.circlepath").foregroundStyle(Color.ink.opacity(0.5))
                                        .accessibilityHidden(true)
                                    Text(text).foregroundStyle(Color.ink)
                                    Spacer(minLength: 0)
                                }
                                .frame(minHeight: 44)
                                .contentShape(.rect)
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("search.recent.\(text)")
                            Button { withAnimation(.easeOut(duration: 0.15)) { recent.remove(text) } } label: {
                                Image(systemName: "xmark").imageScale(.small).foregroundStyle(Color.ink.opacity(0.5))
                                    .frame(width: 44, height: 44).contentShape(.rect)
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Remove \(text)")
                        }
                        Divider().overlay(Color.ink.opacity(0.08))
                    }
                }
            }
            if !genres.isEmpty {
                VStack(alignment: .leading, spacing: 12) {
                    Text("Browse by Genre").font(.title3.weight(.semibold)).accessibilityAddTraits(.isHeader)
                    Flow(spacing: 10) {
                        ForEach(genres, id: \.self) { BrowseGenrePill(genre: $0) }
                    }
                }
            }
            if recent.list.isEmpty, genres.isEmpty {
                ContentUnavailableView("Search your libraries", systemImage: "magnifyingglass",
                                       description: Text("Type a title or a name."))
                    .padding(.top, 80)
            }
        }
        .padding(.horizontal, Metrics.gutter)
        .padding(.vertical, 12)
        .task {
            guard genres.isEmpty, let server = model.server else { return }
            genres = (try? await server.filterChoices(libraryId: nil, collectionType: nil))?.genres ?? []
        }
    }
}

/// The people a search found, as round photos over their names.
struct BrowsePeopleRow: View {
    let people: [PersonCard]

    var body: some View {
        if !people.isEmpty {
            Shelf(title: "People") {
                ForEach(people, id: \.id) { person in
                    NavigationLink(value: Route.person(id: person.id, name: person.name)) {
                        VStack(spacing: 6) {
                            Picture(url: person.image, corner: 44, symbol: "person.fill").frame(width: 88, height: 88)
                            Text(person.name).font(.caption.weight(.medium)).lineLimit(2).multilineTextAlignment(.center)
                        }
                        .frame(width: 96)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("person.\(person.id)")
                }
            }
            .padding(.top, 8)
        }
    }
}
