import BloomCore
import SwiftUI

/// What a library's grid is narrowed to beside its Show filter: one genre,
/// one decade, a lowest community rating, one audio and one subtitle
/// language (Jellyfin 12.0). Empty shows everything.
struct BrowseFilters: Hashable {
    var genre: String?
    /// The decade's first year: 1990 for the 1990s.
    var decade: Int?
    var minRating: Float?
    var audioLanguage: Language?
    var subtitleLanguage: Language?

    var active: Bool {
        genre != nil || decade != nil || minRating != nil || audioLanguage != nil || subtitleLanguage != nil
    }

    var key: String {
        "\(genre ?? "")|\(decade ?? 0)|\(minRating ?? 0)|\(audioLanguage?.code ?? "")|\(subtitleLanguage?.code ?? "")"
    }

    /// The years of the decade the server knows titles of (all ten when
    /// the choices have not come).
    func years(_ known: [Int32]) -> [Int32] {
        guard let decade else { return [] }
        let all = (0..<10).map { Int32(decade + $0) }
        let held = all.filter(known.contains)
        return held.isEmpty ? all : held
    }

    static let ratings: [Float] = [9, 8, 7, 6, 5]

    static func ratingTitle(_ rating: Float) -> String { "\(Int(rating))+" }

    /// The decades of a library's years, newest first.
    static func decades(_ years: [Int32]) -> [Int] {
        Array(Set(years.map { Int($0) / 10 * 10 })).sorted(by: >)
    }
}

/// The genre, decade, rating and track language pickers of a grid's Filter
/// menu, each a submenu. A language submenu shows when the server lists
/// languages (Jellyfin 12.0 and later).
struct BrowseFilterMenus: View {
    @Binding var narrow: BrowseFilters
    let choices: FilterChoices?
    var languages: LanguageChoices? = nil
    /// A genre's own grid: its genre is the page, not a filter.
    var genreFixed = false

    // Submenus of buttons, not pickers: a picker in a submenu of the
    // toolbar's menu showed its entries disabled.
    var body: some View {
        if !genreFixed {
            Menu {
                entry("Any genre", on: narrow.genre == nil) { narrow.genre = nil }
                ForEach(choices?.genres ?? [], id: \.self) { genre in
                    entry(genre, on: narrow.genre == genre) { narrow.genre = genre }
                }
            } label: {
                Label("Genre", systemImage: "theatermasks")
                if let genre = narrow.genre { Text(genre) }
            }
        }
        Menu {
            entry("Any year", on: narrow.decade == nil) { narrow.decade = nil }
            ForEach(BrowseFilters.decades(choices?.years ?? []), id: \.self) { decade in
                entry("\(String(decade))s", on: narrow.decade == decade) { narrow.decade = decade }
            }
        } label: {
            Label("Year", systemImage: "calendar")
            if let decade = narrow.decade { Text("\(String(decade))s") }
        }
        Menu {
            entry("Any rating", on: narrow.minRating == nil) { narrow.minRating = nil }
            ForEach(BrowseFilters.ratings, id: \.self) { rating in
                entry("\(BrowseFilters.ratingTitle(rating)) stars", on: narrow.minRating == rating) { narrow.minRating = rating }
            }
        } label: {
            Label("Rating", systemImage: "star")
            if let rating = narrow.minRating { Text("\(BrowseFilters.ratingTitle(rating)) stars") }
        }
        if !(languages?.audio ?? []).isEmpty || narrow.audioLanguage != nil {
            Menu {
                entry("Any language", on: narrow.audioLanguage == nil) { narrow.audioLanguage = nil }
                ForEach(languages?.audio ?? [], id: \.code) { language in
                    entry(language.name, on: narrow.audioLanguage?.code == language.code) { narrow.audioLanguage = language }
                }
            } label: {
                Label("Audio language", systemImage: "waveform")
                if let language = narrow.audioLanguage { Text(language.name) }
            }
            .accessibilityIdentifier("filter.audioLanguage")
        }
        if !(languages?.subtitles ?? []).isEmpty || narrow.subtitleLanguage != nil {
            Menu {
                entry("Any language", on: narrow.subtitleLanguage == nil) { narrow.subtitleLanguage = nil }
                ForEach(languages?.subtitles ?? [], id: \.code) { language in
                    entry(language.name, on: narrow.subtitleLanguage?.code == language.code) { narrow.subtitleLanguage = language }
                }
            } label: {
                Label("Subtitle language", systemImage: "captions.bubble")
                if let language = narrow.subtitleLanguage { Text(language.name) }
            }
            .accessibilityIdentifier("filter.subtitleLanguage")
        }
    }

    /// A choice with a tick when it is the one in use.
    private func entry(_ title: String, on: Bool, pick: @escaping () -> Void) -> some View {
        Button(action: pick) {
            if on { Label(title, systemImage: "checkmark") } else { Text(title) }
        }
    }
}

/// The filters in use, as quiet pills over the grid; a tap on one takes
/// it off.
struct BrowseActiveFilters: View {
    @Binding var narrow: BrowseFilters
    /// The Show filter's name, when one is on.
    let filter: String?
    let clearFilter: () -> Void

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 8) {
                if let filter { pill(filter) { clearFilter() } }
                if let genre = narrow.genre { pill(genre) { narrow.genre = nil } }
                if let decade = narrow.decade { pill("\(String(decade))s") { narrow.decade = nil } }
                if let rating = narrow.minRating { pill("\(BrowseFilters.ratingTitle(rating)) stars") { narrow.minRating = nil } }
                if let language = narrow.audioLanguage { pill("\(language.name) audio") { narrow.audioLanguage = nil } }
                if let language = narrow.subtitleLanguage { pill("\(language.name) subtitles") { narrow.subtitleLanguage = nil } }
            }
        }
        .contentMargins(.horizontal, Metrics.gutter, for: .scrollContent)
        .scrollIndicators(.hidden)
        .padding(.top, 8)
    }

    private func pill(_ text: String, remove: @escaping () -> Void) -> some View {
        Button(action: remove) {
            HStack(spacing: 6) {
                Text(text)
                Image(systemName: "xmark").imageScale(.small).foregroundStyle(Color.ink.opacity(0.6))
            }
            .font(.subheadline.weight(.medium))
            .padding(.horizontal, 14)
            .frame(minHeight: 36)
            .background(Color.ink.opacity(0.12), in: .capsule)
            .frame(minHeight: 44)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(text), remove filter")
    }
}

/// A genre as a quiet pill that opens its grid: Home's genre row and the
/// suggestions of an empty Search.
struct BrowseGenrePill: View {
    let genre: String

    var body: some View {
        NavigationLink {
            LibraryPage(source: GridSource(id: "genre:\(genre)", title: genre))
        } label: {
            Text(genre)
                .font(.subheadline.weight(.medium))
                .padding(.horizontal, 16)
                .frame(minHeight: 44)
                .background(Color.ink.opacity(0.12), in: .rect(cornerRadius: Metrics.controlCorner))
                .contentShape(.rect(cornerRadius: Metrics.controlCorner))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("genre.\(genre)")
    }
}
