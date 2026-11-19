import BloomCore
import Foundation
import SwiftUI

/// A profile signed in on a server. The token is not here: it is in the
/// Keychain, through core's `SecretStore` (spec §8.1).
struct Account: Codable, Hashable {
    var url: String
    var serverId: String
    var serverName: String
    var userId: String
    var userName: String
}

/// The accounts and the active one, in the app's own defaults.
enum Accounts {
    private static let key = "accounts"
    private static let activeKey = "activeUser"

    static func load() -> [Account] {
        guard let data = UserDefaults.standard.data(forKey: key) else { return [] }
        return (try? JSONDecoder().decode([Account].self, from: data)) ?? []
    }

    static func save(_ accounts: [Account]) {
        UserDefaults.standard.set(try? JSONEncoder().encode(accounts), forKey: key)
    }

    static var activeUser: String? {
        get { UserDefaults.standard.string(forKey: activeKey) }
        set { UserDefaults.standard.set(newValue, forKey: activeKey) }
    }
}

/// What to play, and whether from the start.
struct PlayRequest: Identifiable, Hashable {
    let item: MediaItem
    let fromStart: Bool
    /// Where to start, when another device named it (Play on this phone).
    var startSecs: Double? = nil
    /// What plays after it (Play All, Shuffle, a playlist from a place).
    var queue: [MediaItem] = []
    /// Load it paused (a SyncPlay group starts it).
    var paused = false
    /// The tracks chosen on the detail page, by the server's index.
    var audioIndex: Int64? = nil
    var subtitleIndex: Int64? = nil
    var id: String { item.id + (fromStart ? ".start" : "") }
}

/// A loop over one notification of the app on the main actor, until the
/// task is cancelled.
func onAppNotification(_ name: Notification.Name, _ body: @escaping @MainActor () -> Void) -> Task<Void, Never> {
    Task {
        for await _ in NotificationCenter.default.notifications(named: name) { body() }
    }
}

@Observable
final class AppModel {
    enum Screen: Equatable { case loading, connect, main, failed(String) }

    /// The app's model, for the SyncPlay bridge (it runs off the main actor).
    static weak var current: AppModel?

    var screen: Screen = .loading
    var accounts: [Account] = Accounts.load()
    var active: Account?
    var server: Server?
    /// Bumped at each end of a session (a sign-out, another profile or
    /// server): an answer asked for in the session before is dropped.
    @ObservationIgnored private(set) var sessionEpoch = 0
    var home: Home?
    var libraries: [MediaItem] = []
    var tab: AppTab = .home
    var playing: PlayRequest? {
        didSet {
            // The player of a new request is made here, not in a view's body.
            if let playing { _ = playerFor(playing) }
            // News that came while the player was open: Home loads now.
            if playing == nil, newsOwed { Task { await refresh() } }
        }
    }
    /// The player that plays, full screen or small (it outlives the full
    /// screen view: the small player and system Picture in Picture keep it).
    var player: PlayerModel?
    /// The player is small, over the pages (the Mac's small window).
    var miniPlayer = false
    /// The next `playerFor` is the small player's return to full screen.
    @ObservationIgnored private var expanding = false
    /// The app is in front; false from the background notification to the
    /// return (no polling meanwhile).
    @ObservationIgnored private(set) var foreground = true
    @ObservationIgnored private var appWatches: [Task<Void, Never>] = []

    var account = false
    var settings = false
    /// The server could not be reached; the downloads play from this device.
    var offline = false
    /// Why, in words (the reason of `Words.error`), for the offline page.
    var offlineWhy = ""
    /// A Retry of the offline accessory or page is under way.
    var retrying = false
    /// The tab before going offline: the return goes back to it.
    @ObservationIgnored private var tabBeforeOffline: AppTab?
    /// While offline on the downloads: a look at the server every 30 s.
    @ObservationIgnored private var serverWatch: Task<Void, Never>?
    /// Bumped by each refresh on news, so an open library grid loads again.
    var newsRefreshes = 0
    /// Every film and series of the session, for the matches of Search at
    /// each key (the Mac's title index); loaded after the sign-in and on news.
    @ObservationIgnored var titleIndex: TitleIndex?
    @ObservationIgnored private var titleIndexAt: Date?
    /// The session the title index belongs to.
    @ObservationIgnored private var titleIndexEpoch: Int?
    /// The Jellyfin app that plays instead of this device (Play on), and
    /// its sheet.
    var remote: RemoteSession?
    var showPlayOn = false
    /// The "Add to playlist / collection" sheet.
    var addRequest: AddRequest?
    /// The measured speed of the link to the server (bits per second), and
    /// the cap Auto chose on it last, for "Auto" quality.
    @ObservationIgnored var linkBps: UInt64?
    @ObservationIgnored var autoCap: UInt64?

