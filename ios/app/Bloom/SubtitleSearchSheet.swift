import BloomCore
import SwiftUI

/// Search for subtitles (inventory §3.9): the server's providers, in a
/// language; a download goes into the library and plays at once.
struct SubtitleSearchSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let model: PlayerModel
    @State private var language = ""
    @State private var found: [SubtitleCandidate]?
    @State private var searching = false
    @State private var downloading: String?

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Picker("Language", selection: $language) {
                        ForEach(languages, id: \.code) { Text($0.name).tag($0.code) }
                    }
                    Button(searching ? "Searching…" : "Search") { Task { await search() } }.disabled(searching)
                }
                if let found {
                    Section {
                        ForEach(found, id: \.id) { candidate in
                            Button { Task { await download(candidate) } } label: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(candidate.name).lineLimit(2)
                                    Text(([candidate.detail] + candidate.badges).filter { !$0.isEmpty }.joined(separator: " · "))
                                        .font(.caption).foregroundStyle(.secondary)
                                    if downloading == candidate.id { ProgressView() }
                                }
                            }
                            .disabled(downloading != nil)
                        }
                        if found.isEmpty {
                            Text("Nothing found. The server needs a subtitle provider plugin, such as OpenSubtitles.")
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .navigationTitle("Search for subtitles")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
            .onAppear { if language.isEmpty { language = languages.first?.code ?? "eng" } }
        }
        .presentationDetents([.medium, .large])
    }

    private var languages: [SubtitleLanguage] {
        app.server?.subtitleLanguages(preference: app.prefs.text("SubtitleLanguagePreference").nilIfEmpty) ?? []
    }

    private func search() async {
        guard let id = model.info?.itemId, let server = app.server else { return }
        searching = true
        defer { searching = false }
        do {
            found = try await server.searchSubtitles(itemId: id, language: language)
        } catch {
            app.say("The search failed: \(Words.error(error))")
        }
    }

    private func download(_ candidate: SubtitleCandidate) async {
        guard let id = model.info?.itemId, let server = app.server else { return }
        downloading = candidate.id
        defer { downloading = nil }
        do {
            if let index = try await server.downloadSubtitle(itemId: id, subtitleId: candidate.id) {
                dismiss()
                await model.useDownloadedSubtitle(index)
            } else {
                app.say("The server took the subtitle but has not listed it yet. Try the subtitle menu in a moment.")
            }
        } catch {
            app.say("The download failed: \(Words.error(error))")
        }
    }
}
