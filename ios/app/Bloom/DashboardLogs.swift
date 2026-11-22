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

/// One log file: its end, and with Follow on the lines the server adds,
/// read every few seconds while the page is on screen (only when the
/// file's size moved), with the view kept at the end.
struct LogView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    let name: String
    /// The most of the file the view reads and keeps.
    private static let keep: UInt32 = 200_000
    @State private var text: String?
    /// Where the next read starts: the byte after the last whole line.
    @State private var end: UInt64 = 0
    @State private var follow = false

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    Text(text ?? "Loading…")
                        .font(.system(.caption2, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                        .accessibilityIdentifier("log.text")
                    Color.clear.frame(height: 1).id("end")
                }
            }
            .defaultScrollAnchor(.bottom)
            .onChange(of: text) { if follow { proxy.scrollTo("end", anchor: .bottom) } }
            .onChange(of: follow) { if follow { proxy.scrollTo("end", anchor: .bottom) } }
        }
        .navigationTitle(name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Toggle(isOn: $follow) {
                    Label(follow ? "Following" : "Follow", systemImage: follow ? "dot.radiowaves.left.and.right" : "arrow.down.to.line")
                }
                .toggleStyle(.button)
                .accessibilityIdentifier("log.follow")
            }
        }
        .task {
            do {
                let chunk = try await model.adminServer().adminLogSince(name: name, from: 0, max: Self.keep)
                text = chunk.text
                end = chunk.end
            } catch {
                text = "The log could not load."
            }
        }
        // The follow reads while the page is on screen and the app active.
        .task(id: [follow, scenePhase == .active]) {
            while follow, scenePhase == .active, !Task.isCancelled {
                try? await Task.sleep(for: .seconds(2.5))
                guard follow, !Task.isCancelled, let server = model.server, text != nil else { continue }
                if let chunk = try? await server.adminLogFollow(name: name, end: end, max: Self.keep) {
                    take(chunk)
                }
            }
        }
    }

    private func take(_ chunk: LogChunk) {
        end = chunk.end
        if chunk.restarted {
            text = chunk.text
        } else if !chunk.text.isEmpty {
            var all = (text ?? "") + chunk.text
            // The view keeps the end of the file, from the start of a line.
            if all.utf8.count > Int(Self.keep) {
                let from = all.utf8.index(all.utf8.startIndex, offsetBy: all.utf8.count - Int(Self.keep))
                all = String(String(all[from...]).drop { $0 != "\n" }.dropFirst())
            }
            text = all
        }
    }
}
