import BloomCore
import SwiftUI

struct LogsPage: View {
    @Environment(AppModel.self) private var model
    @State private var find = ""

    var body: some View {
        Loaded(title: "Logs", every: 60, load: { try await model.adminServer().adminLogs() }) { files, _ in
            let shown = find.isEmpty ? files : files.filter { $0.name.localizedCaseInsensitiveContains(find) }
            List(shown, id: \.name) { file in
                NavigationLink { LogView(name: file.name) } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(file.name).lineLimit(2)
                        Text("\(When.text(file.modified)) · \(ByteCountFormatter.string(fromByteCount: Int64(file.size), countStyle: .file))")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            .emptyState(shown.isEmpty, "No logs", symbol: "doc.plaintext", "The server has written no log files.", search: find)
            .searchable(text: $find, prompt: "Find a log")
        }
    }
}

struct LogView: View {
    @Environment(AppModel.self) private var model
    let name: String
    @State private var text: String?

    var body: some View {
        ScrollView {
            Text(text ?? "Loading…")
                .font(.system(.caption2, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
        }
        .defaultScrollAnchor(.bottom)
        .navigationTitle(name)
        .navigationBarTitleDisplayMode(.inline)
        .task { text = (try? await model.server?.adminLogTail(name: name, max: 200_000)) ?? "The log could not load." }
    }
}
