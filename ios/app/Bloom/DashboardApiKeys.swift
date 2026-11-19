import BloomCore
import SwiftUI

struct ApiKeysPage: View {
    @Environment(AppModel.self) private var model
    @State private var naming = false
    @State private var app = ""
    @State private var revoking: ApiKey?

    var body: some View {
        Loaded(title: "API keys", every: 30, load: { try await model.adminServer().adminApiKeys() }) { keys, reload in
            List(keys, id: \.token) { key in
                VStack(alignment: .leading, spacing: 2) {
                    Text(key.app)
                    Text("Made \(When.text(key.created))").font(.caption).foregroundStyle(.secondary)
                }
                .contextMenu {
                    Button("Copy key", systemImage: "doc.on.doc") {
                        UIPasteboard.general.string = key.token
                        model.say("Key copied")
                    }
                }
                .swipeActions {
                    Button("Revoke", role: .destructive) { revoking = key }
                }
            }
            .emptyState(keys.isEmpty, "No API keys", symbol: "key", "A key lets another app use this server. Tap + to make one.")
            .toolbar { Button("New key", systemImage: "plus") { naming = true } }
            .alert("New API key", isPresented: $naming) {
                TextField("App name", text: $app)
                Button("Cancel", role: .cancel) {}
                Button("Create") {
                    let name = app
                    app = ""
                    Task {
                        do { try await model.server?.adminCreateApiKey(app: name); await reload() } catch { model.say(Words.error(error)) }
                    }
                }
            }
            .confirmationDialog("Revoke the key of \(revoking?.app ?? "")?", isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }),
                                titleVisibility: .visible, presenting: revoking) { key in
                Button("Revoke", role: .destructive) {
                    Task {
                        do { try await model.server?.adminRevokeApiKey(token: key.token); await reload() } catch { model.say(Words.error(error)) }
                    }
                }
            } message: { _ in
                Text("What uses it stops working.")
            }
        }
    }
}