    /// The Jellyfin Enhanced plugin's switches for this user, with the ones
    /// this device turned off; nil without the plugin.
    var enhanced: EnhancedSettings?
    /// As the server has them, for Settings.
    var enhancedServer: EnhancedSettings? {
        didSet {
            applyEnhanced()
            Task { await applyQualityTags() }
        }
    }

    /// SyncPlay: the session of this server (made with the socket), what
    /// it shows, and its sheet.
    var syncSession: SyncSession?
    var syncState: SyncState?
    var showSyncPlay = false
    var showDashboard = false
    /// Bumped when a collection's items changed, so its page loads again.
    var collectionChanged = 0
    /// The Chromecast that plays instead of this device, its device, and
    /// what it does (refreshed by its events, and each second while the
    /// app is in front: the position moves while it plays).
    var chromecast: Chromecast?
    var castDevice: CastDevice?
    var castStatus: CastStatus?
    @ObservationIgnored private var castEvents: Task<Void, Never>?
    @ObservationIgnored private var castPoll: Task<Void, Never>?
    /// Keeps the cast going while the app is away (spec §8.4).
    @ObservationIgnored private var castLife: CastLife?

    /// The server's socket: news, and commands of other devices.
    @ObservationIgnored private var socket: ServerSocket?
    @ObservationIgnored private var socketEvents: Task<Void, Never>?
    @ObservationIgnored private var newsWait: Task<Void, Never>?
    /// When the first news that no reload has handled came.
    @ObservationIgnored private var newsFirst: Date?
    @ObservationIgnored private var newsOwed = false
    /// The socket was closed for the background and opens at the return.
    @ObservationIgnored private var socketResting = false
    /// The background time that lets the socket close.
    @ObservationIgnored private var restTask = UIBackgroundTaskIdentifier.invalid

    /// The toast at the top: one at a time, a new one replaces the old.
    var toast: String?

    let keychain = Keychain()
    /// The user's settings on the server.
    let prefs = ServerPrefs()

    /// The player for a play request: the one that plays it already (a
    /// return from the small player), else a new one; another item's
    /// player stops first.
    func playerFor(_ request: PlayRequest) -> PlayerModel {
        // The same request (a return from the small player), or the item
        // that plays asked for with nothing new: no start, no tracks.
        let nothingNew = !request.fromStart && request.startSecs == nil && !request.paused
            && request.audioIndex == nil && request.subtitleIndex == nil
        if let player, expanding || player.info?.itemId == request.item.id && nothingNew {
            expanding = false
            return player
        }
        player?.close()
        let model = PlayerModel()
        model.prefs = prefs
        model.onFinish = { [weak self] in self?.closePlayer() }
        player = model
        return model
    }

    /// The small player: the full screen goes, the playback stays.
    func minimizePlayer() {
        guard player != nil else { return }
        miniPlayer = true
        playing = nil
    }

    /// Back to the full screen from the small player.
    func expandPlayer() {
        guard let player, let request = player.startRequest else { return }
        miniPlayer = false
        expanding = true
        playing = request
    }

    /// The end of playback, from either view.
    func closePlayer() {
        player?.close()
        player = nil
        miniPlayer = false
        playing = nil
        Task { await refresh(titles: false) }
    }

    /// Measures the link, off the main path (a few MB).
    func measureLink() {
        // Not on cellular or in Low Data Mode, nor while a video plays
        // (spec §16.8); a path change measures again (`LinkWatch`).
        guard Preferences.shared.lowerOnSlowLink, let server, LinkWatch.shared.allows() else { return }
        let epoch = sessionEpoch
        Task {
            if let bps = try? await server.measureLink(), bps > 0, epoch == sessionEpoch {
                linkBps = bps
                PlayerModel.stalls.measured()
                NSLog("LINK: %.1f Mbps", Double(bps) / 1_000_000)
            }
        }
    }

