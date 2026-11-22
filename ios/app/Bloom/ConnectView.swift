import BloomCore
import SwiftUI

/// Connect to a server, then sign in (spec §8.1). System fields: the
/// address has the URL keyboard; the username is untyped with no
/// correction (Daniel's choice); the password is a secure field.
struct ConnectView: View {
    @Environment(AppModel.self) private var model

    private enum Field: Hashable { case address, username, password }
    private struct Found {
        let server: Server
        let info: ServerInfo
        let url: String
        let users: [PublicProfile]
        let quickConnect: Bool
        let branding: LoginBranding?
    }

    @State private var address = ""
    @State private var found: Found?
    @State private var username = ""
    @State private var password = ""
    @State private var busy = false
    /// Why the last step failed; it shows by the fields it is about and
    /// goes when the user types again.
    @State private var error: String?
    /// The profile tapped on the sign-in screen.
    @State private var chosen: String?
    /// The Quick Connect code shown while another device approves it.
    @State private var code: QuickConnectCode?
    @State private var quickError: String?
    /// Servers that answered on the local network; nil while looking.
    @State private var nearby: [ServerDiscovery.Found]?
    @FocusState private var field: Field?

    var body: some View {
        NavigationStack {
            Form {
                if let found {
                    signIn(found)
                } else {
                    nearbySection
                    addressSection
                }
                if !model.accounts.isEmpty, found == nil {
                    Section("Your profiles") {
                        ForEach(model.accounts, id: \.self) { account in
                            Button {
                                Task { await model.open(account) }
                            } label: {
                                LabeledContent(account.userName, value: ServerNames.shared.shown(account))
                            }
                            .swipeActions {
                                Button("Forget", role: .destructive) { Task { await model.forget(account) } }
                            }
                        }
                    }
                }
            }
            .navigationTitle(title)
            // The server's name under the title: a long one (a machine id)
            // would be cut short in a large title.
            .navigationSubtitle(found.map { ServerNames.shared.shown(id: $0.info.id, reported: $0.info.name) } ?? "")
            .toolbar {
                // "Servers & profiles…" keeps the session: the way back to it.
                if let active = model.active, model.server != nil, found == nil {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Back to \(active.userName)") { model.screen = .main }
                    }
                }
            }
            .disabled(busy)
            .overlay { if busy { ProgressView() } }
            .onChange(of: username) { error = nil }
            .onChange(of: password) { error = nil }
            .onChange(of: address) { error = nil }
        }
        .onAppear { field = .address }
        .task { await lookNearby() }
    }

    /// Profiles to pick from ask who watches; a bare form asks to sign in
    /// (the server's name is the subtitle).
    private var title: String {
        guard let found else { return "Connect to a server" }
        return found.users.isEmpty ? "Sign in" : "Who is watching?"
    }

    /// The servers that answered on this network, above the address field.
    @ViewBuilder
    private var nearbySection: some View {
        Section {
            if let nearby {
                ForEach(nearby, id: \.id) { server in
                    Button {
                        address = server.address
                        Task { await connect() }
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "server.rack").foregroundStyle(.secondary).frame(width: 24)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(ServerNames.shared.shown(id: server.id, reported: server.name)).foregroundStyle(Color.ink)
                                Text(server.address).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(.tertiary)
                        }
                    }
                    .accessibilityIdentifier("connect.nearby.\(server.id)")
                }
                if nearby.isEmpty {
                    HStack {
                        Text("No server answered.").foregroundStyle(.secondary)
                        Spacer()
                        Button("Look again") { Task { await lookNearby() } }
                    }
                }
            } else {
                HStack(spacing: 10) {
                    ProgressView()
                    Text("Looking for servers…").foregroundStyle(.secondary)
                }
            }
        } header: {
            Text("On this network")
        }
    }

    @ViewBuilder
    private var addressSection: some View {
        Section {
            TextField("Server address", text: $address, prompt: Text("https://jellyfin.example.com"))
                .textContentType(.URL)
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.go)
                .focused($field, equals: .address)
                .onSubmit { Task { await connect() } }
                .accessibilityIdentifier("connect.address")
        } header: {
            Text("Address")
        } footer: {
            if let error {
                Label(error, systemImage: "exclamationmark.circle")
                    .font(.body)
                    .foregroundStyle(Color.fail)
                    .accessibilityIdentifier("connect.error")
            } else {
                Text("Enter the address of your Jellyfin server.")
            }
        }
        mainAction("Connect", disabled: address.trimmingCharacters(in: .whitespaces).isEmpty) { await connect() }
    }

    /// The one white button of the screen, in a section of its own so it
    /// does not read as another field of the group above (DESIGN.md).
    private func mainAction(_ title: String, disabled: Bool, _ action: @escaping () async -> Void) -> some View {
        Section {
            Button(title) { Task { await action() } }
                .buttonStyle(MainButtonStyle(wide: true))
                .disabled(disabled)
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
        }
        .listSectionSpacing(12)
    }

    @ViewBuilder
    private func signIn(_ found: Found) -> some View {
        if let splash = found.branding?.splash {
            Section {
                Picture(url: splash, corner: 0)
                    .frame(height: 160)
                    .listRowInsets(EdgeInsets())
            }
        }
        if !found.users.isEmpty {
            Section {
                profiles(found)
            }
        }
        Section {
            TextField("Username", text: $username)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.next)
                .focused($field, equals: .username)
                .onSubmit { field = .password }
                .onChange(of: username) { if username != found.users.first(where: { $0.id == chosen })?.name { chosen = nil } }
                .accessibilityIdentifier("connect.username")
            SecureField("Password", text: $password)
                .textContentType(.password)
                .submitLabel(.go)
                .focused($field, equals: .password)
                .onSubmit { Task { await signIn() } }
                .accessibilityIdentifier("connect.password")
        } header: {
            if !found.users.isEmpty { Text("Or another account") }
        } footer: {
            VStack(alignment: .leading, spacing: 8) {
                if let error {
                    // Body text under the group (DESIGN.md, "Inputs").
                    Label(error, systemImage: "exclamationmark.circle")
                        .font(.body)
                        .foregroundStyle(Color.fail)
                        .accessibilityIdentifier("connect.error")
                }
                // The server's own words for its sign-in page (Markdown, as
                // the web client reads it).
                if let disclaimer = found.branding?.disclaimer {
                    Text((try? AttributedString(markdown: disclaimer)) ?? AttributedString(disclaimer))
                }
            }
        }
        mainAction("Sign In", disabled: username.isEmpty) { await signIn() }
        if found.quickConnect {
            Section {
                if let code {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(code.code.count == 6 ? "\(code.code.prefix(3))  \(code.code.suffix(3))" : code.code)
                            .font(.system(.largeTitle, design: .monospaced).weight(.semibold))
                            .accessibilityLabel("Code \(code.code.map(String.init).joined(separator: " "))")
                        Text("On a device where you are signed in, open Quick Connect and enter this code.")
                            .font(.footnote).foregroundStyle(.secondary)
                        Label("Waiting for the approval…", systemImage: "hourglass").font(.footnote)
                    }
                    Button("Cancel", role: .cancel) { self.code = nil }
                } else {
                    Button("Use Quick Connect") { Task { await startQuickConnect(found) } }
                }
            } header: {
                Text("Quick Connect")
            } footer: {
                if let quickError { Text(quickError).foregroundStyle(Color.fail) }
            }
            .task(id: code?.secret) { await pollQuickConnect(found) }
        }
        Section {
            Button("Change server") {
                self.found = nil
                error = nil
                quickError = nil
                chosen = nil
            }
        }
    }

    /// The server's users as pictures to tap, three across: a tap fills the
    /// name and puts the password field in focus. (Jellyfin 12 sends
    /// `HasPassword` as true for every user, so no tap signs in alone; an
    /// account with no password signs in with the field empty.)
    private func profiles(_ found: Found) -> some View {
        // One or two users sit in the middle; more go three across.
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 12, alignment: .top), count: min(max(found.users.count, 1), 3)),
                  spacing: 16) {
            ForEach(found.users, id: \.id) { user in
                Button {
                    chosen = user.id
                    username = user.name
                    password = ""
                    field = .password
                } label: {
                    VStack(spacing: 8) {
                        ProfileDisc(name: user.name, picture: user.picture, size: 72)
                            .overlay {
                                Circle().strokeBorder(Color.ink, lineWidth: chosen == user.id ? 2.5 : 0).padding(-4)
                            }
                        Text(user.name)
                            .font(.subheadline.weight(chosen == user.id ? .semibold : .regular))
                            .foregroundStyle(Color.ink)
                            .lineLimit(1)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 4)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(user.name)
                .accessibilityAddTraits(chosen == user.id ? .isSelected : [])
                .accessibilityIdentifier("connect.profile.\(user.name)")
            }
        }
        .padding(.vertical, 8)
        .listRowBackground(Color.clear)
        .listRowInsets(EdgeInsets())
    }

    /// The address as the server list keeps it: a scheme, no trailing slash.
    private func normalized(_ text: String) -> String {
        var url = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if !url.contains("://") { url = "http://" + url }
        while url.hasSuffix("/") { url.removeLast() }
        return url
    }

    private func lookNearby() async {
        nearby = nil
        nearby = await ServerDiscovery.search()
    }

    private func connect() async {
        let url = normalized(address)
        busy = true
        error = nil
        defer { busy = false }
        let server = Server(url: url, deviceId: model.deviceId, secrets: model.keychain)
        do {
            let info = try await server.publicInfo()
            let users = (try? await server.publicProfiles()) ?? []
            let quick = await server.quickConnectEnabled()
            let branding = try? await server.loginBranding()
            username = ""
            password = ""
            chosen = nil
            found = Found(server: server, info: info, url: url, users: users, quickConnect: quick, branding: branding)
            field = users.isEmpty ? .username : nil
        } catch {
            self.error = Words.error(error)
        }
    }

    private func startQuickConnect(_ found: Found) async {
        quickError = nil
        // The code must not sit under the keyboard.
        field = nil
        do {
            code = try await found.server.quickConnectStart()
        } catch {
            quickError = Words.error(error)
        }
    }

    /// Polls every 3 s until another device approves the code; the code
    /// lasts 10 minutes (inventory §1.2). The task stops with the code.
    private func pollQuickConnect(_ found: Found) async {
        guard let code else { return }
        let started = Date.now
        while !Task.isCancelled, Date.now.timeIntervalSince(started) < 600 {
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            if (try? await found.server.quickConnectApproved(secret: code.secret)) == true {
                do {
                    let me = try await found.server.quickConnectSignIn(secret: code.secret)
                    self.code = nil
                    await model.signedIn(server: found.server, info: found.info, url: found.url, me: me)
                } catch {
                    quickError = Words.error(error)
                    self.code = nil
                }
                return
            }
        }
        if self.code?.secret == code.secret {
            self.code = nil
            quickError = "The code expired. Start Quick Connect again."
        }
    }

    private func signIn() async {
        guard let found else { return }
        if username.isEmpty {
            error = "Enter a username."
            return
        }
        busy = true
        error = nil
        do {
            let me = try await found.server.signIn(username: username, password: password)
            password = ""
            busy = false
            await model.signedIn(server: found.server, info: found.info, url: found.url, me: me)
        } catch BloomError.SignInAgain {
            busy = false
            error = "Wrong username or password."
            // Back in the password field once the form takes input again
            // (the busy form had dropped the focus).
            Task { field = .password }
        } catch {
            busy = false
            self.error = Words.error(error)
        }
    }
}

/// A user's picture on a disc, or the first letters of the name when there
/// is none (or it fails to load).
struct ProfileDisc: View {
    let name: String
    let picture: String?
    var size: CGFloat = 72

    var body: some View {
        ZStack {
            Circle().fill(Color.ink.opacity(0.12))
            Text(initials).font(.system(size: size * 0.36, weight: .semibold)).foregroundStyle(Color.ink)
            if let picture, let url = URL(string: picture) {
                AsyncImage(url: url, transaction: Transaction(animation: .easeOut(duration: 0.2))) { phase in
                    if case .success(let image) = phase {
                        image.resizable().scaledToFill().transition(.opacity)
                    }
                }
            }
        }
        .frame(width: size, height: size)
        .clipShape(.circle)
        .accessibilityHidden(true)
    }

    private var initials: String {
        let words = name.split(whereSeparator: { $0 == " " || $0 == "." || $0 == "_" || $0 == "-" })
        let letters = words.prefix(2).compactMap(\.first)
        return String(letters.isEmpty ? Array(name.prefix(1)) : letters).uppercased()
    }
}
