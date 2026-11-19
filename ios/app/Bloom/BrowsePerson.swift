import BloomCore
import SwiftUI

/// A person's page: the photo, the name, the birth, a short biography that
/// opens in full, then the films and shows of the libraries, each with
/// what the person did on it.
struct BrowsePersonPage: View {
    @Environment(AppModel.self) private var model
    let id: String
    let name: String
    @State private var person: PersonPage?
    @State private var error: String?
    @State private var bioOpen = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                header
                if let person {
                    if person.credits.isEmpty {
                        Text("No titles in your libraries.").foregroundStyle(Color.ink.opacity(0.6))
                            .padding(.horizontal, Metrics.gutter)
                    } else {
                        VStack(alignment: .leading, spacing: Metrics.shelfSpacing) {
                            Text("In Your Libraries").font(.title3.weight(.semibold)).accessibilityAddTraits(.isHeader)
                            LazyVGrid(columns: Metrics.posterGrid, spacing: Metrics.gridRowSpacing) {
                                ForEach(visible(person.credits), id: \.item.id) { credit in
                                    CardLink(item: credit.item) { PosterCard(item: credit.item, detail: line(credit)) }
                                }
                            }
                        }
                        .padding(.horizontal, Metrics.gutter)
                    }
                } else if error == nil {
                    LazyVGrid(columns: Metrics.posterGrid, spacing: Metrics.gridRowSpacing) {
                        ForEach(0..<6, id: \.self) { _ in PosterSkeleton() }
                    }
                    .padding(.horizontal, Metrics.gutter)
                }
            }
            .padding(.vertical, 12)
        }
        .overlay {
            if let error {
                ContentUnavailableView("This page could not load", systemImage: "exclamationmark.triangle", description: Text(error))
            }
        }
        .background(Color.page)
        .navigationTitle(name)
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
        .refreshable { await load() }
    }

    private func load() async {
        guard let server = model.server else { return }
        do {
            person = try await server.personPage(personId: id)
            error = nil
        } catch {
            if person == nil { self.error = Words.error(error) }
        }
    }

    private func visible(_ credits: [Credit]) -> [Credit] {
        let shown = Set(HiddenTitles.shared.visible(credits.map(\.item), on: .library).map(\.id))
        return credits.filter { shown.contains($0.item.id) }
    }

    /// The role, else the year: "as Jo March", "Director".
    private func line(_ credit: Credit) -> String {
        guard let role = credit.role else { return credit.item.subtitle }
        if let year = credit.item.year { return "\(role) · \(year)" }
        return role
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .center, spacing: 16) {
                Picture(url: person?.image, corner: 56, symbol: "person.fill")
                    .frame(width: 112, height: 112)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 6) {
                    Text(person?.name ?? name).font(.display(28)).lineLimit(2).minimumScaleFactor(0.8)
                    if let facts = facts {
                        Text(facts).font(.subheadline).foregroundStyle(Color.ink.opacity(0.7))
                    }
                }
            }
            if let bio = person?.overview {
                VStack(alignment: .leading, spacing: 6) {
                    Text(bio).font(.subheadline).foregroundStyle(Color.ink.opacity(0.8))
                        .lineLimit(bioOpen ? nil : 4)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("person.bio")
                    // Four lines hold about 200 letters on a phone.
                    if bio.count > 200 {
                        Button(bioOpen ? "Less" : "More") {
                            withAnimation(.easeOut(duration: 0.2)) { bioOpen.toggle() }
                        }
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Color.ink)
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("person.bio.more")
                    }
                }
            }
        }
        .padding(.horizontal, Metrics.gutter)
    }

    /// "Born 2 May 1984 in Harlingen, Netherlands", the death when known.
    private var facts: String? {
        guard let person else { return nil }
        var parts: [String] = []
        if let born = Self.date(person.born) {
            parts.append(person.birthplace.map { "Born \(born) in \($0)" } ?? "Born \(born)")
        } else if let place = person.birthplace {
            parts.append("Born in \(place)")
        }
        if let died = Self.date(person.died) { parts.append("Died \(died)") }
        return parts.isEmpty ? nil : parts.joined(separator: "\n")
    }

    private static func date(_ text: String?) -> String? {
        guard let text, let date = try? Date(text, strategy: Date.ISO8601FormatStyle().year().month().day()) else { return nil }
        // The server's date is a day in UTC: shown in UTC, or a zone west
        // of it shows the day before.
        var style = Date.FormatStyle.dateTime.day().month(.wide).year()
        style.timeZone = TimeZone(identifier: "UTC")!
        return date.formatted(style)
    }
}
