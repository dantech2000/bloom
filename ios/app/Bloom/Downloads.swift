import BloomCore
import Foundation
import Network
import Synchronization
import SwiftUI

/// One downloaded (or downloading) item: the original file, as the Mac
/// keeps it (inventory §5.6).
struct DownloadEntry: Codable, Identifiable {
    enum State: String, Codable { case downloading, paused, failed, done }

    /// Every field but `authorization`, which stays in memory: a token is
    /// kept in the Keychain alone (spec §8.1). An index of an older build
    /// still has it; the decoder passes over it.
    private enum CodingKeys: String, CodingKey {
        case item, segments, file, url, state, received, total, resumeData, server, size, reason, recovered, estimate, mediaSourceId
    }

    /// The server and the item: two servers can know one item id.
    var id: String { Self.key(server: server, item: item.id) }
    var item: MediaItem
    var segments: [Segment]
    /// The file's name in the downloads folder.
    var file: String
    var url: String
    /// The `Authorization` header of the request, from the server of this
    /// session; empty after a start of the app until the server gives it.
    var authorization = ""
    var state: State
    var received: Int64 = 0
    var total: Int64 = 0
    var resumeData: Data?
    /// The id of the item's server; nil for a download made before the app
    /// recorded it, which counts for any server.
    var server: String?
    /// The size of the file the server gave at the first answer; a later
    /// answer with another size is another file (a release replaced it).
    var size: Int64?
    /// Why the download failed, in words.
    var reason: String?
    /// The version downloaded (a media source id), when the detail page
    /// chose one; nil is the item's own file. A resume asks for the same.
    var mediaSourceId: String?
    /// Made again from the folder without its record: the server gives the
    /// details at the next look.
    var recovered: Bool?
    /// The server's estimate of the size, before the first answer gives it.
    var estimate: Int64?

    var progress: Double { total > 0 ? Double(received) / Double(total) : 0 }
    /// The bytes it takes on the device, or will.
    var bytes: Int64 { max(total, received) }
    /// What a running download will take in all: its size, or the estimate
    /// until the server answers.
    var expected: Int64 { max(bytes, state == .downloading ? estimate ?? 0 : 0) }
    /// The bytes a running download has still to write.
    var remaining: Int64 { state == .downloading ? max(0, (total > 0 ? total : estimate ?? 0) - received) : 0 }

    nonisolated static func key(server: String?, item: String) -> String { "\(server ?? "")/\(item)" }
}

/// The downloads of this device (spec §8.5): transfers on one background
/// `URLSession`, so they go on with the app suspended; files in Application
/// Support, out of the backup; an index next to them, and a record of each
/// download beside its file, so a lost index comes back from the folder. A
/// force quit cancels the transfers (Apple): those entries show as paused
/// and resume.
@Observable
final class Downloads {
    static let shared = Downloads()
    nonisolated static let sessionId = "dev.bloom.ios.next.downloads"
    /// Space that stays free on the device after a download.
    static let reserve: Int64 = 500_000_000
    /// No byte for this long: the transfer is made again from where it was
    /// (the Mac's stall rule, longer for a phone's links).
    static let stallLimit: TimeInterval = 45
    /// Stalls in a row before the download counts as failed.
    static let stallsAtMost = 5

    var entries: [DownloadEntry] = []

    @ObservationIgnored private var session: URLSession!
    @ObservationIgnored private let delegate = DownloadDelegate()
    @ObservationIgnored private var tasks: [String: URLSessionDownloadTask] = [:]
    @ObservationIgnored private var lastSave = Date.distantPast
    /// The downloads at once the session was made with ("1 or 2 at a time").
    @ObservationIgnored private var parallel = 1
    /// The session is made again for a new "at a time": transfers wait.
    @ObservationIgnored private var rebuilding = false
    @ObservationIgnored private var waitingForSession: [(key: String, resumeData: Data?)] = []
    /// The time of the last byte of each transfer, the stalls in a row, and
    /// the transfers that wait for a network (no stall).
    @ObservationIgnored private var lastBytes: [String: Date] = [:]
    @ObservationIgnored private var stalls: [String: Int] = [:]
    @ObservationIgnored private var noNetwork: Set<String> = []
    @ObservationIgnored private var watchdog: Task<Void, Never>?
    @ObservationIgnored private var lastWatch = Date.now

