import BloomCore
import SwiftUI

struct DevicesPage: View {
    @Environment(AppModel.self) private var model
    @State private var deleting: AdminDevice?

    var body: some View {
        Loaded(title: "Devices", every: 30, load: { try await model.adminServer().adminDevices() }) { devices, reload in
            List(devices, id: \.id) { device in
                VStack(alignment: .leading, spacing: 2) {
                    Text(device.name)
                    Text([device.app, device.user, device.lastActive.map(When.text)].compactMap { $0 }.joined(separator: " · "))
                        .font(.caption).foregroundStyle(.secondary)
                }
                .swipeActions {
                    if device.id != model.deviceId {
                        Button("Delete", role: .destructive) { deleting = device }
                    }
                }
            }
            .confirmationDialog("Delete \(deleting?.name ?? "")?", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
                                titleVisibility: .visible, presenting: deleting) { device in
                Button("Delete device", role: .destructive) {
                    Task {
                        do { try await model.server?.adminDeleteDevice(deviceId: device.id); await reload() } catch { model.say(Words.error(error)) }
                    }
                }
            } message: { _ in
                Text("It is signed out and must sign in again.")
            }
        }
    }
}

struct ActivityPage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Loaded(title: "Activity", every: 10, load: { try await model.adminServer().adminActivity(limit: 100) }) { entries, _ in
            List(entries, id: \.rowKey) { entry in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    if entry.severity != .information {
                        Circle().fill(entry.severity == .error ? Color.fail : Color.warn).frame(width: 8, height: 8)
                    }
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.name)
                        Text([entry.overview, When.text(entry.date)].compactMap { $0 }.joined(separator: " · "))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
    }
}
