import BloomCore
import SwiftUI

struct UsersPage: View {
    @Environment(AppModel.self) private var model
    @State private var creating = false
    @State private var name = ""
    @State private var password = ""

    var body: some View {
        Loaded(title: "Users", every: 30, load: { try await model.adminServer().adminUsers() }) { users, reload in
            List {
                ForEach(users, id: \.id) { user in
                    NavigationLink { UserPage(user: user) } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(user.name)
                            Text([user.admin ? "Administrator" : nil, user.disabled ? "Disabled" : nil,
                                  user.lastActive.map { "Active \(When.text($0))" }].compactMap { $0 }.joined(separator: " · "))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .toolbar { Button("New user", systemImage: "plus") { creating = true } }
            .alert("New user", isPresented: $creating) {
                TextField("Name", text: $name).textInputAutocapitalization(.never).autocorrectionDisabled()
                SecureField("Password", text: $password)
                Button("Cancel", role: .cancel) {}
                Button("Create") {
                    let (n, p) = (name, password)
                    name = ""
                    password = ""
                    Task {
                        do { try await model.server?.adminCreateUser(name: n, password: p); await reload() } catch { model.say(Words.error(error)) }
                    }
                }
            }
        }
    }
}

struct UserPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let user: AdminUser
    @State private var disabled: Bool
    @State private var settingPassword = false
    @State private var password = ""
    @State private var confirmDelete = false

    init(user: AdminUser) {
        self.user = user
        _disabled = State(initialValue: user.disabled)
    }

    var body: some View {
        List {
            Section {
                LabeledContent("Role", value: user.admin ? "Administrator" : "User")
                LabeledContent("Last active", value: When.text(user.lastActive))
            }
            Section {
                Toggle("Enabled", isOn: Binding(get: { !disabled }, set: { on in
                    disabled = !on
                    Task {
                        do { try await model.server?.adminSetUserDisabled(userId: user.id, disabled: !on) } catch {
                            disabled = on
                            model.say(Words.error(error))
                        }
                    }
                }))
                .disabled(user.id == model.active?.userId)
                Button("Set password…") { settingPassword = true }
                NavigationLink("Access and permissions") {
                    AccessPage(userId: user.id, name: user.name) { saved in disabled = saved.isDisabled }
                }
            }
            if user.id != model.active?.userId {
                Section {
                    Button("Delete user…", role: .destructive) { confirmDelete = true }
                }
            }
        }
        .navigationTitle(user.name)
        .alert("Password of \(user.name)", isPresented: $settingPassword) {
            SecureField("New password", text: $password)
            Button("Cancel", role: .cancel) { password = "" }
            Button("Set") {
                let p = password
                password = ""
                Task {
                    do { try await model.server?.adminSetUserPassword(userId: user.id, password: p); model.say("Password set") } catch { model.say(Words.error(error)) }
                }
            }
        }
        .confirmationDialog("Delete \(user.name)?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete user", role: .destructive) {
                Task {
                    do { try await model.server?.adminDeleteUser(userId: user.id); dismiss() } catch { model.say(Words.error(error)) }
                }
            }
        } message: {
            Text("Their watched state and settings go with them.")
        }
    }
}