    /// The server's switches less the ones this device turned off.
    func applyEnhanced() {
        guard var settings = enhancedServer else {
            enhanced = nil
            return
        }
        let off = Preferences.shared.enhancedOff
        if off.contains("seerr_rows") { settings.seerrSimilar = false; settings.seerrRecommended = false }
        if off.contains("links") { settings.seerrLink = false; settings.arrLinks = false }
        if off.contains("rating_in_player") { settings.ratingInPlayer = false }
        if off.contains("auto_skip_intro") { settings.autoSkipIntro = false }
        if off.contains("auto_skip_outro") { settings.autoSkipOutro = false }
        if off.contains("bookmarks") { settings.bookmarks = false }
        if off.contains("hidden_content") { settings.hiddenContent = false }
        if off.contains("request_more") { settings.requestMore = false }
        // The custom subtitle style: own setting, then the system's alone.
        if off.contains("subtitle_style") { settings.subtitle = nil }
        enhanced = settings
    }

    func say(_ text: String) {
        toast = text
        AccessibilityNotification.Announcement(text).post()
    }

    /// This device's id for the server: in the Keychain, this device only
    /// (spec §8.1), so a restored backup on another phone gets its own.
    var deviceId: String {
        if let id = keychain.get(key: "device-id"), !id.isEmpty { return id }
        let id = UUID().uuidString.lowercased()
        keychain.set(key: "device-id", value: id)
        return id
    }

    func start() async {
        Self.current = self
        prefs.app = self
        watchBackground()
        AirPlayNotice.shared.watch(self)
        // A test or a debug run can point the app at one server (the mock).
        if let url = ProcessInfo.processInfo.environment["BLOOM_SERVER"] {
            await signInForTests(url: url)
            return
        }
        if let id = Accounts.activeUser, let account = accounts.first(where: { $0.userId == id }) {
            await open(account)
        } else {
            screen = .connect
        }
    }

    /// Opens a profile signed in before.
    func open(_ account: Account) async {
        let server = Server(url: account.url, deviceId: deviceId, secrets: keychain)
        guard (try? server.restore(userId: account.userId)) == true else {
            screen = .connect
            return
        }
        endSession()
        self.server = server
        active = account
        Accounts.activeUser = account.userId
        screen = .loading
        await reload()
    }

    /// After a sign-in on the connect screen.
    func signedIn(server: Server, info: ServerInfo, url: String, me: SignedIn) async {
        let account = Account(url: url, serverId: info.id, serverName: info.name, userId: me.userId, userName: me.userName)
        accounts.removeAll { $0.userId == me.userId && $0.serverId == info.id }
        accounts.append(account)
        Accounts.save(accounts)
        Accounts.activeUser = me.userId
        endSession()
        self.server = server
        active = account
        screen = .loading
        await reload()
    }

    /// The end of the session before another (a sign-out, another profile
    /// or server): the player, the socket, SyncPlay, any target (the Mac's
    /// `cast_off`) and the offline state go, and the epoch moves on.
    private func endSession() {
        sessionEpoch += 1
        closeSocket()
        player?.close()
        player = nil
        miniPlayer = false
        playing = nil
        prefs.reset()
        endSync()
        endCast()
        remote = nil
        leaveOffline()
    }

    func reload() async {
        guard let server else { screen = .connect; return }
        let epoch = sessionEpoch
        let wasOffline = offline
        do {
            async let home = server.home()
            async let libraries = server.libraries()
            let (loadedHome, loadedLibraries) = try await (home, libraries)
            // A sign-out, another server, or the connect screen came meanwhile.
            guard epoch == sessionEpoch, screen != .connect else { return }
            self.home = loadedHome
            self.libraries = loadedLibraries
            offline = false
            screen = .main
            if wasOffline { cameBack() }
            openSocket()
            loadTitleIndex()
            Task { await prefs.ensureLoaded(server) }
            measureLink()
            Task {
                let enhanced = (try? await server.enhanced()) ?? nil
                if epoch == sessionEpoch { enhancedServer = enhanced }
            }
            if !OfflinePositions.all.isEmpty { Task { await OfflinePositions.flush(server) } }
        } catch BloomError.SignInAgain {
            guard epoch == sessionEpoch else { return }
            screen = .connect
        } catch BloomError.Offline(let why) where epoch == sessionEpoch && screen != .connect
                    && (screen == .main || !Downloads.shared.entries.isEmpty) {
            // Offline on the main screen, or at the start with downloads:
            // the app stays on its tabs with the downloads (inventory §1.3)
            // and looks for the server now and then.
            goOffline(Words.error(BloomError.Offline(why: why)))
        } catch {
            guard epoch == sessionEpoch, screen != .connect else { return }
            screen = .failed(Words.error(error))
        }
    }

