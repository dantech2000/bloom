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
        let users: [PublicUser]
        let quickConnect: Bool
        let branding: LoginBranding?
    }

    @State private var address = ""
    @State private var found: Found?
    @State private var username = ""
    @State private var password = ""
    @State private var busy = false
    @State private var error: String?
    /// The Quick Connect code shown while another device approves it.
    @State private var code: QuickConnectCode?
    @FocusState private var field: Field?

    var body: some View {
        NavigationStack {
            Form {
                if let found {
                    signIn(found)
                } else {
                    addressSection
                }
                if let error {
                    Section { Text(error).foregroundStyle(Color.fail) }
                }
                if !model.accounts.isEmpty, found == nil {
                    Section("Your profiles") {
                        ForEach(model.accounts, id: \.self) { account in
                            Button {
                                Task { await model.open(account) }
                            } label: {
                                LabeledContent(account.userName, value: account.serverName)
                            }
                            .swipeActions {
                                Button("Forget", role: .destructive) { Task { await model.forget(account) } }
                            }
                        }
                    }
                }
            }
            .navigationTitle(found == nil ? "Connect to a server" : "Who is watching?")
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
        }
        .onAppear { field = .address }
    }

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
            Button("Connect") { Task { await connect() } }
                .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
        } footer: {
            Text("Enter the address of your Jellyfin server.")
        }
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
            Section("Users on \(found.info.name)") {
                ForEach(found.users, id: \.id) { user in
                    Button(user.name) {
                        username = user.name
                        field = .password
                    }
                }
            }
        }
        Section {
            TextField("Username", text: $username)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.next)
                .focused($field, equals: .username)
                .onSubmit { field = .password }
                .accessibilityIdentifier("connect.username")
            SecureField("Password", text: $password)
                .textContentType(.password)
                .submitLabel(.go)
                .focused($field, equals: .password)
                .onSubmit { Task { await signIn() } }
                .accessibilityIdentifier("connect.password")
            Button("Sign In") { Task { await signIn() } }
                .disabled(username.isEmpty)
        } footer: {
            // The server's own words for its sign-in page (Markdown, as the
            // web client reads it).
            if let disclaimer = found.branding?.disclaimer {
                Text((try? AttributedString(markdown: disclaimer)) ?? AttributedString(disclaimer))
            }
        }
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
            }
            .task(id: code?.secret) { await pollQuickConnect(found) }
        }
        Section {
            Button("Change server") {
                self.found = nil
                error = nil
            }
        }
    }

    /// The address as the server list keeps it: a scheme, no trailing slash.
    private func normalized(_ text: String) -> String {
        var url = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if !url.contains("://") { url = "http://" + url }
        while url.hasSuffix("/") { url.removeLast() }
        return url
    }

    private func connect() async {
        let url = normalized(address)
        busy = true
        error = nil
        defer { busy = false }
        let server = Server(url: url, deviceId: model.deviceId, secrets: model.keychain)
        do {
            let info = try await server.publicInfo()
            let users = (try? await server.publicUsers()) ?? []
            let quick = await server.quickConnectEnabled()
            let branding = try? await server.loginBranding()
            found = Found(server: server, info: info, url: url, users: users, quickConnect: quick, branding: branding)
            field = .username
        } catch {
            self.error = Words.error(error)
        }
    }

    private func startQuickConnect(_ found: Found) async {
        error = nil
        // The code must not sit under the keyboard.
        field = nil
        do {
            code = try await found.server.quickConnectStart()
        } catch {
            self.error = Words.error(error)
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
                    self.error = Words.error(error)
                    self.code = nil
                }
                return
            }
        }
        if self.code?.secret == code.secret {
            self.code = nil
            error = "The code expired. Start Quick Connect again."
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
        defer { busy = false }
        do {
            let me = try await found.server.signIn(username: username, password: password)
            password = ""
            await model.signedIn(server: found.server, info: found.info, url: found.url, me: me)
        } catch BloomError.SignInAgain {
            error = "Wrong username or password."
        } catch {
            self.error = Words.error(error)
        }
    }
}