/// What one user may do (the Mac's access editor): the libraries and the
/// rights of the policy. The page keeps the policy as the server gave it
/// and as the user edits it; Save sends only the changes, and the rest of
/// the policy goes back as the server has it.
struct AccessPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let userId: String
    let name: String
    /// The user page behind shows the new Enabled state.
    let onSaved: (AccessPolicy) -> Void
    @State private var access: UserAccess?
    @State private var edited: AccessPolicy?
    @State private var error: String?
    @State private var saving = false
    @State private var askAdmin: Bool?
    @State private var askDiscard = false

    private var dirty: Bool { access.map { $0.policy != edited } ?? false }

    var body: some View {
        Group {
            if let access, edited != nil {
                form(access)
            } else if let error {
                ContentUnavailableView {
                    Label("Access could not load", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { Task { await load() } }
                }
            } else {
                ProgressView()
            }
        }
        .navigationTitle("Access")
        .navigationBarBackButtonHidden(dirty)
        .toolbar {
            if dirty {
                ToolbarItem(placement: .cancellationAction) { Button("Discard") { askDiscard = true } }
            }
            ToolbarItem(placement: .confirmationAction) {
                if saving { ProgressView() } else { Button("Save") { Task { await save() } }.disabled(!dirty) }
            }
        }
        .confirmationDialog("Discard the changes?", isPresented: $askDiscard, titleVisibility: .visible) {
            Button("Discard", role: .destructive) { dismiss() }
        } message: {
            Text("The changes to the access of \(name) are not saved.")
        }
        .confirmationDialog(askAdmin == true ? "Make \(name) an administrator?" : "Remove the administrator right of \(name)?",
                            isPresented: Binding(get: { askAdmin != nil }, set: { if !$0 { askAdmin = nil } }), titleVisibility: .visible) {
            Button(askAdmin == true ? "Make administrator" : "Remove right", role: .destructive) {
                if let on = askAdmin { edited?.isAdministrator = on }
            }
        } message: {
            Text(askAdmin == true
                 ? "\(name) can change every setting of the server, and the users and keys of other people. The change applies when you save."
                 : "\(name) loses the access to the dashboard. The change applies when you save.")
        }
        .task { if access == nil { await load() } }
    }

    private func load() async {
        do {
            let loaded = try await model.adminServer().adminUserAccess(userId: userId)
            access = loaded
            edited = loaded.policy
            error = nil
        } catch {
            self.error = Words.error(error)
        }
    }

    private func save() async {
        guard let access, let edited else { return }
        saving = true
        defer { saving = false }
        do {
            let saved = try await model.adminServer().adminSaveUserAccess(userId: userId, original: access.original, edited: edited)
            self.access = saved
            self.edited = saved.policy
            onSaved(saved.policy)
            model.say("Access saved")
        } catch {
            model.say(Words.error(error))
        }
    }

    /// A binding into the edited policy.
    private func value<T>(_ key: WritableKeyPath<AccessPolicy, T>, default fallback: T) -> Binding<T> {
        Binding(get: { edited?[keyPath: key] ?? fallback }, set: { edited?[keyPath: key] = $0 })
    }

    private func flag(_ key: WritableKeyPath<AccessPolicy, Bool>, _ label: String, _ help: String) -> some View {
        Toggle(isOn: value(key, default: false)) { Row(label: label, help: help) }
    }

    private func form(_ access: UserAccess) -> some View {
        let p = edited ?? access.policy
        return Form {
            Section {
                Toggle(isOn: Binding(get: { p.isAdministrator }, set: { askAdmin = $0 })) {
                    Row(label: "Administrator", help: "Can change every setting of the server.")
                }
                .disabled(access.me)
                Toggle(isOn: value(\.isDisabled, default: false)) {
                    Row(label: "Disabled", help: "The user cannot sign in.")
                }
                .disabled(access.me || p.isAdministrator)
                flag(\.isHidden, "Hidden", "The user does not show on the sign-in page.")
            } header: {
                Text("Account")
            } footer: {
                if access.me {
                    Text("You cannot change Administrator or Disabled for the account you are signed in with.")
                } else if p.isAdministrator {
                    Text("The server does not disable an administrator.")
                }
            }
            Section {
                Toggle(isOn: Binding(get: { p.enableAllFolders }, set: { on in
                    edited?.enableAllFolders = on
                    // As the web dashboard does: the list is empty while the
                    // user sees all libraries.
                    if on { edited?.enabledFolders = [] }
                })) {
                    Row(label: "All libraries", help: "The user sees every library, including libraries that you add later.")
                }
                if !p.enableAllFolders {
                    ForEach(access.libraries, id: \.id) { library in
                        Toggle(library.name, isOn: Binding(get: { p.enabledFolders.contains { same($0, library.id) } }, set: { on in
                            edited?.enabledFolders.removeAll { same($0, library.id) }
                            if on { edited?.enabledFolders.append(library.id) }
                        }))
                    }
                }
            } header: {
                Text("Libraries")
            } footer: {
                if access.libraries.isEmpty {
                    Text("The server has no libraries.")
                } else if !p.enableAllFolders && !access.libraries.contains(where: { l in p.enabledFolders.contains { same($0, l.id) } }) {
                    Label("No library is chosen. The user sees no library.", systemImage: "exclamationmark.triangle.fill").foregroundStyle(Color.warn)
                }
            }
            Section("Playback") {
                flag(\.enableMediaPlayback, "Play media", "The user can play videos and music.")
                flag(\.enableVideoPlaybackTranscoding, "Transcode video", "The server may convert video for this user.")
                flag(\.enableAudioPlaybackTranscoding, "Transcode audio", "The server may convert audio for this user.")
                flag(\.enablePlaybackRemuxing, "Remux", "The server may change the container of a file without a new encode.")
            }
            Section("Features") {
                flag(\.enableContentDownloading, "Download media", "The user can download files.")
                flag(\.enableContentDeletion, "Delete media", "The user can delete items from the libraries.")
                flag(\.enableCollectionManagement, "Manage collections", "The user can make and change collections.")
                flag(\.enableSubtitleManagement, "Manage subtitles", "The user can search for, upload and delete subtitles.")
                flag(\.enableRemoteControlOfOtherUsers, "Control other users", "The user can control the sessions of other users.")
                flag(\.enableSharedDeviceControl, "Control shared devices", "The user can control a device that other users also use.")
                Picker(selection: value(\.syncPlayAccess, default: .createAndJoinGroups)) {
                    Text("Create and join groups").tag(SyncPlayAccess.createAndJoinGroups)
                    Text("Join groups").tag(SyncPlayAccess.joinGroups)
                    Text("No access").tag(SyncPlayAccess.off)
                } label: {
                    Row(label: "SyncPlay", help: "What the user can do in a watch-together group.")
                }
            }
            Section("Limits") {
                LabeledContent {
                    TextField("No limit", value: Binding(get: { p.remoteClientBitrateLimit == 0 ? nil : Double(p.remoteClientBitrateLimit) / 1_000_000 },
                                                         set: { edited?.remoteClientBitrateLimit = Int64((max($0 ?? 0, 0) * 1_000_000).rounded()) }),
                              format: .number)
                        .keyboardType(.decimalPad)
                        .multilineTextAlignment(.trailing)
                        .frame(maxWidth: 100)
                    Text("Mbps").foregroundStyle(.secondary)
                } label: {
                    Row(label: "Bitrate limit away from home", help: "The highest bitrate of a stream outside the local network. Empty means no limit.")
                }
                Stepper(value: value(\.maxActiveSessions, default: 0), in: 0...100) {
                    Row(label: "Active sessions: \(p.maxActiveSessions == 0 ? "No limit" : "\(p.maxActiveSessions)")",
                        help: "How many sessions the user can have at one time.")
                }
                Stepper(value: value(\.loginAttemptsBeforeLockout, default: -1), in: -1...100) {
                    Row(label: "Failed sign-ins before lockout: \(p.loginAttemptsBeforeLockout == -1 ? "Never" : "\(p.loginAttemptsBeforeLockout)")",
                        help: "Never keeps the account open. 0 locks it after 3 failed sign-ins.")
                }
            }
        }
    }

    /// Two spellings of one library id (dashes, case) compare equal.
    private func same(_ a: String, _ b: String) -> Bool {
        a.replacingOccurrences(of: "-", with: "").lowercased() == b.replacingOccurrences(of: "-", with: "").lowercased()
    }

    private struct Row: View {
        let label: String
        let help: String
        var body: some View {
            VStack(alignment: .leading, spacing: 2) {
                Text(label)
                Text(help).font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
