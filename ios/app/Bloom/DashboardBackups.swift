import BloomCore
import SwiftUI

/// The server's backups (Jellyfin 10.11 and later; the Mac's
/// src/admin/backups.rs): the archives, a new one with the parts the user
/// picks, and a restore. A restore replaces all the server's data and
/// restarts it, so the app reads the archive's manifest again and asks in
/// red first. The server sends no size of an archive.
struct BackupsPage: View {
    @Environment(AppModel.self) private var model
    @State private var metadata = false
    @State private var subtitles = false
    @State private var trickplay = false
    @State private var creating = false
    @State private var askScan = false
    /// The archive the user asked to restore, with its manifest read again.
    @State private var restoring: BackupInfo?

    var body: some View {
        Loaded(title: "Backups", every: 30, load: { try await model.adminServer().adminBackups() }) { list, reload in
            List {
                Section {
                    Toggle(isOn: $metadata) { part("Metadata", "Pictures and information fetched for the library") }
                    Toggle(isOn: $subtitles) { part("Subtitles", "Subtitle files downloaded or extracted") }
                    Toggle(isOn: $trickplay) { part("Trickplay", "The thumbnails of the seek bar; often large") }
                    if list.scanRunning {
                        StateLine(text: "A library scan is running. A backup made now holds the library halfway through the scan.", color: .warn)
                            .font(.footnote)
                    }
                    Button {
                        if list.scanRunning { askScan = true } else { create(reload) }
                    } label: {
                        HStack {
                            Text(creating ? "Creating Backup…" : "Create Backup")
                            if creating { Spacer(); ProgressView() }
                        }
                    }
                    .disabled(creating)
                    .accessibilityIdentifier("backups.create")
                } header: {
                    Text("New backup")
                } footer: {
                    Text("A backup always holds the database: the users, the settings, what each user watched, and the library.")
                }
                Section {
                    if list.backups.isEmpty {
                        Text("The server has no backups yet.").foregroundStyle(.secondary)
                    }
                    ForEach(list.backups, id: \.fileName) { backup in
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(Self.date(backup.date))
                                Text("Jellyfin \(backup.serverVersion) · \(backup.contents)")
                                    .font(.caption).foregroundStyle(.secondary)
                                Text(When.text(backup.date)).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button("Restore") { ask(backup) }
                                .buttonStyle(.borderless)
                                .foregroundStyle(Color.fail)
                                .accessibilityIdentifier("backups.restore.\(backup.fileName)")
                        }
                        .accessibilityElement(children: .contain)
                    }
                } header: {
                    Text("Backups")
                } footer: {
                    Text(footer(list))
                }
            }
            .alert("Back up during a library scan?", isPresented: $askScan) {
                Button("Cancel", role: .cancel) {}
                Button("Back Up Now") { create(reload) }
            } message: {
                Text("A library scan is running. The backup will hold the library as it is halfway through the scan. Wait for the scan to end for a complete backup.")
            }
            .alert("Restore this backup?", isPresented: Binding(get: { restoring != nil }, set: { if !$0 { restoring = nil } }), presenting: restoring) { backup in
                Button("Cancel", role: .cancel) {}
                Button("Restore and Restart", role: .destructive) { restore(backup) }
            } message: { backup in
                Text(restoreMessage(backup, scanning: list.scanRunning))
            }
        }
    }

    private func part(_ title: String, _ detail: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
            Text(detail).font(.caption).foregroundStyle(.secondary)
        }
    }

    private func footer(_ list: BackupList) -> String {
        let place = list.folder.map { "The server keeps its backups in \($0). " } ?? ""
        return place + "It does not report their size. A restore replaces everything on the server with the backup and restarts it."
    }

    private func restoreMessage(_ backup: BackupInfo, scanning: Bool) -> String {
        var text = "The server restarts and replaces all its data with this backup of \(Self.date(backup.date)) (Jellyfin \(backup.serverVersion), \(backup.contents)). Everything that changed since then is lost: users, settings, what each user watched, and the library. The server is offline until the restore ends. You cannot undo this."
        if scanning { text += " A library scan is running now; the restart stops it." }
        return text
    }

    private func create(_ reload: @escaping () async -> Void) {
        creating = true
        Task {
            do {
                let made = try await model.adminServer().adminCreateBackup(metadata: metadata, subtitles: subtitles, trickplay: trickplay)
                model.say("Backup created: \(made.contents)")
                await reload()
            } catch {
                model.say(Words.error(error))
            }
            creating = false
        }
    }

    /// Reads the manifest again (the archive may be gone), then asks.
    private func ask(_ backup: BackupInfo) {
        Task {
            do { restoring = try await model.adminServer().adminBackupManifest(fileName: backup.fileName) } catch {
                model.say(Words.error(error))
            }
        }
    }

    private func restore(_ backup: BackupInfo) {
        Task {
            do {
                try await model.adminServer().adminRestoreBackup(fileName: backup.fileName)
                model.say("The server restarts to restore the backup.")
            } catch {
                model.say(Words.error(error))
            }
        }
    }

    /// "21 Nov 2026 at 09:12" in local time.
    static func date(_ iso: String) -> String {
        // The server writes seven digits of a second, which the parser
        // refuses: the fraction goes.
        let whole = iso.replacingOccurrences(of: #"\.\d+"#, with: "", options: .regularExpression)
        guard let date = try? Date(whole, strategy: .iso8601) else { return iso }
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}