    /// The server went away (spec §16.9): the tabs stay, the Downloads tab
    /// is selected, the accessory says "{server} is offline" with Retry,
    /// and Home shows the offline page. `announce` false: the user chose
    /// it (Open Downloads on the offline page), so no toast.
    func goOffline(_ why: String, announce: Bool = true) {
        offlineWhy = why
        let already = offline && screen == .main
        offline = true
        screen = .main
        guard !already else { return }
        NSLog("OFFLINE: %@", why)
        if tab != .downloads { tabBeforeOffline = tab }
        tab = .downloads
        if announce { say("\(serverName) is offline") }
        watchForServer()
    }

    /// The name the offline words use.
    var serverName: String { active.map(ServerNames.shared.shown) ?? "The server" }

    /// The pages loaded after being offline: a toast, and the tab the user
    /// was on before (the Downloads tab was the app's choice).
    private func cameBack() {
        serverWatch?.cancel()
        NSLog("OFFLINE: back")
        say("The server is back")
        if tab == .downloads, let before = tabBeforeOffline { tab = before }
        tabBeforeOffline = nil
    }

    /// Retry of the offline accessory and the offline page.
    func retry() async {
        guard !retrying else { return }
        retrying = true
        defer { retrying = false }
        await reload()
        if offline { say("\(serverName) is still offline") }
    }

    /// The socket closed while on the tabs: one look at the server says
    /// whether it went away or only the socket dropped.
    private func probe() {
        guard screen == .main, !offline, !socketResting, let server else { return }
        let epoch = sessionEpoch
        Task { [weak self] in
            do {
                _ = try await server.libraries()
            } catch BloomError.Offline(let why) {
                guard let self, epoch == self.sessionEpoch else { return }
                self.goOffline(Words.error(BloomError.Offline(why: why)))
            } catch {}
        }
    }