    /// The folder of the downloads, made and kept out of the backup at the
    /// first use.
    nonisolated static let folder: URL = {
        let url = URL.applicationSupportDirectory.appending(path: "Downloads", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var marked = url
        try? marked.setResourceValues(values)
        return url
    }()

    private var indexFile: URL { Self.folder.appending(path: "index.json") }

    private init() {
        entries = load()
        makeSession()
        // Tasks that ran on while the app was not: take them back; an entry
        // left "downloading" with no task was ended by a force quit.
        session.getAllTasks { [weak self] tasks in
            Task { @MainActor in self?.adopt(tasks.compactMap { $0 as? URLSessionDownloadTask }) }
        }
        watchdog = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(15))
                self?.watch()
            }
        }
    }

    private func makeSession() {
        let config = URLSessionConfiguration.background(withIdentifier: Self.sessionId)
        config.sessionSendsLaunchEvents = true
        config.isDiscretionary = false
        // "1 or 2 at a time" (Settings › Downloads); the Mac's default is 1.
        parallel = Preferences.shared.downloadsAtOnce
        config.httpMaximumConnectionsPerHost = parallel
        session = URLSession(configuration: config, delegate: delegate, delegateQueue: nil)
    }

    private func adopt(_ running: [URLSessionDownloadTask]) {
        for task in running {
            if let key = DownloadDelegate.parts(task)?.key, entries.contains(where: { $0.id == key }) {
                tasks[key] = task
                lastBytes[key] = .now
            } else {
                task.cancel() // of a download removed meanwhile
            }
        }
        for index in entries.indices where entries[index].state == .downloading && tasks[entries[index].id] == nil {
            entries[index].state = .paused
        }
        save()
    }

    // MARK: The index, and its recovery

    /// The index; a lost or unreadable one is made again from the folder:
    /// each file with its record (`<file>.json`), or with its name alone
    /// (`<itemId>.<ext>`). Ids that are no safe file name are left out.
    private func load() -> [DownloadEntry] {
        let fm = FileManager.default
        var list: [DownloadEntry] = []
        var changed = false
        if let data = try? Data(contentsOf: indexFile) {
            if let saved = StoredFormat.decode([DownloadEntry].self, from: data) {
                list = saved.value
                // An index of an older build is written in this build's form.
                changed = saved.old
            } else {
                NSLog("DOWNLOADS: the index could not be read; it is made again from the folder")
                let aside = Self.folder.appending(path: "index.unreadable.json")
                try? fm.removeItem(at: aside)
                try? fm.moveItem(at: indexFile, to: aside)
                changed = true
            }
        }
        let before = list.count
        list.removeAll { !Self.safeId($0.item.id) || !Self.safeFile($0.file) || !($0.server.map(Self.safeId) ?? true) }
        changed = changed || list.count != before

        let names = (try? fm.contentsOfDirectory(atPath: Self.folder.path)) ?? []
        let known = Set(list.map(\.file))
        for name in names where !name.hasPrefix(".") && !name.hasSuffix(".json") && !known.contains(name) {
            guard Self.safeFile(name) else { continue }
            let size = Self.fileSize(Self.folder.appending(path: name))
            if var entry = Self.record(of: name), Self.safeId(entry.item.id) {
                entry.state = .done
                entry.received = size
                entry.total = size
                list.append(entry)
                changed = true
            } else if let stem = name.split(separator: ".").first.map(String.init), Self.safeId(stem),
                      !list.contains(where: { $0.item.id == stem && $0.server == nil }) {
                list.append(DownloadEntry(item: Self.unknownItem(stem), segments: [], file: name, url: "",
                                          state: .done, received: size, total: size, recovered: true))
                changed = true
            }
        }
        // A record whose download had not ended: it goes on from the start.
        for name in names where name.hasSuffix(".json") && !name.hasPrefix("index") {
            let file = String(name.dropLast(5))
            guard !names.contains(file), !list.contains(where: { $0.file == file }),
                  var entry = Self.record(of: file), Self.safeId(entry.item.id) else { continue }
            entry.state = .paused
            list.append(entry)
            changed = true
        }
        // A finished download whose file is gone.
        for index in list.indices where list[index].state == .done
            && !fm.fileExists(atPath: Self.folder.appending(path: list[index].file).path) {
            list[index].state = .failed
            list[index].reason = "The file is no longer on this device."
            list[index].received = 0
            changed = true
        }
        if changed { write(list) }
        return list
    }

    private static func record(of file: String) -> DownloadEntry? {
        guard let data = try? Data(contentsOf: folder.appending(path: "\(file).json")),
              let entry = StoredFormat.decode(DownloadEntry.self, from: data)?.value, entry.file == file else { return nil }
        return entry
    }

    /// The record beside the file (the token is never written).
    private func writeRecord(_ entry: DownloadEntry) {
        var record = entry
        record.resumeData = nil
        if let data = try? JSONEncoder().encode(record) {
            try? data.write(to: Self.folder.appending(path: "\(entry.file).json"),
                            options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        }
    }

    private nonisolated static func fileSize(_ url: URL) -> Int64 {
        Int64((try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
    }

    /// An item known by its id alone, until the server names it.
    private static func unknownItem(_ id: String) -> MediaItem {
        MediaItem(id: id, name: "Downloaded file", kind: .other(name: ""), collectionType: nil, year: nil, overview: nil,
                  officialRating: nil, communityRating: nil, runtimeSecs: nil, resumeSecs: 0, progress: nil,
                  played: false, favorite: false, unplayedCount: nil, seriesId: nil, seriesName: nil, seasonId: nil,
                  episodeCode: nil, title: "Downloaded file", playable: true, isSeries: false, genres: [],
                  poster: nil, wide: nil, backdrop: nil, logo: nil, trailer: nil, webUrl: nil, criticRating: nil,
                  seasonNumber: nil, episodeNumber: nil, qualityTags: [])
    }

    /// An id as the Mac allows it in a folder name: letters, digits and
    /// dashes (a server's ids are hex).
    nonisolated static func safeId(_ id: String) -> Bool {
        !id.isEmpty && id.count <= 128 && id.unicodeScalars.allSatisfy { $0.isASCII && (CharacterSet.alphanumerics.contains($0) || $0 == "-") }
    }

    /// One path component of the folder: no `/`, no `..`, no NUL.
    nonisolated static func safeFile(_ name: String) -> Bool {
        !name.isEmpty && name != "." && name != ".." && !name.hasPrefix(".") && !name.contains("/")
            && !name.contains("\\") && !name.contains("\0") && name != "index.json"
    }

    // MARK: Questions

    /// The server the app is signed in to; nil before it knows.
    var activeServer: String? { AppModel.current?.active?.serverId }

    /// Whether a download may open through the active server: its own, or
    /// one from before servers were recorded.
    func isOfActiveServer(_ entry: DownloadEntry) -> Bool {
        entry.server == nil || activeServer == nil || entry.server == activeServer
    }

    /// The download of an item of the active server.
    func entry(_ itemId: String) -> DownloadEntry? {
        let server = activeServer
        return entries.first { $0.item.id == itemId && $0.server == server }
            ?? entries.first { $0.item.id == itemId && isOfActiveServer($0) }
    }

    /// The file of a finished download of the active server, to play from
    /// this device; another server's file with the same id does not count.
    func localFile(_ itemId: String) -> URL? {
        guard let entry = entry(itemId), entry.state == .done, Self.safeFile(entry.file) else { return nil }
        let url = Self.folder.appending(path: entry.file)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    /// The bytes the downloads take, or will.
    var usedBytes: Int64 { entries.reduce(0) { $0 + $1.bytes } }

    nonisolated static var freeBytes: Int64? {
        try? URL.applicationSupportDirectory.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
            .volumeAvailableCapacityForImportantUsage
    }

    /// The storage limit of Settings › Downloads in bytes; nil for none.
    static var limitBytes: Int64? {
        let gb = Preferences.shared.downloadLimitGB
        return gb > 0 ? Int64(gb) * 1_000_000_000 : nil
    }

    private static func bytes(_ count: Int64) -> String { ByteCountFormatter.string(fromByteCount: count, countStyle: .file) }

    /// Why `more` bytes do not fit (the reserve of the device, the storage
    /// limit), or nil. The other running downloads count with what they
    /// have still to write (a season starts each episode before any answers).
    private func roomFor(_ more: Int64, besides key: String?) -> String? {
        let others = entries.filter { $0.id != key }
        if let disk = Self.freeBytes {
            let free = max(0, disk - others.reduce(Int64(0)) { $0 + $1.remaining })
            if more + Self.reserve > free {
                return "Not enough space on this device: it needs about \(Self.bytes(more)), and \(Self.bytes(free)) is free."
            }
        }
        if let limit = Self.limitBytes {
            let used = others.reduce(Int64(0)) { $0 + $1.expected }
            if used + more > limit {
                return "Over the storage limit of \(Self.bytes(limit)) (\(Self.bytes(used)) in use)."
            }
        }
        return nil
    }

    // MARK: Actions

    /// Starts a download from the active server; the sentence of a failure,
    /// or nil. `server` must be the active one: the entry records the
    /// active server's id.
    @discardableResult
    func download(_ item: MediaItem, server: Server, version: String? = nil) async -> String? {
        guard AppModel.current?.server === server else { return Self.serverChanged }
        guard entry(item.id) == nil else { return nil }
        guard Self.safeId(item.id) else { return "The item's id is not safe for a file name." }
        let serverId = activeServer
        if let serverId, !Self.safeId(serverId) { return "The server's id is not safe for a file name." }
        do {
            let info = try await server.downloadInfo(itemId: item.id, mediaSourceId: version)
            // A switch of server while the server answered: the id is stale.
            guard AppModel.current?.server === server, activeServer == serverId else { return Self.serverChanged }
            guard entry(item.id) == nil else { return nil } // a second tap while the first asked
            let estimate = Int64(info.estimatedBytes ?? 0)
            if let refused = roomFor(estimate, besides: nil) { return refused }
            var file = "\(item.id).\(Self.fileExtension(info.fileExtension))"
            // Another server's download of the same id holds the name.
            if entries.contains(where: { $0.file == file }), let serverId { file = "\(serverId)-\(file)" }
            guard !entries.contains(where: { $0.file == file }) else { return "This item is downloaded from another server." }
            var entry = DownloadEntry(item: info.item, segments: info.segments, file: file, url: info.url,
                                      authorization: info.authorization, state: .downloading, server: serverId)
            entry.estimate = estimate > 0 ? estimate : nil
            entry.mediaSourceId = version
            entries.append(entry)
            writeRecord(entry)
            start(entry, resumeData: nil)
            save()
            return nil
        } catch {
            return Words.error(error)
        }
    }

    private static func fileExtension(_ ext: String) -> String {
        let safe = !ext.isEmpty && ext.count <= 8 && ext.unicodeScalars.allSatisfy { $0.isASCII && CharacterSet.alphanumerics.contains($0) }
        return safe ? ext.lowercased() : "bin"
    }

    func pause(_ key: String) {
        stalls[key] = nil
        guard let task = tasks[key] else {
            // Waiting for a new session, or a task that never came back.
            waitingForSession.removeAll { $0.key == key }
            if let index = entries.firstIndex(where: { $0.id == key }), entries[index].state == .downloading {
                entries[index].state = .paused
                save()
            }
            return
        }
        tasks[key] = nil
        task.cancel { [weak self] data in
            Task { @MainActor in
                guard let self, let index = self.entries.firstIndex(where: { $0.id == key }) else { return }
                self.entries[index].state = .paused
                self.entries[index].resumeData = data
                self.save()
            }
        }
    }

    /// Resume (from where it was) or Try again (a failed download, or one
    /// with no address left: the server is asked again and the file comes
    /// anew). The sentence of a failure, or nil.
    @discardableResult
    func resume(_ key: String, server: Server?) async -> String? {
        guard let index = entries.firstIndex(where: { $0.id == key }), entries[index].state != .done,
              entries[index].state != .downloading else { return nil }
        let entry = entries[index]
        let fresh = entry.state == .failed || entry.url.isEmpty
        // A paused download with no resume data after a start of the app
        // asks the server for the header again and goes on from its address.
        if !fresh, entry.resumeData == nil, entry.authorization.isEmpty {
            guard isOfActiveServer(entry) else { return Self.otherServer }
            guard let server else { return "The server cannot be reached." }
            do {
                let info = try await server.downloadInfo(itemId: entry.item.id, mediaSourceId: entry.mediaSourceId)
                guard let index = entries.firstIndex(where: { $0.id == key }) else { return nil }
                entries[index].authorization = info.authorization
            } catch {
                return Words.error(error)
            }
            return await resume(key, server: server)
        }
        if !fresh {
            entries[index].state = .downloading
            entries[index].reason = nil
            let data = entries[index].resumeData
            entries[index].resumeData = nil
            start(entries[index], resumeData: data)
            save()
            return nil
        }
        guard isOfActiveServer(entry) else { return Self.otherServer }
        guard let server else { return "The server cannot be reached." }
        do {
            let info = try await server.downloadInfo(itemId: entry.item.id, mediaSourceId: entry.mediaSourceId)
            guard let index = entries.firstIndex(where: { $0.id == key }), entries[index].state != .downloading else { return nil }
            let estimate = Int64(info.estimatedBytes ?? 0)
            if let refused = roomFor(estimate, besides: key) { return refused }
            if entries[index].state == .done { return nil }
            entries[index].estimate = estimate > 0 ? estimate : nil
            entries[index].item = info.item
            entries[index].segments = info.segments
            entries[index].url = info.url
            entries[index].authorization = info.authorization
            entries[index].server = entries[index].server ?? activeServer
            entries[index].state = .downloading
            entries[index].reason = nil
            entries[index].resumeData = nil
            entries[index].size = nil
            entries[index].received = 0
            entries[index].total = 0
            entries[index].recovered = nil
            writeRecord(entries[index])
            start(entries[index], resumeData: nil)
            save()
            return nil
        } catch {
            return Words.error(error)
        }
    }

    static let serverChanged = "The server changed before the download started."
    static let otherServer = "This download is of another server. Sign in to that server to open it."

    /// Removes the download of an item of the active server.
    func remove(_ itemId: String) {
        if let entry = entry(itemId) { remove(key: entry.id) }
    }

    func remove(key: String) {
        tasks[key]?.cancel()
        tasks[key] = nil
        waitingForSession.removeAll { $0.key == key }
        forget(key)
        if let entry = entries.first(where: { $0.id == key }), Self.safeFile(entry.file),
           !entries.contains(where: { $0.id != key && $0.file == entry.file }) {
            try? FileManager.default.removeItem(at: Self.folder.appending(path: entry.file))
            try? FileManager.default.removeItem(at: Self.folder.appending(path: "\(entry.file).json"))
        }
        entries.removeAll { $0.id == key }
        save()
    }

    /// The details of downloads made again from the folder, from the
    /// active server (once it answers for the item).
    func refreshRecovered(_ server: Server?) async {
        guard let server else { return }
        for key in entries.filter({ $0.recovered == true && isOfActiveServer($0) }).map(\.id) {
            guard let entry = entries.first(where: { $0.id == key }),
                  let info = try? await server.downloadInfo(itemId: entry.item.id, mediaSourceId: entry.mediaSourceId),
                  let index = entries.firstIndex(where: { $0.id == key }) else { continue }
            entries[index].item = info.item
            entries[index].segments = info.segments
            entries[index].url = info.url
            entries[index].authorization = info.authorization
            entries[index].server = entries[index].server ?? activeServer
            entries[index].recovered = nil
            writeRecord(entries[index])
            save()
        }
    }

    private func start(_ entry: DownloadEntry, resumeData: Data?) {
        if rebuilding {
            waitingForSession.append((entry.id, resumeData))
            return
        }
        let task: URLSessionDownloadTask
        if let resumeData {
            task = session.downloadTask(withResumeData: resumeData)
        } else {
            guard let url = URL(string: entry.url) else { return }
            var request = URLRequest(url: url)
            request.setValue(entry.authorization, forHTTPHeaderField: "Authorization")
            request.allowsCellularAccess = Preferences.shared.downloadOverCellular
            request.allowsConstrainedNetworkAccess = false
            task = session.downloadTask(with: request)
        }
        task.taskDescription = "\(entry.id)|\(entry.file)"
        tasks[entry.id] = task
        lastBytes[entry.id] = .now
        noNetwork.remove(entry.id)
        task.resume()
    }

    private func forget(_ key: String) {
        lastBytes[key] = nil
        stalls[key] = nil
        noNetwork.remove(key)
    }

    private func fail(_ key: String, _ reason: String, resumeData: Data? = nil) {
        tasks[key]?.cancel()
        tasks[key] = nil
        forget(key)
        guard let index = entries.firstIndex(where: { $0.id == key }) else { return }
        entries[index].state = .failed
        entries[index].reason = reason
        entries[index].resumeData = resumeData
        save()
    }

    // MARK: "1 or 2 at a time"

    /// Makes the session again when "at a time" changed: a background
    /// session takes its settings when it is made. The transfers pause with
    /// their resume data, the old session ends, and they go on in the new.
    func applyParallel() {
        guard Preferences.shared.downloadsAtOnce != parallel, !rebuilding else { return }
        rebuilding = true
        let running = tasks
        tasks = [:]
        Task {
            for (key, task) in running {
                let data = await task.cancelByProducingResumeData()
                waitingForSession.append((key, data))
            }
            session.finishTasksAndInvalidate()
        }
    }

    fileprivate func sessionEnded() {
        guard rebuilding else { return }
        makeSession()
        rebuilding = false
        let waiting = waitingForSession
        waitingForSession = []
        for (key, data) in waiting {
            guard let entry = entries.first(where: { $0.id == key }), entry.state == .downloading else { continue }
            start(entry, resumeData: data)
        }
        applyParallel() // changed again meanwhile
    }

    // MARK: Stalls

    /// A transfer with no byte for `stallLimit` is made again from where it
    /// was; after `stallsAtMost` in a row it fails. One that waits for a
    /// network, or a server that cannot be reached, is no stall.
    private func watch() {
        // Back from a suspension: the bytes of the time between were not
        // told to the app, so the clocks start again.
        if Date.now.timeIntervalSince(lastWatch) > 40 { for key in tasks.keys { lastBytes[key] = .now } }
        lastWatch = .now
        guard !rebuilding else { return }
        // No network, or a path the request may not use (cellular with
        // "Download over cellular" off, Low Data Mode): the task waits, and
        // a background session never says so (`taskIsWaitingForConnectivity`
        // is not called for one). A wait is no stall; the clocks start again.
        guard AppModel.current?.offline != true, Self.pathCarries else {
            for key in tasks.keys { lastBytes[key] = .now }
            return
        }
        for (key, task) in tasks where task.state == .running && !noNetwork.contains(key) {
            guard let last = lastBytes[key], Date.now.timeIntervalSince(last) > Self.stallLimit else { continue }
            let count = (stalls[key] ?? 0) + 1
            NSLog("DOWNLOADS: %@ stalled (%d)", key, count)
            tasks[key] = nil
            task.cancel { [weak self] data in
                Task { @MainActor in
                    guard let self, let entry = self.entries.first(where: { $0.id == key }), entry.state == .downloading else { return }
                    if count >= Self.stallsAtMost {
                        let minutes = Int(Self.stallLimit) * count / 60
                        self.fail(key, "No data came from the server for about \(max(1, minutes)) minutes.", resumeData: data)
                    } else if data == nil, entry.authorization.isEmpty {
                        // A transfer of an earlier run with no resume data:
                        // the header comes from the server first.
                        self.pause(key)
                        self.stalls[key] = count
                        Task { await self.resume(key, server: AppModel.current?.server) }
                    } else {
                        self.start(entry, resumeData: data)
                        self.stalls[key] = count
                    }
                }
            }
        }
    }

    /// Whether the current network path can carry a download request
    /// (`start` sets `allowsCellularAccess` from the preference and refuses
    /// constrained paths). An unknown path counts as usable.
    private static var pathCarries: Bool {
        guard let path = LinkWatch.shared.path else { return true }
        if path.status != .satisfied || path.isConstrained { return false }
        if path.usesInterfaceType(.cellular), !Preferences.shared.downloadOverCellular { return false }
        return true
    }

    // MARK: From the delegate (main actor)

    fileprivate func progressed(_ key: String, received: Int64, total: Int64) {
        guard let index = entries.firstIndex(where: { $0.id == key }), entries[index].state == .downloading else { return }
        lastBytes[key] = .now
        stalls[key] = nil
        noNetwork.remove(key)
        if total > 0 {
            if let size = entries[index].size, size != total {
                // The server sends another file than the one this began
                // with (If-Range found the old one gone).
                NSLog("DOWNLOADS: %@ changed on the server (%lld to %lld bytes)", key, size, total)
                fail(key, "The file on the server changed (\(Self.bytes(size)), now \(Self.bytes(total))). Try again downloads it anew.")
                return
            }
            if entries[index].size == nil {
                entries[index].size = total
                if let refused = roomFor(total - received, besides: key) {
                    entries[index].total = total
                    fail(key, refused)
                    return
                }
            }
        }
        entries[index].received = received
        entries[index].total = total
        if Date.now.timeIntervalSince(lastSave) > 5 { save() }
    }

    fileprivate func waitsForNetwork(_ key: String) { noNetwork.insert(key) }

    fileprivate func finished(_ key: String, ok: Bool, reason: String?, resumeData: Data?) {
        guard let index = entries.firstIndex(where: { $0.id == key }) else { return }
        // A background relaunch: `adopt` may mark the entry paused before
        // this hop arrives; the file is in place, so it is done.
        guard entries[index].state == .downloading || (ok && entries[index].state == .paused) else { return }
        tasks[key] = nil
        forget(key)
        if ok {
            entries[index].state = .done
            entries[index].reason = nil
            entries[index].resumeData = nil
            let size = Self.fileSize(Self.folder.appending(path: entries[index].file))
            entries[index].received = max(size, entries[index].received)
            entries[index].total = entries[index].received
            writeRecord(entries[index])
        } else {
            entries[index].state = .failed
            entries[index].reason = reason
            entries[index].resumeData = resumeData
        }
        save()
    }

    private func save() {
        lastSave = .now
        write(entries)
    }

    private func write(_ list: [DownloadEntry]) {
        if let data = try? JSONEncoder().encode(list) {
            try? data.write(to: indexFile, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        }
    }
}

/// The places of downloads played while the server could not be reached
/// (inventory §3.11): one per item, the last; sent at the next connection
/// as a Stopped report, which sets the item's resume point.
enum OfflinePositions {
    struct Entry: Codable {
        let itemId: String
        let mediaSourceId: String
        let secs: Double
    }

    private static let key = "offlinePositions"

    static var all: [Entry] {
        guard let data = UserDefaults.standard.data(forKey: key) else { return [] }
        return (try? JSONDecoder().decode([Entry].self, from: data)) ?? []
    }

    private static func save(_ entries: [Entry]) {
        UserDefaults.standard.set(try? JSONEncoder().encode(entries), forKey: key)
    }

    static func record(itemId: String, mediaSourceId: String, secs: Double) {
        var entries = all.filter { $0.itemId != itemId }
        entries.append(Entry(itemId: itemId, mediaSourceId: mediaSourceId, secs: secs))
        save(entries)
    }

    /// Sends what waits; an entry that fails stays for the next time.
    static func flush(_ server: Server) async {
        var left: [Entry] = []
        for entry in all {
            do {
                try await server.report(kind: .stopped, infoItemId: entry.itemId, mediaSourceId: entry.mediaSourceId,
                                        playSessionId: UUID().uuidString.lowercased(), playMethod: .directPlay,
                                        positionSecs: entry.secs, paused: true)
            } catch {
                left.append(entry)
            }
        }
        save(left)
    }
}

/// URLSession's delegate. It runs on the session's queue: the file is moved
/// into place before `didFinishDownloadingTo` returns (Apple), then the
/// state goes to the main actor.
nonisolated final class DownloadDelegate: NSObject, URLSessionDownloadDelegate, Sendable {
    /// The completion handler of a background relaunch, called when the
    /// session has delivered its events.
    static let backgroundCompletion = Mutex<(@Sendable () -> Void)?>(nil)

    /// The entry's key and file of a task ("key|file"). A task of a build
    /// before servers were recorded has the item id alone for the key.
    static func parts(_ task: URLSessionTask) -> (key: String, file: String)? {
        let parts = task.taskDescription?.split(separator: "|", maxSplits: 1).map(String.init) ?? []
        guard parts.count == 2, Downloads.safeFile(parts[1]) else { return nil }
        let key = parts[0].contains("/") ? parts[0] : DownloadEntry.key(server: nil, item: parts[0])
        return (key, parts[1])
    }

    /// The words for an answer that is not the file.
    private static func refusal(_ status: Int) -> String {
        switch status {
        case 404, 410: "The file is no longer on the server. Try again downloads it anew."
        case 416: "The file on the server changed. Try again downloads it anew."
        case 401, 403: "The server refused the download (\(status)). Sign in again, then try again."
        default: "The server answered \(status) instead of the file."
        }
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData _: Int64,
                    totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64) {
        guard let (key, _) = Self.parts(downloadTask) else { return }
        Task { @MainActor in
            Downloads.shared.progressed(key, received: totalBytesWritten, total: totalBytesExpectedToWrite)
        }
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didResumeAtOffset fileOffset: Int64,
                    expectedTotalBytes: Int64) {
        guard let (key, _) = Self.parts(downloadTask) else { return }
        Task { @MainActor in Downloads.shared.progressed(key, received: fileOffset, total: expectedTotalBytes) }
    }

    func urlSession(_ session: URLSession, taskIsWaitingForConnectivity task: URLSessionTask) {
        guard let (key, _) = Self.parts(task) else { return }
        Task { @MainActor in Downloads.shared.waitsForNetwork(key) }
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        guard let (key, file) = Self.parts(downloadTask) else { return }
        let status = (downloadTask.response as? HTTPURLResponse)?.statusCode ?? 0
        // 206: the rest of a resumed download.
        guard (200..<300).contains(status) else {
            Task { @MainActor in Downloads.shared.finished(key, ok: false, reason: Self.refusal(status), resumeData: nil) }
            return
        }
        let target = Downloads.folder.appending(path: file)
        try? FileManager.default.removeItem(at: target)
        do {
            try FileManager.default.moveItem(at: location, to: target)
            Task { @MainActor in Downloads.shared.finished(key, ok: true, reason: nil, resumeData: nil) }
        } catch {
            let reason = "The file could not be saved: \(error.localizedDescription)"
            Task { @MainActor in Downloads.shared.finished(key, ok: false, reason: reason, resumeData: nil) }
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: (any Error)?) {
        guard let (key, _) = Self.parts(task) else { return }
        let status = (task.response as? HTTPURLResponse)?.statusCode ?? 0
        if error == nil {
            // The file was handled in `didFinishDownloadingTo`; an answer
            // that is not the file is said there as well (twice is harmless).
            if !(200..<300).contains(status) {
                Task { @MainActor in Downloads.shared.finished(key, ok: false, reason: Self.refusal(status), resumeData: nil) }
            }
            return
        }
        if (error as? URLError)?.code == .cancelled { return } // a pause, a removal, a stall, a new session
        let urlError = error as? URLError
        let data = urlError?.userInfo[NSURLSessionDownloadTaskResumeData] as? Data
        let reason = error.map { "The download stopped: \($0.localizedDescription)" }
        Task { @MainActor in Downloads.shared.finished(key, ok: false, reason: reason, resumeData: data) }
    }

    func urlSession(_ session: URLSession, didBecomeInvalidWithError error: (any Error)?) {
        Task { @MainActor in Downloads.shared.sessionEnded() }
    }

    func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        let completion = Self.backgroundCompletion.withLock { held in
            defer { held = nil }
            return held
        }
        Task { @MainActor in completion?() }
    }
}
