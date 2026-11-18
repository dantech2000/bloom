import BloomCore
import SwiftUI

struct LibrariesPage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Loaded(title: "Libraries", load: { try await model.adminServer().adminLibraries() }) { libraries, reload in
            List(libraries, id: \.id) { library in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(library.name)
                        Spacer()
                        if library.scanProgress == nil {
                            Button("Scan") {
                                Task {
                                    do { try await model.server?.adminScanLibrary(libraryId: library.id); await reload() } catch { model.say(Words.error(error)) }
                                }
                            }
                            .buttonStyle(.borderless)
                        }
                    }
                    if let progress = library.scanProgress {
                        ProgressView(value: progress / 100) { Text("Scanning…").font(.caption) }
                    }
                    Text(library.locations.joined(separator: "\n")).font(.caption).foregroundStyle(.secondary)
                }
            }
        }
    }
}