    /// While offline on the downloads: a look at the server every 30 s;
    /// when it answers, the pages load again and a toast says so.
    private func watchForServer() {
        serverWatch?.cancel()
        serverWatch = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(30))
                guard let self, self.offline, let server = self.server else { return }
                if (try? await server.libraries()) != nil {
                    await self.reload()
                    // Home may still fail: the watch goes on while offline.
                    if !self.offline { return }
                }
            }
        }
    }

    func signOut() async {
        guard let account = active else { return }
        try? await server?.signOut(userId: account.userId)
        accounts.removeAll { $0 == account }
        Accounts.save(accounts)
        Accounts.activeUser = nil
        endSession()
        server = nil
        active = nil
        home = nil
        libraries = []
        screen = .connect
    }

    /// No offline state carries over to another session.
    private func leaveOffline() {
        serverWatch?.cancel()
        serverWatch = nil
        offline = false
        offlineWhy = ""
        tabBeforeOffline = nil
        titleIndex = nil
    }

    /// Forgets a saved profile: its token is revoked on the server (when it
    /// answers) and leaves the Keychain.
    func forget(_ account: Account) async {
        let server = Server(url: account.url, deviceId: deviceId, secrets: keychain)
        if (try? server.restore(userId: account.userId)) == true {
            try? await server.signOut(userId: account.userId)
        }
        accounts.removeAll { $0 == account }
        Accounts.save(accounts)
        if Accounts.activeUser == account.userId { Accounts.activeUser = nil }
    }

    /// Back to the server list, keeping this session (Servers & profiles…).
    func showServers() {
        screen = .connect
    }

    func play(_ item: MediaItem, fromStart: Bool = false, audio: Int64? = nil, subtitle: Int64? = nil) {
        if playElsewhere([item], start: fromStart ? 0 : Double(item.resumeSecs)) { return }
        playing = PlayRequest(item: item, fromStart: fromStart, audioIndex: audio, subtitleIndex: subtitle)
    }

    /// Plays from a place (a bookmark), wherever plays go.
    func play(_ item: MediaItem, startSecs: Double) {
        if playElsewhere([item], start: startSecs) { return }
        playing = PlayRequest(item: item, fromStart: false, startSecs: startSecs)
    }

    /// Plays a list in order (Play All, Shuffle, Play from here). A remote
    /// target gets the whole list; a Chromecast the first item.
    func play(all items: [MediaItem], shuffled: Bool = false) {
        var items = items.filter { $0.playable }
        if shuffled { items.shuffle() }
        guard let first = items.first else {
            say("Nothing here plays.")
            return
        }
        if playElsewhere(items, start: 0) { return }
        playing = PlayRequest(item: first, fromStart: true, queue: Array(items.dropFirst()))
    }

    /// A play that is not this device's, in this order: the SyncPlay group
    /// (inventory §4.6), the Chromecast (the first item), the chosen
    /// device (inventory §4.1). False when this device plays it.
    private func playElsewhere(_ items: [MediaItem], start: Double) -> Bool {
        if let group = SyncBridge.shared.group {
            group.user(intent: .play(itemIds: items.map(\.id), index: 0, startSecs: start))
            return true
        }
        if let chromecast, let device = castDevice {
            let id = items[0].id
            send(to: device.name) { try await chromecast.load(itemId: id, startSecs: start) }
            return true
        }
        if let remote {
            let (server, ids) = (self.server, items.map(\.id))
            send(to: remote.deviceName) {
                try await server?.remotePlay(sessionId: remote.id, itemIds: ids, command: .playNow,
                                             startSecs: start > 0 ? start : nil)
            }
            return true
        }
        return false
    }

    /// Hands a play to another device, and says whether it took it.
    private func send(to name: String, _ play: @escaping @MainActor () async throws -> Void) {
        Task {
            do {
                try await play()
                say("Playing on \(name)")
            } catch {
                say("\(name) did not take it: \(Words.error(error))")
            }
        }
    }

    /// The sessions of other devices, and the chosen one brought up to date.
    func remoteSessions() async -> [RemoteSession] {
        let sessions = (try? await server?.remoteSessions()) ?? []
        if let chosen = remote {
            remote = sessions.first { $0.id == chosen.id } ?? chosen
        }
        return sessions
    }

    // MARK: Chromecast

    /// Connects to a Chromecast and makes it the target; the certificate
    /// it showed the first time is the one it must show (trust on first use,
    /// kept in the Keychain as on the Mac's config).
    func choose(_ device: CastDevice) {
        endCast()
        guard let server, let account = active else { return }
        remote = nil
        let key = "cast-pin.\(device.id)"
        let pin = keychain.get(key: key).flatMap { Data(base64Encoded: $0) }
        let cast: Chromecast
        do {
            cast = try server.castConnect(device: device, pin: pin, serverId: account.serverId)
        } catch {
            say("\(device.name) could not be reached: \(Words.error(error))")
            return
        }
        chromecast = cast
        castDevice = device
        castStatus = cast.status()
        castEvents = Task { [weak self] in
            while let event = await cast.nextEvent() {
                // Events left in the channel after a switch are the old device's.
                guard let self, !Task.isCancelled, self.chromecast === cast else { return }
                switch event {
                case .pinned(let certificate):
                    self.keychain.set(key: key, value: certificate.base64EncodedString())
                case .error(let message):
                    self.say(message)
                case .closed, .changed, .ended:
                    break
                }
                self.castStatus = cast.status()
            }
        }
        castLife = CastLife { [weak self] in self?.castReturned() }
        pollCast()
    }

    /// Reads the Chromecast's status each second while the app is in
    /// front; the background stops it and the return starts it again.
    private func pollCast() {
        castPoll?.cancel()
        guard let cast = chromecast, foreground else { return }
        castPoll = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, self.chromecast === cast else { return }
                self.castStatus = cast.status()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    /// Back from the background: the device played on meanwhile. Asks it
    /// for its state again; a socket that died while the app was suspended
    /// fails on that, and the session connects again by itself.
    private func castReturned() {
        guard let chromecast else { return }
        chromecast.refresh()
        castStatus = chromecast.status()
    }

    /// This device again, or another target: the Chromecast connection
    /// ends. The item this app loaded stops (the Mac's `leave`); what
    /// another sender put on the device plays on.
    func endCast() {
        castLife?.end()
        castLife = nil
        castEvents?.cancel()
        castEvents = nil
        castPoll?.cancel()
        castPoll = nil
        chromecast?.leave()
        chromecast?.close()
        chromecast = nil
        castDevice = nil
        castStatus = nil
    }

    /// A request to Seerr, after the user confirmed it.
    func seerrRequest(tmdbId: Int64, mediaType: SeerrMediaType, title: String) async {
        do {
            try await server?.seerrRequest(tmdbId: tmdbId, mediaType: mediaType)
            say("Requested \(title)")
        } catch {
            say("Seerr did not take the request: \(Words.error(error))")
        }
    }

    // MARK: The server's socket

    /// Opens the socket once signed in; a test on a real server's session
    /// keeps it closed (BLOOM_NO_SOCKET), as the Mac's test instances do.
    private func openSocket() {
        guard socket == nil, let server, ProcessInfo.processInfo.environment["BLOOM_NO_SOCKET"] != "1",
              let socket = try? server.socket() else { return }
        self.socket = socket
        // SyncPlay reads the socket too; its session outlives a rest of the
        // socket in the background, so a group is not left for that.
        if syncSession == nil, let session = try? server.syncplay(player: SyncBridge.shared) {
            syncSession = session
            SyncBridge.shared.use(session)
        }
        socket.attachSyncplay(session: syncSession)
        socketEvents = Task { [weak self] in
            while let event = await socket.next() {
                // Events left in the channel after a close are the old socket's.
                guard let self, !Task.isCancelled, self.socket === socket else { return }
                self.handle(event)
            }
        }
    }

    /// The socket rests in the background (spec §8.10): iOS suspends the
    /// app, and a frozen socket would only time out. The player keeps it,
    /// since its audio plays on and other devices may control it. At the
    /// return, what changed meanwhile is unknown, so Home loads again.
    /// The system's notifications, not SwiftUI's scene phase: the phase
    /// arrives with a view update, which a suspended app never makes.
    private func watchBackground() {
        guard appWatches.isEmpty else { return }
        appWatches = [
            onAppNotification(UIApplication.didEnterBackgroundNotification) { [weak self] in self?.enteredBackground() },
            onAppNotification(UIApplication.willEnterForegroundNotification) { [weak self] in self?.returned() },
        ]
    }

    private func enteredBackground() {
        foreground = false
        castPoll?.cancel()
        castPoll = nil
        guard socket != nil, player == nil else { return }
        NSLog("SOCKET: rests in the background")
        // A second of background time: the socket's thread sees the stop
        // within half a second and closes the connection.
        endRestTask()
        restTask = UIApplication.shared.beginBackgroundTask(withName: "socket") { [weak self] in
            MainActor.assumeIsolated { self?.endRestTask() }
        }
        closeSocket()
        socketResting = true
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(1))
            self?.endRestTask()
        }
    }

    private func endRestTask() {
        guard restTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(restTask)
        restTask = .invalid
    }

    private func returned() {
        foreground = true
        pollCast()
        guard socketResting else { return }
        socketResting = false
        guard screen == .main else { return }
        openSocket()
        news()
    }

    /// The end of SyncPlay for this session (a sign-out, another profile):
    /// the session leaves its group as it goes.
    private func endSync() {
        SyncBridge.shared.use(nil)
        syncSession = nil
        syncState = nil
    }

    private func closeSocket() {
        socketEvents?.cancel()
        socketEvents = nil
        socket?.close()
        socket = nil
        newsWait?.cancel()
        newsFirst = nil
        newsOwed = false
    }

    private func handle(_ event: ServerEvent) {
        switch event {
        case .opened(let again):
            NSLog("SOCKET: open%@", again ? " again" : "")
            postCapabilities()
            // What changed while the socket was down is unknown: reload.
            // Offline, the socket that opens again is the server's return.
            if offline { Task { await reload() } } else if again { news() }
        case .closed:
            NSLog("SOCKET: closed")
            probe()
        case .news:
            news()
        case .sessions:
            break
        case .command(let request):
            guard Preferences.shared.allowRemoteControl else { return }
            NSLog("SOCKET: command %@", request.describe())
            run(request)
        case .play(let ids, let command, let start, let index):
            guard Preferences.shared.allowRemoteControl else { return }
            NSLog("SOCKET: play %@ %@", "\(command)", ids.joined(separator: ","))
            Task { await playFromRemote(ids, command: command, start: start, index: index.map(Int.init)) }
        }
    }

    /// Tells the server whether other devices may control this one.
    func postCapabilities() {
        guard socket != nil, let server else { return }
        let control = Preferences.shared.allowRemoteControl
        Task { try? await server.postCapabilities(control: control) }
    }

    /// News of the server: Home loads again once the news has been quiet
    /// for 3 s (15 s at most), never under the player (core's rules).
    private func news() {
        let first = newsFirst ?? .now
        newsFirst = first
        newsWait?.cancel()
        let since = UInt64(max(0, Date.now.timeIntervalSince(first)) * 1000)
        newsWait = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(newsDelayMs(sinceFirstMs: since)))
            guard !Task.isCancelled, let self else { return }
            self.newsFirst = nil
            if self.playing != nil {
                self.newsOwed = true
            } else {
                await self.refresh()
            }
        }
    }

    /// Home, the libraries and an open library grid again, quietly: a
    /// failure keeps what shows, but a server gone away is offline.
    /// `titles` false: the change (a favourite, a played mark) added or
    /// removed no title, so the title index stays.
    func refresh(titles: Bool = true) async {
        newsOwed = false
        guard let server, screen == .main, !offline else { return }
        let epoch = sessionEpoch
        async let home = server.home()
        async let libraries = server.libraries()
        do {
            let loaded = try await home
            guard epoch == sessionEpoch else { return }
            self.home = loaded
        } catch BloomError.Offline(let why) {
            guard epoch == sessionEpoch else { return }
            goOffline(Words.error(BloomError.Offline(why: why)))
            return
        } catch {}
        let loadedLibraries = try? await libraries
        guard epoch == sessionEpoch else { return }
        if let loadedLibraries { self.libraries = loadedLibraries }
        newsRefreshes += 1
        if titles { loadTitleIndex(force: true) }
        NSLog("SOCKET: refreshed")
    }

    /// Loads the title index off the main thread, when there is none for
    /// this session, it is older than ten minutes, or `force` (news: the
    /// library changed). An answer for another session or an older request
    /// is dropped; a failure leaves the old index and the next search tries.
    func loadTitleIndex(force: Bool = false) {
        guard let server, !offline else { return }
        let epoch = sessionEpoch
        if titleIndexEpoch != epoch {
            titleIndex = nil
            titleIndexAt = nil
            titleIndexEpoch = epoch
        }
        if !force, let at = titleIndexAt, Date.now.timeIntervalSince(at) < 600 { return }
        let asked = Date.now
        titleIndexAt = asked
        Task { [weak self] in
            let index = try? await server.titleIndex()
            guard let self, epoch == self.sessionEpoch, self.titleIndexAt == asked else { return }
            if let index {
                self.titleIndex = index
                NSLog("SEARCH: title index of %d titles", index.count())
            } else {
                self.titleIndexAt = nil
            }
        }
    }

    /// A command of another device, on the player that is open.
    private func run(_ request: RemoteRequest) {
        let player = self.player
        let state = PlayerState(playerOpen: player != nil, paused: player?.state.paused ?? false,
                                volume: 100, muted: player?.muted ?? false)
        switch request.plan(state: state) {
        case .togglePause: player?.togglePause()
        case .seekTo(let secs): player?.seek(to: secs)
        case .seekBy(let secs): player?.skip(secs)
        case .next: if let player { Task { await player.playNext() } }
        case .stop: player?.finished = true
        case .setMuted(let muted): player?.setMuted(muted)
        case .toggleMute: if let player { player.setMuted(!player.muted) }
        case .toast(let header, let text): say("\(header): \(text)")
        case .previous: if let player { Task { await player.previous() } }
        case .setAudioStream(let index): if let player { Task { await player.useStream(audio: index) } }
        case .setSubtitleStream(let index): if let player { Task { await player.useStream(subtitle: index) } }
        // The phone's volume is the user's (iOS lets no app set it); full
        // screen is the only way the player shows.
        case .setVolume, .toggleFullscreen, .nothing:
            break
        }
    }

    /// Another device asks this one to play: the first item it named, here
    /// (never passed on to a Chromecast or a session this phone controls).
    /// Play next and Play last go to the open player's queue.
    private func playFromRemote(_ ids: [String], command: PlayCommand, start: Double?, index: Int?) async {
        if command == .playNext || command == .playLast {
            guard let player, let server else {
                // Nothing plays: the first item plays now.
                await playFromRemote(ids, command: .playNow, start: nil, index: 0)
                return
            }
            var items: [MediaItem] = []
            for id in ids {
                if let detail = try? await server.detail(itemId: id) { items.append(detail.item) }
            }
            player.enqueue(items, next: command == .playNext)
            say(items.count == 1 ? "\(items[0].title) is queued." : "\(items.count) items are queued.")
            return
        }
        let at = index.flatMap { ids.indices.contains($0) ? $0 : nil } ?? 0
        guard ids.indices.contains(at), let server,
              let detail = try? await server.detail(itemId: ids[at]) else { return }
        if let player {
            // The open player ends first; the new one opens after the cover.
            player.finished = true
            try? await Task.sleep(for: .milliseconds(800))
        }
        // The ids after it are the queue (a list played from another device).
        let rest = Array(ids[(at + 1)...].prefix(100))
        var found: [String: MediaItem] = [:]
        await withTaskGroup(of: (String, MediaItem?).self) { group in
            for id in rest {
                group.addTask { (id, try? await server.detail(itemId: id).item) }
            }
            for await (id, item) in group { found[id] = item }
        }
        playing = PlayRequest(item: detail.item, fromStart: start == nil, startSecs: start,
                              queue: rest.compactMap { found[$0] })
    }

    func remoteCommand(_ command: PlaystateCommand) {
        guard let remote else { return }
        Task { try? await server?.remotePlaystate(sessionId: remote.id, command: command) }
    }

    func toggleFavorite(_ item: MediaItem) async {
        try? await server?.setFavorite(itemId: item.id, favorite: !item.favorite)
        await refresh(titles: false)
    }

    /// Continue Watching or Next Up without this item (Jellyfin Enhanced).
    func hideFromRow(_ item: MediaItem, nextUp: Bool) async {
        try? await server?.hideFromRow(itemId: item.id, nextUp: nextUp)
        await refresh(titles: false)
    }

    func togglePlayed(_ item: MediaItem) async {
        try? await server?.setPlayed(itemId: item.id, played: !item.played)
        await refresh(titles: false)
    }

    /// Debug runs against the mock server sign in as its user.
    private func signInForTests(url: String) async {
        let server = Server(url: url, deviceId: deviceId, secrets: keychain)
        do {
            let info = try await server.publicInfo()
            // The mock takes any name; the container's account is named by the test.
            let env = ProcessInfo.processInfo.environment
            let me = try await server.signIn(username: env["BLOOM_USER"] ?? "tester", password: env["BLOOM_PASSWORD"] ?? "")
            await signedIn(server: server, info: info, url: url, me: me)
        } catch {
            screen = .failed(Words.error(error))
        }
    }
}

