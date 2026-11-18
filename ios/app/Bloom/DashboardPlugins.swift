import BloomCore
import SwiftUI

struct PluginsPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openURL) private var openURL
    @State private var removing: AdminPlugin?

    var body: some View {
        Loaded(title: "Plugins", every: 30, load: { try await model.adminServer().adminPlugins() }) { plugins, reload in
            List(plugins, id: \.self.key) { plugin in
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(plugin.name)
                        // An active plugin has no dot; one that needs a look has.
                        if plugin.status == .active {
                            Text("\(plugin.version) · \(plugin.status.name())").font(.caption).foregroundStyle(.secondary)
                        } else {
                            StateLine(text: "\(plugin.version) · \(plugin.status.name())", color: plugin.status == .malfunctioned ? .fail : .warn)
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    Spacer()
                    Menu {
                        Button("Open settings in the web", systemImage: "safari") {
                            if let url = URL(string: plugin.webUrl) { openURL(url) }
                        }
                        Button(plugin.status == .disabled ? "Enable" : "Disable") {
                            Task {
                                do {
                                    try await model.server?.adminSetPluginEnabled(pluginId: plugin.id, version: plugin.version, enabled: plugin.status == .disabled)
                                    model.say("The change counts after a restart of the server")
                                    await reload()
                                } catch {
                                    model.say(Words.error(error))
                                }
                            }
                        }
                        if plugin.canUninstall {
                            Button("Uninstall…", role: .destructive) { removing = plugin }
                        }
                    } label: {
                        Label("More", systemImage: "ellipsis").labelStyle(.iconOnly).toolTarget()
                    }
                }
            }
            .confirmationDialog("Uninstall \(removing?.name ?? "")?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                                titleVisibility: .visible, presenting: removing) { plugin in
                Button("Uninstall", role: .destructive) {
                    Task {
                        do { try await model.server?.adminUninstallPlugin(pluginId: plugin.id, version: plugin.version); await reload() } catch {
                            model.say(Words.error(error))
                        }
                    }
                }
            }
        }
    }
}

extension AdminPlugin {
    /// Two installs of one plugin share an id (CLAUDE.md): the version too.
    var key: String { id + version }
}
