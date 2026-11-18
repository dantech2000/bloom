import BloomCore
import Foundation
import Observation

/// The user's settings on the server (the Mac's `Prefs`): the user's
/// Configuration (languages, subtitle mode, remember tracks) and the
/// display preferences (home rows, skip lengths, the Up next card). Every
/// Jellyfin app of the user reads them.
///
/// A change shows at once and is saved whole; the bridge merges it into the
/// server's document of now, so what another client set stays. A save
/// that fails puts the value back and says so. One save per document at a
/// time; a change made meanwhile goes when it ends.
@Observable
final class ServerPrefs {
    enum Load: Equatable { case idle, loading, loaded, failed(String) }

    private(set) var load: Load = .idle
    private(set) var hasPassword = true
    private(set) var isAdmin = false
    private(set) var canCollect = false
    private(set) var canDownload = true
    private(set) var syncplayAccess = SyncPlayAccess.createAndJoinGroups
    private(set) var picture: String?
    private(set) var canManageSubtitles = false
    var languages: [Language] = []

    /// What shows, with the user's changes.
    @ObservationIgnored private var current: [PrefsDoc: [String: Any]] = [:]
    /// What the server has, as far as the app knows.
    @ObservationIgnored private var acknowledged: [PrefsDoc: [String: Any]] = [:]
    @ObservationIgnored private var busy: Set<PrefsDoc> = []
    @ObservationIgnored private var queued: Set<PrefsDoc> = []
    /// Changes on every edit, so the views that read a value draw again.
    private var version = 0

    @ObservationIgnored weak var app: AppModel?

    func reset() {
        load = .idle
        current = [:]
        acknowledged = [:]
        version += 1
    }

    /// The app's session (`AppModel.sessionEpoch`, bumped at a sign-out or
    /// another server): an answer asked for in another one is dropped.
    private var session: Int { app?.sessionEpoch ?? 0 }

    func ensureLoaded(_ server: Server?) async {
        guard load != .loaded, load != .loading, let server else { return }
        load = .loading
        let asked = session
        do {
            let prefs = try await server.prefs()
            guard session == asked else { return }
            let configuration = Self.object(prefs.configurationJson)
            let display = Self.object(prefs.displayJson)
            current = [.configuration: configuration, .display: display]
            acknowledged = current
            hasPassword = prefs.hasPassword
            isAdmin = prefs.isAdmin
            canCollect = prefs.canCollect
            canDownload = prefs.canDownload
            syncplayAccess = prefs.syncplayAccess
            picture = prefs.picture
            canManageSubtitles = prefs.canManageSubtitles
            version += 1
            load = .loaded
        } catch {
            guard session == asked else { return }
            load = .failed(Words.error(error))
        }
        if languages.isEmpty { languages = (try? await server.languages()) ?? [] }
    }

    // MARK: Reading

    private func value(_ doc: PrefsDoc, _ key: String) -> Any? {
        _ = version
        return current[doc]?[key]
    }

    func text(_ key: String) -> String { value(.configuration, key) as? String ?? "" }
    func bool(_ key: String, default fallback: Bool) -> Bool { value(.configuration, key) as? Bool ?? fallback }

    /// A key of the display document's `CustomPrefs` (strings, as the web
    /// client keeps them).
    func custom(_ key: String) -> String? {
        ((value(.display, "CustomPrefs") as? [String: Any])?[key]) as? String
    }

    /// The home row kind in a slot, or the web client's default.
    func homeSection(_ slot: Int) -> String {
        custom("homesection\(slot)") ?? (slot < Self.defaultSections.count ? Self.defaultSections[slot] : "none")
    }

    static let defaultSections = ["smalllibrarytiles", "resume", "resumeaudio", "resumebook", "livetv", "nextup", "latestmedia"]

    /// Seconds of a skip button: `skipBackLength` (10) or `skipForwardLength` (30).
    func skipSecs(_ key: String) -> Double {
        guard let text = custom(key), let ms = Double(text), ms > 0 else { return key == "skipBackLength" ? 10 : 30 }
        return ms / 1000
    }

    var upNextCard: Bool { custom("enableNextVideoInfoOverlay") != "false" }

    // MARK: Changing

    func set(_ key: String, _ value: Any) {
        guard load == .loaded else { return }
        current[.configuration]?[key] = value
        version += 1
        save(.configuration)
    }

    func setCustom(_ changes: [String: String]) {
        guard load == .loaded else { return }
        var display = current[.display] ?? [:]
        var custom = display["CustomPrefs"] as? [String: Any] ?? [:]
        for (key, value) in changes { custom[key] = value }
        display["CustomPrefs"] = custom
        current[.display] = display
        version += 1
        save(.display)
    }

    private func save(_ doc: PrefsDoc) {
        guard let server = app?.server else { return }
        if busy.contains(doc) {
            queued.insert(doc)
            return
        }
        guard let edited = current[doc], let known = acknowledged[doc],
              !NSDictionary(dictionary: edited).isEqual(to: known) else { return }
        busy.insert(doc)
        let (sentJson, knownJson) = (Self.json(edited), Self.json(known))
        let asked = session
        Task {
            defer {
                busy.remove(doc)
                if queued.remove(doc) != nil { save(doc) }
            }
            do {
                let taken = Self.object(try await server.savePrefs(doc: doc, originalJson: knownJson, editedJson: sentJson))
                // Another session's document is not this one's.
                guard session == asked else { return }
                acknowledged[doc] = taken
                if doc == .display { await app?.refresh() }
            } catch {
                guard session == asked else { return }
                // What the save tried, back to what the server has.
                for (key, value) in edited where !Self.same(value, known[key]) {
                    if Self.same(current[doc]?[key], value) { current[doc]?[key] = known[key] }
                }
                version += 1
                app?.say("The setting was not saved: \(Words.error(error))")
            }
        }
    }

    // MARK: JSON

    private static func object(_ json: String) -> [String: Any] {
        (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] ?? [:]
    }

    private static func json(_ object: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: object) else { return "{}" }
        return String(decoding: data, as: UTF8.self)
    }

    private static func same(_ a: Any?, _ b: Any?) -> Bool {
        switch (a, b) {
        case (nil, nil): true
        case let (a?, b?): NSArray(array: [a]).isEqual(to: [b])
        default: false
        }
    }
}