enum AppTab: Hashable {
    case home, library, downloads, search
    case libraryItem(String)
}

enum Route: Hashable {
    case item(MediaItem)
    case library(MediaItem)
    case favorites
    case playlists
    case person(id: String, name: String)
}

/// What a grid page lists: a library or folder, or one of the app's own
/// lists (the bridge's "favorites", "playlists", "person:<id>").
struct GridSource: Hashable {
    let id: String
    let title: String
    let collectionType: CollectionType?
    /// The server's kind of the folder (`boxSet` for a collection).
    var kind: MediaKind? = nil

    init(_ item: MediaItem) {
        id = item.id
        title = item.name
        collectionType = item.collectionType
        kind = item.kind
    }

    init(id: String, title: String) {
        self.id = id
        self.title = title
        collectionType = nil
    }

    static let favorites = GridSource(id: "favorites", title: "Favorites")
    static let playlists = GridSource(id: "playlists", title: "Playlists")
    static func person(_ id: String, _ name: String) -> GridSource { GridSource(id: "person:\(id)", title: name) }
}

extension MediaItem: @retroactive Identifiable {}

/// Every sentence the app shows for a code from core (spec §5.1: core sends
/// codes, Swift writes the words).
enum Words {
    static func error(_ error: Error) -> String {
        guard let error = error as? BloomError else { return error.localizedDescription }
        switch error {
        case .Offline(let why):
            switch why {
            case .dns: return "The server's name could not be found. Check the address."
            case .connect: return "No connection to the server. Check that it runs and that this device is on its network."
            case .timeout: return "The server did not answer in time."
            case .tls: return "The secure connection to the server could not be set up."
            case .gateway(let status): return "The server is offline (the proxy in front of it answered \(status))."
            }
        case .SignInAgain: return "Sign in again."
        case .Answered(let detail): return "The server answered with an error: \(detail)"
        case .NotSignedIn: return "Not signed in."
        case .Internal(let message): return "Something went wrong: \(message)"
        }
    }

    static func runtime(_ secs: Int64?) -> String? {
        guard let secs, secs > 0 else { return nil }
        if secs < 60 { return "< 1m" }
        let h = secs / 3600, m = (secs % 3600) / 60
        return h > 0 ? "\(h)h \(m)m" : "\(m)m"
    }

    static func clock(_ secs: Double) -> String {
        guard secs.isFinite, secs >= 0 else { return "0:00" }
        let s = Int(secs), h = s / 3600, m = (s % 3600) / 60, r = s % 60
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, r) : String(format: "%d:%02d", m, r)
    }
}
