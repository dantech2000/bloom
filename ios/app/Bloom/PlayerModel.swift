import AVFoundation
import BloomCore
import MediaAccessibility
import MediaPlayer
import MPVUI
import SwiftUI
import UIKit

/// The state of the player and what it does (spec §8.3, §8.10). mpv plays
/// what the server decided (`PlaybackInfo`); this model reports to the
/// server, keeps the Lock Screen up to date, and follows the life cycle.
@Observable
final class PlayerModel {
    var state = Mpv.State()
    var info: PlayInfo?
    var error: String?
    var controls = true {
        didSet { liftSubtitles() }
    }
    /// The cap the Quality menu chose; none follows the setting.
    var quality: UInt64? = Preferences.shared.cap
    /// The stalls of the player under Auto, and the cap they set (it holds
    /// across items until the link is measured again; core's rules).
    static let stalls = StallWatch()
    /// The cap the last stream was asked with, for the bitrate that plays.
    private var sentCap: UInt64?
    /// The episodes of the current season, for the picker.
    var seasonEpisodes: [MediaItem] = []
    /// Chapters and preview frames of the item (the server's timeline).
    var timeline: Timeline?
    /// Decoded preview sheets, by number.
    private var sheets: [UInt32: CGImage] = [:]
    private var sheetsLoading: Set<UInt32> = []
    var showEpisodes = false
    var showBookmarks = false
    var showSyncPlay = false
    var showSubtitleSearch = false
    /// mpv's tracks, apart from the state that changes with the time: the
    /// menus read these, and a menu that is redrawn closes.
    private(set) var tracks: [Mpv.Track] = []
    /// The server's tracks of the item that plays, with their flags.
    private(set) var fileTracks: [TrackInfo] = []
    @ObservationIgnored private var fileTracksFor: String?
    /// The files of the item that plays (its versions), its own first.
    private(set) var versions: [MediaVersion] = []
    /// The version chosen for an item (a media source id): on the detail
    /// page or in the settings menu. It holds over the reloads of that
    /// item (a quality change) and names nothing for another.
    @ObservationIgnored private var version: (itemId: String, sourceId: String)?
    /// The version that plays.
    var currentVersion: MediaVersion? {
        versions.first { $0.id == info?.mediaSourceId } ?? versions.first(where: \.isDefault)
    }
    /// The Play on sheet over the video.
    var showPlayOn = false
    var scrubbing: Double?
    /// A server subtitle being fetched, by its index.
    var fetchingSubtitle: Int64?
    /// Closed by the end of the last item.
    var finished = false {
        didSet { if finished, !oldValue { onFinish?() } }
    }
    /// The app's closer, called once the player is done.
    @ObservationIgnored var onFinish: (() -> Void)?
    /// The user's settings on the server: autoplay, the Up next card, the
    /// skip lengths.
    @ObservationIgnored weak var prefs: ServerPrefs?
    var skipBack: Double { prefs?.skipSecs("skipBackLength") ?? 10 }
    var skipForward: Double { prefs?.skipSecs("skipForwardLength") ?? 30 }
    var muted = false
    /// The speed, 0.5 to 2 (not kept: each film starts at 1).
    var speed = 1.0
    /// Seconds the audio and the subtitles are moved, for this item.
    var audioDelay = 0.0
    var subtitleDelay = 0.0
    /// The pause screen: the title and overview after 5 s of pause.
    var pauseScreen = false
    @ObservationIgnored private var pauseWait: Task<Void, Never>?
    /// The playback info card, and its lines while it shows.
    var showInfo = false {
        didSet { if showInfo { refreshInfo() } }
    }
    var infoLines: [(String, String)] = []
    /// The item's bookmarks, for the marks on the seek bar.
    var bookmarks: [Bookmark] = []
    /// The favourite mark of the item playing, as the button shows it.
    var favorite = false
    /// The items that play after this one (Play All, Shuffle, a playlist);
    /// empty, the server's next episode follows.
    var queue: [MediaItem] = []
    /// The items of the queue played before, for Previous.
    private var history: [MediaItem] = []
    /// The request the player opened with.
    private(set) var startRequest: PlayRequest?
    /// The next load asks the server to burn the chosen subtitle in (an
    /// image subtitle whose file did not come; once per item).
    private var burnIn = false
    private var burnedIn: Set<String> = []
    /// The tracks chosen on the detail page, for the first item.
    private var chosenAudio: Int64?
    private var chosenSubtitle: Int64?
    /// The item `chosenAudio`, `chosenSubtitle` and `burnIn` were set for.
    private var choicesFor: String?
    /// What plays next: the queue's first, or the next episode.
    var nextItem: MediaItem? { queue.first ?? info?.next }
    /// "Hide" on the Up next card.
    var upNextHidden = false
    /// "Are you still watching?" shows over the paused next episode.
    var stillWatchingAsk = false
    /// A frame step runs: mpv plays one frame and pauses again, so the
    /// paused state flickers; the step buttons stay meanwhile.
    var frameStepping = false
    @ObservationIgnored private var frameStepToken = 0
    /// The session of that question (core's rule, as on the Mac).
    @ObservationIgnored private let stillWatching = StillWatching()

    private var mpv: Mpv?
    /// MPVUI's player, for the video view and Picture in Picture.
    var engine: MPVPlayer? { mpv?.player }
    private var server: Server?
    private var reporter: Task<Void, Never>?
    private var hideTask: Task<Void, Never>?
    /// The seek of a scrub whose value rests (`scrub(to:)`).
    private var scrubSettled: Task<Void, Never>?
    /// The system's notifications (interruptions, the route, background),
    /// each a task over its stream; the close cancels them.
    private var systemTasks: [Task<Void, Never>] = []
    /// The info card's loop, once a second while it shows.
    private var infoTask: Task<Void, Never>?
    private var artwork: MPMediaItemArtwork?
    /// When the Lock Screen's Now Playing was last written.
    private var nowPlayingAt = Date.distantPast
    /// The current item was started again from 0 after its resume point
    /// turned out to lie past its end; once only.
    private var restarted = false
    /// The segments skipped without a tap, by their start, once each.
    private var autoSkipped: Set<Double> = []
    /// Bumped by each load and by the close: an answer for an older one
    /// is dropped. The tasks a load starts check `isCurrent` instead.
    private var loadToken = 0
    /// The tracks of the item that plays, kept over a reload of it (a
    /// quality change, a stall step-down): mpv ids, and the files added.
    private var keptTracks: (itemId: String, aid: String?, sid: String?, files: [(path: String, title: String)])?
    /// The load named its tracks: the timeline's defaults do not apply.
    private var explicitTracks = false
    /// The item plays with its subtitle burned in by the server.
    private var burnedInNow = false
    /// Subtitle files added beside the file that plays; a restart of the
    /// file (`loadfile`) drops them, so they go back once it is ready.
    private var addedSubtitles: [(path: String, title: String)] = []
    private var subtitlesToRestore: [(path: String, title: String)] = []
    /// The items looked for again after a failed load, once each.
    private var refound: Set<String> = []
    /// A menu of the bar is open: the controls stay until a choice is made
    /// or the video is tapped (2026-11-14: the timer hid them, and the open
    /// menu went with them as the user scrolled in it).
    private var holding = false
    /// Whether the film played when AirPlay began to prepare (the route
    /// button starts it), so it goes on playing on the TV.
    private var playingBeforeRoute = false

    // MARK: Start, next, close

    func start(_ request: PlayRequest, server: Server?) async {
        guard mpv == nil else { return }
        guard let server else {
            SyncBridge.shared.loadFailedHere()
            error = "No server to play from."
            return
        }
        self.server = server
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .moviePlayback)
        try? AVAudioSession.sharedInstance().setActive(true)
        UIApplication.shared.isIdleTimerDisabled = true
        let muted = ProcessInfo.processInfo.environment["BLOOM_MUTED"] == "1"
        self.muted = muted
        let mpv = Mpv(muted: muted, onChange: { [weak self] state in
            self?.changed(state)
        }, onEnd: { [weak self] in
            self?.ended()
        })
        self.mpv = mpv
        mpv.onStall = { [weak self] on in self?.stallChanged(on) }
        mpv.onReload = { [weak self] in
            guard let self else { return }
            self.subtitlesToRestore += self.addedSubtitles
            self.addedSubtitles = []
        }
        SyncBridge.shared.attach(mpv)
        applySubtitleLook()
        startRequest = request
        queue = request.queue
        stillWatching.begin(itemId: request.item.id, episode: request.item.kind == .episode)
        chosenAudio = request.audioIndex
        chosenSubtitle = request.subtitleIndex
        choicesFor = request.item.id
        version = request.mediaSourceId.map { (itemId: request.item.id, sourceId: $0) }
        observeSystem()
        watchExternalPlayback(mpv.player)
        remoteCommands()
        await load(itemId: request.item.id, fromStart: request.fromStart, at: request.startSecs, paused: request.paused)
    }

    /// Asks the server how to play an item and loads it; `at` reloads at a
    /// place (a change of quality).
    private func load(itemId: String, fromStart: Bool, at: Double? = nil, paused: Bool = false) async {
        guard let server, let mpv else {
            SyncBridge.shared.loadFailedHere()
            return
        }
        loadToken += 1
        let token = loadToken
        // Choices made for another item (an overtaken load kept them) do
        // not apply to this one.
        if choicesFor != itemId {
            chosenAudio = nil
            chosenSubtitle = nil
            burnIn = false
        }
        do {
            let (info, cap) = try await playInfo(server: server, itemId: itemId, fromStart: fromStart, at: at)
            // A later load or the close came meanwhile: this answer's
            // session ends at once (no transcode for a stream nobody reads).
            guard token == loadToken, self.mpv === mpv else {
                NSLog("PLAYER: a load of %@ was overtaken; its session ends", info.itemId)
                // Nothing played: no Stopped (it would set a resume point),
                // only the end of a transcode the server started.
                if info.transcoding { try? await server.endTranscode(playSessionId: info.playSessionId) }
                return
            }
            sentCap = cap
            apply(info, to: mpv, paused: paused)
        } catch {
            guard token == loadToken else { return }
            await recover(from: error, itemId: itemId, token: token, fromStart: fromStart, at: at, paused: paused)
        }
    }

    /// The answer of the current load: the item goes to mpv, and what
    /// belongs to the item that played before is reset.
    private func apply(_ info: PlayInfo, to mpv: Mpv, paused: Bool) {
        // The stalls of another item count no more.
        if self.info?.itemId != info.itemId { Self.stalls.clear() }
        self.info = info
        upNextHidden = false
        // Subtitle files of the same item go back after a reload.
        let keep = keptTracks?.itemId == info.itemId ? keptTracks : nil
        keptTracks = nil
        subtitlesToRestore = keep?.files ?? []
        addedSubtitles = []
        favorite = info.item.favorite
        // A delay belongs to one file (the Mac keeps it per item).
        audioDelay = 0
        subtitleDelay = 0
        mpv.set(property: "audio-delay", "0")
        mpv.set(property: "sub-delay", "0")
        mpv.set(property: "speed", String(speed))
        state = Mpv.State()
        // The chosen tracks, as options for this file (auto otherwise).
        explicitTracks = keep != nil || info.audioMpvId != nil || info.subtitleIndex != nil
        mpv.set(property: "aid", keep?.aid ?? info.audioMpvId.map { "\($0)" } ?? "auto")
        mpv.set(property: "sid", keep?.sid ?? info.subtitleMpvId.map { $0 == 0 ? "no" : "\($0)" } ?? "auto")
        mpv.load(url: info.url, header: info.httpHeader, start: info.startSecs, paused: paused)
        state.paused = paused
        SyncBridge.shared.loadIssued(itemId: info.itemId)
        NSLog("PLAYER: load %@; chosen audio %@ subtitle %@ (mpv %@/%@), %d server files", info.itemId,
              String(describing: chosenAudio), String(describing: info.subtitleIndex),
              String(describing: info.audioMpvId), String(describing: info.subtitleMpvId), info.externalSubtitles.count)
        // A subtitle in a file of its own comes from the server apart.
        if let wanted = info.subtitleIndex, wanted >= 0, info.subtitleMpvId == nil,
           let file = info.externalSubtitles.first(where: { $0.index == wanted }) {
            Task { await selectServerSubtitle(file) }
        }
        // The choice is for the item it was made on.
        chosenAudio = nil
        chosenSubtitle = nil
        burnedInNow = burnIn
        burnIn = false
        restarted = false
        autoSkipped = []
        liftSubtitles()
        Task { await loadSeason(info) }
        // Another item's tracks name nothing here; a reload keeps them.
        // Another version is another file, with tracks of its own.
        let file = info.itemId + "/" + info.mediaSourceId
        if fileTracksFor != file {
            if fileTracksFor?.hasPrefix(info.itemId + "/") != true { versions = [] }
            fileTracks = []
            fileTracksFor = file
            Task { await loadFile(info) }
        }
        Task { await loadBookmarks() }
        Task { await loadTimeline(info) }
        report(.start)
        reporter?.cancel()
        reporter = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(10))
                self?.report(.progress)
                self?.autoStepUp()
            }
        }
        loadArtwork(info)
        updateNowPlaying()
        hideSoon()
    }

    /// A load that failed. An episode the server replaced (a new release)
    /// has a new id: the one with the same numbers in the series takes its
    /// place, once. Anything else shows the error.
    private func recover(from error: Error, itemId: String, token: Int, fromStart: Bool, at: Double?, paused: Bool) async {
        if let server, !refound.contains(itemId), let lost = lostItem(itemId), let series = lost.seriesId,
           let found = try? await server.refindEpisode(lostId: itemId, seriesId: series, season: lost.seasonNumber, episode: lost.episodeNumber) {
            // A later load or the close came while the server looked.
            guard token == loadToken else { return }
            refound.insert(itemId)
            NSLog("PLAYER: %@ is gone; playing %@ instead", itemId, found.id)
            await load(itemId: found.id, fromStart: fromStart, at: at, paused: paused)
            return
        }
        guard token == loadToken else { return }
        dropCarried()
        // The old item's Stopped went out: no progress after it.
        reporter?.cancel()
        mpv?.setPaused(true)
        self.error = Words.error(error)
    }

    /// Whether an answer belongs to the load that plays: every task a load
    /// starts checks it before it writes.
    private func isCurrent(_ info: PlayInfo) -> Bool {
        self.info?.playSessionId == info.playSessionId
    }

    /// A load that failed: what was meant for it goes with it.
    private func dropCarried() {
        keptTracks = nil
        chosenAudio = nil
        chosenSubtitle = nil
        burnIn = false
        SyncBridge.shared.loadFailedHere()
    }

    /// The item that plays again at its place, as it was: paused or not,
    /// the same tracks (unless `chosen` names new ones by the server's
    /// index), burned in if it was.
    private func reloadHere(chosen: Bool = false) async {
        guard let info else { return }
        if !chosen {
            let audio = tracks.first { $0.kind == .audio && $0.selected }
            let sub = tracks.first { $0.kind == .sub && $0.selected }
            // On a direct play mpv's ids are the file's and hold; a
            // transcode's tracks are the server's choice for the load.
            let direct = !info.transcoding
            keptTracks = (itemId: info.itemId, aid: direct ? audio.map { "\($0.id)" } : nil,
                          sid: direct && sub?.external != true ? (sub.map { "\($0.id)" } ?? "no") : nil,
                          files: addedSubtitles)
            if burnedInNow {
                burnIn = true
                chosenSubtitle = info.subtitleIndex
                choicesFor = info.itemId
            }
        }
        let at = state.position, paused = state.paused
        await endSession()
        await load(itemId: info.itemId, fromStart: false, at: at, paused: paused)
    }

    /// What the player knew of an item that failed to load.
    private func lostItem(_ id: String) -> MediaItem? {
        if info?.itemId == id { return info?.item }
        if let request = startRequest, request.item.id == id { return request.item }
        return seasonEpisodes.first { $0.id == id } ?? queue.first { $0.id == id } ?? history.first { $0.id == id }
    }

    /// How to play an item: a downloaded file plays from this device (the
    /// server, when it answers, still gives the segments and the reports);
    /// offline, from what the download kept. With the cap the stream was
    /// asked with, which `load` keeps once the answer is its own.
    private func playInfo(server: Server, itemId: String, fromStart: Bool, at: Double?) async throws -> (PlayInfo, UInt64?) {
        // The version chosen for this item. A download plays from this
        // device when nothing is chosen or it is the version chosen; another
        // version comes from the server.
        let source = version?.itemId == itemId ? version?.sourceId : nil
        let saved = Downloads.shared.entry(itemId)
        guard source == nil || source == saved?.mediaSourceId,
              let file = Downloads.shared.localFile(itemId), let entry = saved else {
            // On cellular the cellular limit too, the lower of the two
            // (spec Q11).
            let cellular = Preferences.shared.cellularCap
            let base = requestCap(chosen: quality, cellular: cellular, onCellular: LinkWatch.shared.onCellular)
            if base != quality { NSLog("PLAYER: cellular: the stream is capped at %@", bitrateLabel(bitrate: base ?? 0)) }
            let info = try await server.playInfo(itemId: itemId, fromStart: fromStart, startSecs: at, maxBitrate: base,
                                                 audioIndex: chosenAudio, subtitleIndex: chosenSubtitle, burnIn: burnIn,
                                                 mediaSourceId: source)
            // Auto on a slow link: the rung the link carries, and under it
            // the cap that stalls set (core's rules); never over the
            // cellular limit.
            guard quality == nil, Preferences.shared.lowerOnSlowLink, let app = AppModel.current else { return (info, base) }
            let decided = autoCap(measuredBps: app.linkBps, fileBps: info.sourceBitrate, codec: nil, previous: app.autoCap)
            if let decided { app.autoCap = decided }
            guard let cap = Self.stalls.cap(decided: decided, fileBps: info.sourceBitrate),
                  base.map({ cap < $0 }) ?? true else { return (info, base) }
            NSLog("PLAYER: Auto lowers the stream to %@%@", bitrateLabel(bitrate: cap), cap == decided ? "" : " (after stalls)")
            let lowered = try await server.playInfo(itemId: itemId, fromStart: fromStart, startSecs: at, maxBitrate: cap,
                                                    audioIndex: chosenAudio, subtitleIndex: chosenSubtitle, burnIn: burnIn,
                                                    mediaSourceId: source)
            return (lowered, cap)
        }
        let start = at ?? (fromStart ? 0 : Double(entry.item.resumeSecs))
        if var online = try? await server.playInfo(itemId: itemId, fromStart: fromStart, startSecs: at, maxBitrate: nil,
                                                   audioIndex: chosenAudio, subtitleIndex: chosenSubtitle, burnIn: burnIn,
                                                   mediaSourceId: entry.mediaSourceId) {
            online.url = file.path
            online.httpHeader = ""
            online.transcoding = false
            online.playMethod = .directPlay
            online.rungs = []
            return (online, nil)
        }
        let offline = PlayInfo(url: file.path, httpHeader: "", itemId: itemId, mediaSourceId: entry.mediaSourceId ?? itemId,
                        playSessionId: UUID().uuidString.lowercased(), playMethod: .directPlay, transcoding: false,
                        startSecs: start, title: entry.item.title, item: entry.item, segments: entry.segments,
                        externalSubtitles: [], next: nil, rungs: [], sourceBitrate: nil,
                        audioMpvId: nil, subtitleMpvId: nil, subtitleIndex: nil)
        return (offline, nil)
    }

    /// The episode before this one in its season.
    var previousEpisode: MediaItem? {
        if let last = history.last { return last }
        guard let id = info?.itemId, let index = seasonEpisodes.firstIndex(where: { $0.id == id }), index > 0 else { return nil }
        return seasonEpisodes[index - 1]
    }

    /// Previous: back to the start, or to the episode before in the first
    /// 5 s (the Mac's rule).
    func previous() async {
        if let group = SyncBridge.shared.group {
            group.user(intent: .previous)
            return
        }
        if state.position > 5 || previousEpisode == nil {
            seek(to: 0)
        } else if let item = previousEpisode {
            if history.last?.id == item.id {
                history.removeLast()
                if let current = info?.item { queue.insert(current, at: 0) }
                await play(item, fromStart: true)
            } else {
                await play(item)
            }
        }
    }

    func setSpeed(_ speed: Double) {
        // A group plays at one speed.
        guard SyncBridge.shared.group == nil else { return }
        self.speed = speed
        mpv?.set(property: "speed", String(speed))
        updateNowPlaying()
    }

    func setAudioDelay(_ secs: Double) {
        audioDelay = secs
        mpv?.set(property: "audio-delay", String(secs))
    }

    func setSubtitleDelay(_ secs: Double) {
        subtitleDelay = secs
        mpv?.set(property: "sub-delay", String(secs))
    }

    func toggleFavorite() async {
        guard let info, let server else { return }
        favorite.toggle()
        do {
            try await server.setFavorite(itemId: info.itemId, favorite: favorite)
        } catch {
            favorite.toggle()
        }
    }

    /// The clock time the item ends at, at this speed.
    var endsAt: Date? {
        guard state.duration > 0 else { return nil }
        return Date.now.addingTimeInterval(max(0, state.duration - state.position) / max(speed, 0.1))
    }

    /// The lines of the info card: how the server sends the item, and what
    /// mpv plays, once a second while it shows. One loop at a time: the
    /// card shown again starts it anew.
    private func refreshInfo() {
        infoTask?.cancel()
        infoTask = Task { [weak self] in
            while !Task.isCancelled, let self, self.showInfo, let mpv = self.mpv {
                let method = info.map { $0.transcoding ? "Transcode" : $0.playMethod.name() } ?? "—"
                self.infoLines = [("Play method", method)] + mpv.infoLines()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    /// The next episode, from the Up next card or at the end of this one;
    /// `paused` when "Are you still watching?" asks first.
    func playNext(paused: Bool = false) async {
        if let group = SyncBridge.shared.group {
            group.user(intent: .next)
            return
        }
        if !queue.isEmpty {
            if let current = info?.item { history.append(current) }
            guard let next = await nextOfQueue() else {
                finished = true
                return
            }
            await play(next, fromStart: true, paused: paused)
            return
        }
        guard let next = info?.next else { return }
        await play(next, paused: paused)
    }

    /// The first item of the queue to play. A show (Play All on a library
    /// of shows) stands for its episodes from the next one not watched
    /// (core's `series_queue`, the Mac's `expand_series`); a show with none
    /// is passed over.
    private func nextOfQueue() async -> MediaItem? {
        while !queue.isEmpty {
            let next = queue.removeFirst()
            guard next.isSeries else { return next }
            guard let server, let episodes = try? await server.seriesQueue(seriesId: next.id), let first = episodes.first else {
                NSLog("PLAYER: no episodes for the show %@ of the queue", next.id)
                continue
            }
            queue.insert(contentsOf: episodes.dropFirst(), at: 0)
            return first
        }
        return nil
    }

    /// Another item in this player (the episode picker, Up next); an item
    /// of a SyncPlay group comes at the group's place, paused.
    func play(_ item: MediaItem, fromStart: Bool = false, at secs: Double? = nil, paused: Bool = false) async {
        showEpisodes = false
        stillWatching.note(itemId: item.id, episode: item.kind == .episode)
        await endSession()
        await load(itemId: item.id, fromStart: fromStart, at: secs, paused: paused)
    }

    /// A change of quality: the item again at its place, under the new cap.
    func setQuality(_ cap: UInt64?) async {
        guard info != nil, cap != quality else { return }
        quality = cap
        // A manual choice wins; its stalls must not step Auto down later.
        Self.stalls.clear()
        await reloadHere()
    }

    /// The timeline: chapters, preview frames, and on a direct play the
    /// tracks the user's settings pick (the server counts them as mpv does).
    private func loadTimeline(_ info: PlayInfo) async {
        timeline = nil
        sheets = [:]
        sheetsLoading = []
        guard let server, let timeline = try? await server.timeline(itemId: info.itemId) else { return }
        guard isCurrent(info) else { return }
        self.timeline = timeline
        guard !info.transcoding, !explicitTracks else { return }
        if let audio = timeline.audioTrack { mpv?.set(property: "aid", "\(audio)") }
        switch timeline.subtitle {
        case .unknown: break
        case .off: mpv?.set(property: "sid", "no")
        case .track(let number): mpv?.set(property: "sid", "\(number)")
        }
    }

    /// The preview frame at a position, if its sheet is here; asks for the
    /// sheet otherwise (once), with the session's header.
    func previewFrame(at secs: Double) -> CGImage? {
        guard let trick = timeline?.trickplay, trick.intervalMs > 0, trick.columns > 0, trick.rows > 0 else { return nil }
        let index = UInt32(max(0, secs) * 1000 / Double(trick.intervalMs))
        let perSheet = trick.columns * trick.rows
        let sheet = index / perSheet
        guard let image = sheets[sheet] else {
            loadSheet(sheet, trick)
            return nil
        }
        let place = index % perSheet
        let rect = CGRect(x: Int((place % trick.columns) * trick.width), y: Int((place / trick.columns) * trick.height),
                          width: Int(trick.width), height: Int(trick.height))
        return image.cropping(to: rect)
    }

    /// The chapter a position is in.
    func chapter(at secs: Double) -> String? {
        timeline?.chapters.last { $0.startSecs <= secs }?.name.nilIfEmpty
    }

    private func loadSheet(_ sheet: UInt32, _ trick: TrickplaySheets) {
        guard !sheetsLoading.contains(sheet), let info,
              let url = URL(string: trick.sheetUrl.replacingOccurrences(of: "{sheet}", with: "\(sheet)")) else { return }
        sheetsLoading.insert(sheet)
        let request = Self.request(url, header: info.httpHeader)
        Task { [weak self] in
            let image = try? await URLSession.shared.data(for: request).0
            // A sheet of the item before must not land in this one's set
            // (`loadTimeline` cleared it, the loading mark too).
            guard let self, self.isCurrent(info) else { return }
            self.sheetsLoading.remove(sheet)
            if let cgImage = image.flatMap({ UIImage(data: $0)?.cgImage }) { self.sheets[sheet] = cgImage }
        }
    }

    private func loadSeason(_ info: PlayInfo) async {
        let item = info.item
        guard let server, let series = item.seriesId, let season = item.seasonId else {
            seasonEpisodes = []
            return
        }
        let episodes = (try? await server.episodes(seriesId: series, seasonId: season)) ?? []
        guard isCurrent(info) else { return }
        seasonEpisodes = episodes
    }

    /// The item's bookmarks (Jellyfin Enhanced), also after the sheet edits
    /// them.
    func loadBookmarks() async {
        guard AppModel.current?.enhanced?.bookmarks == true, let info, let server else {
            bookmarks = []
            return
        }
        let marks = (try? await server.bookmarks(itemId: info.itemId)) ?? []
        guard isCurrent(info) else { return }
        bookmarks = marks
    }

    /// A request with the session's header ("Name: value").
    private static func request(_ url: URL, header: String) -> URLRequest {
        var request = URLRequest(url: url)
        let parts = header.split(separator: ":", maxSplits: 1).map { $0.trimmingCharacters(in: .whitespaces) }
        if parts.count == 2 { request.setValue(parts[1], forHTTPHeaderField: parts[0]) }
        return request
    }

    func close() {
        NSLog("PLAYER: close; position %.1f, finished %d", state.position, finished ? 1 : 0)
        SyncBridge.shared.attach(nil)
        loadToken += 1
        reporter?.cancel()
        hideTask?.cancel()
        infoTask?.cancel()
        Task { await endSession() }
        mpv?.stop()
        mpv = nil
        systemTasks.forEach { $0.cancel() }
        systemTasks = []
        MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
        let commands = MPRemoteCommandCenter.shared()
        [commands.playCommand, commands.pauseCommand, commands.togglePlayPauseCommand,
         commands.skipForwardCommand, commands.skipBackwardCommand, commands.changePlaybackPositionCommand]
            .forEach { $0.removeTarget(nil) }
        UIApplication.shared.isIdleTimerDisabled = false
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        // A measurement that waited for the link runs now.
        LinkWatch.shared.measureIfWanted()
    }

    /// The Stopped report, and the end of the server's transcode (spec
    /// §8.10: the server must not keep a job for a stream nobody reads).
    private func endSession() async {
        guard let info, let server else { return }
        await sendReport(.stopped, info: info, server: server)
        if info.transcoding {
            try? await server.endTranscode(playSessionId: info.playSessionId)
        }
    }

    // MARK: Events of mpv

    /// Something plays (not paused): the link is busy (`LinkWatch`).
    var isPlaying: Bool { mpv != nil && !state.paused }

    /// The bitrate that plays: the cap of a transcode, or the file's.
    var playingBps: UInt64? {
        guard let info else { return nil }
        return info.transcoding ? (sentCap ?? info.sourceBitrate) : info.sourceBitrate
    }

    /// A pause for data began or ended. Under Auto, repeated stalls step
    /// the quality down a rung and the item loads again at its place (the
    /// Mac's `adaptive_tick`): in a group it takes more stalls, as the
    /// group waits for the reload.
    private func stallChanged(_ on: Bool) {
        NSLog("PLAYER: %@ for data at %.1f", on ? "waiting" : "done waiting", state.position)
        guard let info, quality == nil, Preferences.shared.lowerOnSlowLink,
              Downloads.shared.localFile(info.itemId) == nil else {
            Self.stalls.clear()
            return
        }
        Self.stalls.buffering(on: on)
        guard !on, scrubbing == nil,
              let cap = Self.stalls.step(playingBps: playingBps, inGroup: SyncBridge.shared.group != nil) else { return }
        let label = bitrateLabel(bitrate: cap)
        NSLog("PLAYER: stalls; Auto steps down to %@", label)
        AppModel.current?.say("Slow connection: switched to \(label)")
        // The Mac's rule: a measurement is due as soon as the link is free
        // (a pause, the end of the item); it is what lets Auto step up.
        AppModel.current?.measureLink()
        Task { await reloadHere() }
    }

    /// Auto steps back up while the item plays (the Mac's recovery in
    /// `adaptive_tick`, core-side `StallWatch.stepUp`): ten minutes without
    /// a stall after the step down, and a measurement newer than it that
    /// carries more. Asked every 10 s with the progress report; never on a
    /// downloaded file, under a manual quality, or in a SyncPlay group.
    private func autoStepUp() {
        guard let info, let app = AppModel.current, quality == nil, Preferences.shared.lowerOnSlowLink,
              mpv != nil, !state.paused, !state.buffering, scrubbing == nil,
              Downloads.shared.localFile(info.itemId) == nil,
              let up = Self.stalls.stepUp(measuredBps: app.linkBps, fileBps: info.sourceBitrate, codec: nil,
                                          previous: app.autoCap, inGroup: SyncBridge.shared.group != nil) else { return }
        // The cellular limit still holds (no test runs on cellular, so a
        // step up there is not expected).
        if let cellular = Preferences.shared.cellularCap, LinkWatch.shared.onCellular, up.cap.map({ $0 > cellular }) ?? true {
            return
        }
        NSLog("PLAYER: no stall for 10 minutes; Auto steps up to %@", up.label)
        app.say("Better connection: switched to \(up.label)")
        Task { await reloadHere() }
    }

    /// A new network path while a stream plays: on cellular, a stream over
    /// the cellular limit loads again at its place under it. Back on Wi-Fi
    /// the limit lifts at the next load (a lower stream costs nothing).
    func pathChanged() {
        guard let info, mpv != nil, LinkWatch.shared.onCellular, Downloads.shared.localFile(info.itemId) == nil,
              let cap = cellularStep(playingBps: playingBps, cellular: Preferences.shared.cellularCap) else { return }
        let label = bitrateLabel(bitrate: cap)
        NSLog("PLAYER: on cellular; the stream loads again under %@", label)
        AppModel.current?.say("Cellular: switched to \(label)")
        Task { await reloadHere() }
    }

    private func changed(_ new: Mpv.State) {
        let pausedChanged = new.paused != state.paused
        state = new
        if new.tracks != tracks { tracks = new.tracks }
        if let failure = new.failure, error == nil {
            NSLog("PLAYER: mpv stopped: %@", failure)
            error = "The video could not play."
        }
        // The restarted file is ready: its subtitle files go back.
        if !subtitlesToRestore.isEmpty, new.duration > 0, !new.buffering {
            let files = subtitlesToRestore
            subtitlesToRestore = []
            for file in files {
                NSLog("PLAYER: subtitle file added after the load (position %.1f of %.1f)", new.position, new.duration)
                mpv?.addSubtitle(path: file.path, title: file.title)
                addedSubtitles.append(file)
            }
        }
        if pausedChanged {
            if new.paused { LinkWatch.shared.measureIfWanted() }
            report(.progress)
            updateNowPlaying()
            pauseWait?.cancel()
            pauseScreen = false
            if new.paused {
                pauseWait = Task { [weak self] in
                    try? await Task.sleep(for: .seconds(5))
                    guard !Task.isCancelled, let self, self.state.paused, self.scrubbing == nil, !self.showEpisodes else { return }
                    withAnimation(.easeOut(duration: 0.2)) { self.pauseScreen = true }
                }
            }
        }
        // The Lock Screen's elapsed time follows the position about once a
        // second, here and not in a view: the small player and Picture in
        // Picture show no PlayerView.
        if Date.now.timeIntervalSince(nowPlayingAt) >= 1 { updateNowPlaying() }
        autoSkip()
    }

    /// Skips an intro or the credits without a tap when the setting says
    /// so, once per segment (inventory §3.4).
    private func autoSkip() {
        // Never in a SyncPlay group: a skip would move the whole group.
        guard SyncBridge.shared.group == nil, let segment = skippable, !autoSkipped.contains(segment.startSecs) else { return }
        let local = Preferences.shared
        // This device's switch, or the plugin's (Jellyfin Enhanced).
        let plugin = AppModel.current?.enhanced
        let wanted = switch segment.kind {
        case .intro, .recap: local.autoSkipIntro || plugin?.autoSkipIntro == true
        case .outro: local.autoSkipCredits || plugin?.autoSkipOutro == true
        case .preview, .commercial, .unknown: false
        }
        guard wanted else { return }
        autoSkipped.insert(segment.startSecs)
        seek(to: segment.endSecs)
    }

    /// The look of subtitles for mpv, each value from the first source
    /// that sets it: Settings › Subtitles, else the system's caption style,
    /// else the Jellyfin Enhanced style (spec §7.3; core's
    /// `subtitle_properties`). With the plugin's style off on this device
    /// (Settings › Jellyfin Enhanced) `applyEnhanced` leaves it out.
    private func applySubtitleLook() {
        let properties = subtitleProperties(own: Self.ownSubtitleLook(), system: Self.systemSubtitleLook(),
                                            plugin: AppModel.current?.enhanced?.subtitle)
        for property in properties { mpv?.set(property: property.name, property.value) }
        NSLog("PLAYER: subtitle look %@", properties.map { "\($0.name)=\($0.value)" }.joined(separator: " "))
    }

    /// Settings › Subtitles; "Default" leaves a value to the next source.
    static func ownSubtitleLook() -> SubtitleLook {
        let look = Preferences.shared
        return SubtitleLook(color: look.subtitleColor.nilIfEmpty,
                            boxed: look.subtitleBox == 0 ? nil : look.subtitleBox == 1,
                            back: look.subtitleBox == 1 ? "#99000000" : nil,
                            scale: look.subtitleScale > 0 ? look.subtitleScale : nil,
                            font: nil,
                            position: look.subtitlePosition > 0 ? Int64(look.subtitlePosition) : nil)
    }

    /// The system's caption style (Settings › Accessibility › Subtitles &
    /// Captioning): a value counts when the style says it goes over the
    /// video's own (`useValue`), the system's "Video Override" off.
    static func systemSubtitleLook() -> SubtitleLook {
        var look = SubtitleLook(color: nil, boxed: nil, back: nil, scale: nil, font: nil, position: nil)
        var behavior = MACaptionAppearanceBehavior.useContentIfAvailable
        let text = MACaptionAppearanceCopyForegroundColor(.user, &behavior).takeRetainedValue()
        if behavior == .useValue {
            var ignored = MACaptionAppearanceBehavior.useContentIfAvailable
            look.color = mpvColor(text, opacity: MACaptionAppearanceGetForegroundOpacity(.user, &ignored))
        }
        let back = MACaptionAppearanceCopyBackgroundColor(.user, &behavior).takeRetainedValue()
        if behavior == .useValue {
            var ignored = MACaptionAppearanceBehavior.useContentIfAvailable
            let opacity = MACaptionAppearanceGetBackgroundOpacity(.user, &ignored)
            look.boxed = opacity > 0.01
            look.back = opacity > 0.01 ? mpvColor(back, opacity: opacity) : nil
        }
        let size = MACaptionAppearanceGetRelativeCharacterSize(.user, &behavior)
        if behavior == .useValue, size > 0 { look.scale = Double(size) }
        let font = MACaptionAppearanceCopyFontDescriptorForStyle(.user, &behavior, .default).takeRetainedValue()
        if behavior == .useValue, let family = CTFontDescriptorCopyAttribute(font, kCTFontFamilyNameAttribute) as? String {
            look.font = family
        }
        return look
    }

    /// A colour as mpv reads it, "#AARRGGBB", with the style's opacity.
    private static func mpvColor(_ color: CGColor, opacity: CGFloat) -> String? {
        guard let srgb = CGColorSpace(name: CGColorSpace.sRGB),
              let parts = color.converted(to: srgb, intent: .defaultIntent, options: nil)?.components, parts.count >= 3 else { return nil }
        let byte = { (value: CGFloat) in Int((min(max(value, 0), 1) * 255).rounded()) }
        return String(format: "#%02X%02X%02X%02X", byte(opacity), byte(parts[0]), byte(parts[1]), byte(parts[2]))
    }

    /// Subtitles sit above the control bar while it shows. mpv's margin is
    /// in lines of a 720-line picture; the bar and its padding are about
    /// 145 of the 402 points of a phone in landscape, so 260 lines, and
    /// 300 leaves a gap.
    private func liftSubtitles() {
        mpv?.set(property: "sub-margin-y", controls ? "300" : "34")
    }

    /// The end of the file.
    private func ended() {
        // In a group the group names what plays next.
        if let group = SyncBridge.shared.group {
            group.ended()
            return
        }
        NSLog("PLAYER: end of file; start %.1f, position %.1f, duration %.1f, restarted %d",
              info?.startSecs ?? -1, state.position, state.duration, restarted ? 1 : 0)
        // The resume point lies at or past the end of the file (a file the
        // server replaced with a shorter one, or a stale position): play it
        // from the start instead of closing at once. By the file's length,
        // not by a clock: opening a file can take seconds (2026-11-14).
        if let info, !restarted, info.startSecs > 0, state.duration > 0, info.startSecs >= state.duration - 1 {
            restarted = true
            subtitlesToRestore += addedSubtitles
            addedSubtitles = []
            state.duration = 0
            state.buffering = true
            // A pause made while the file opened stays (2026-11-14: the
            // restart played on after the user's pause).
            mpv?.load(url: info.url, header: info.httpHeader, start: 0, paused: state.paused)
            return
        }
        // A queue the user made plays on; the next episode follows the setting.
        if !queue.isEmpty || (info?.next != nil && prefs?.bool("EnableNextEpisodeAutoPlay", default: true) ?? true) {
            // "Are you still watching?" (core's rule): the next episode
            // loads paused, and the question shows over it.
            if let next = nextItem,
               stillWatching.ask(itemId: next.id, episode: next.kind == .episode, choice: prefs?.stillWatching ?? .off) {
                NSLog("PLAYER: still watching? asking before %@", next.id)
                Task {
                    await playNext(paused: true)
                    if !finished, mpv != nil { stillWatchingAsk = true }
                }
            } else {
                Task { await playNext() }
            }
        } else {
            finished = true
        }
    }

    // MARK: Controls

    /// Continue: the paused episode plays, and a new session of the
    /// question starts with it.
    func keepWatching() {
        stillWatchingAsk = false
        if let id = info?.itemId { stillWatching.keepWatching(itemId: id) }
        mpv?.setPaused(false)
        hideSoon()
    }

    /// Stop: the player closes.
    func stopWatching() {
        stillWatchingAsk = false
        finished = true
    }

    /// One frame on or back, paused (the Mac's `,` and `.`). A group plays
    /// in step, so a member does not step alone.
    func frameStep(forward: Bool) {
        guard SyncBridge.shared.group == nil else { return }
        stillWatching.input()
        frameStepping = true
        frameStepToken += 1
        let token = frameStepToken
        mpv?.frameStep(forward: forward)
        hideSoon()
        Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(600))
            guard let self, self.frameStepToken == token else { return }
            self.frameStepping = false
        }
    }

    func togglePause() {
        stillWatching.input()
        if let group = SyncBridge.shared.group {
            group.user(intent: .togglePause)
            hideSoon()
            return
        }
        mpv?.setPaused(!state.paused)
        hideSoon()
    }

    func setMuted(_ on: Bool) {
        muted = on
        mpv?.set(property: "mute", on ? "yes" : "no")
    }

    func skip(_ secs: Double) {
        stillWatching.input()
        if let group = SyncBridge.shared.group {
            group.user(intent: .seek(positionSecs: max(0, state.position + secs)))
            hideSoon()
            return
        }
        mpv?.skip(secs)
        hideSoon()
    }

    func seek(to secs: Double) {
        if let group = SyncBridge.shared.group {
            group.user(intent: .seek(positionSecs: secs))
            return
        }
        mpv?.seek(to: secs)
        report(.progress)
    }

    func scrub(to secs: Double?) {
        scrubSettled?.cancel()
        if let secs {
            scrubbing = secs
            hideTask?.cancel()
            // The Slider's end of editing can be lost: after testSettings,
            // a drag of testUpNext ended in UIKit (the slider went inactive)
            // and SwiftUI never called `editing == false`, so nothing seeked
            // (2026-11-14). A value that rests 3 s seeks on its own: long
            // enough that a finger held still over the preview frames is
            // not cut short.
            scrubSettled = Task { [weak self] in
                try? await Task.sleep(for: .seconds(3))
                guard !Task.isCancelled, let self, self.scrubbing == secs else { return }
                self.scrub(to: nil)
            }
        } else if let target = scrubbing {
            seek(to: target)
            scrubbing = nil
            hideSoon()
        }
    }

    func tapVideo() {
        stillWatching.input()
        holding = false
        controls.toggle()
        if controls { hideSoon() }
    }

    func holdControls() {
        stillWatching.input()
        holding = true
        hideTask?.cancel()
        controls = true
    }

    /// A choice was made in a menu: the timer runs again.
    func releaseControls() {
        holding = false
        hideSoon()
    }

    /// The controls hide 2.5 s after the last touch, unless paused or
    /// scrubbing, and never under VoiceOver or Switch Control (spec §7.3).
    func hideSoon() {
        hideTask?.cancel()
        controls = true
        guard !holding, !UIAccessibility.isVoiceOverRunning, !UIAccessibility.isSwitchControlRunning else { return }
        // UI tests keep the controls up (they cannot race a 2.5 s timer).
        guard ProcessInfo.processInfo.environment["BLOOM_CONTROLS_STAY"] != "1" else { return }
        hideTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(2.5))
            // A test may let them hide while paused alone (BLOOM_HIDE_WHEN_PAUSED):
            // a playing video keeps XCUITest from going idle, so its first
            // tap (the pause) cannot race the timer while the video plays.
            let pausedHides = ProcessInfo.processInfo.environment["BLOOM_HIDE_WHEN_PAUSED"] == "1"
            guard !Task.isCancelled, let self, !self.holding, pausedHides == self.state.paused, self.scrubbing == nil else { return }
            withAnimation(.easeOut(duration: 0.15)) { self.controls = false }
        }
    }

    // MARK: Tracks

    var audioTracks: [Mpv.Track] { tracks.filter { $0.kind == .audio } }
    var subtitleTracks: [Mpv.Track] { tracks.filter { $0.kind == .sub } }

    /// A track of mpv in friendly words. A file the server sent is named
    /// by the server's track (its flags: forced, SDH); a track of the file
    /// by what mpv reads in it.
    func name(of track: Mpv.Track) -> TrackName {
        if track.external, let subtitle = info?.externalSubtitles.first(where: { $0.title == track.name }) {
            return name(of: subtitle)
        }
        let name = TrackName(kind: track.kind == .audio ? .audio : .subtitle, language: track.language, codec: track.codec,
                             title: track.name, forced: track.forced, isDefault: track.isDefault)
        return name.name == "Unknown language" && track.language == nil && track.name.isEmpty
            ? TrackName(name: "Track \(track.id)", detail: name.detail) : name
    }

    func name(of subtitle: ExternalSubtitle) -> TrackName {
        if let track = fileTracks.first(where: { $0.index == subtitle.index && $0.kind == .subtitle }) {
            return TrackName(track)
        }
        return TrackName(kind: .subtitle, language: subtitle.language, codec: subtitle.codec, title: nil)
    }

    /// The tracks of the item's file with their flags, for the names, and
    /// the item's versions, for the settings menu.
    private func loadFile(_ info: PlayInfo) async {
        guard let server else { return }
        let list = (try? await server.mediaVersions(itemId: info.itemId)) ?? []
        guard isCurrent(info) else { return }
        versions = list
        let file = list.first { $0.id == info.mediaSourceId } ?? list.first(where: \.isDefault) ?? list.first
        fileTracks = file?.file.tracks ?? []
    }

    /// Plays another version of the item at the place it is at. The tracks
    /// of one file name nothing in another: the server and mpv choose
    /// them again.
    func switchVersion(_ sourceId: String) async {
        guard let info, sourceId != info.mediaSourceId else { return }
        NSLog("PLAYER: version %@ of %@", sourceId, info.itemId)
        version = (itemId: info.itemId, sourceId: sourceId)
        await reloadHere(chosen: true)
    }

    /// The server's subtitle files not yet in mpv.
    var serverSubtitles: [ExternalSubtitle] {
        // By the title `sub-add` gave: the menu's title has the codec too.
        let added = Set(subtitleTracks.filter(\.external).map(\.name))
        return (info?.externalSubtitles ?? []).filter { !added.contains($0.title) }
    }

    func selectAudio(_ track: Mpv.Track) {
        releaseControls()
        mpv?.set(property: "aid", "\(track.id)")
        report(.progress)
    }

    func selectSubtitle(_ track: Mpv.Track?) {
        releaseControls()
        mpv?.set(property: "sid", track.map { "\($0.id)" } ?? "no")
        report(.progress)
    }

    /// A track another device chose, by the server's index: a reload at
    /// the place with that track (the Mac maps the index the same way at
    /// load). Subtitles off (-1) is a switch in mpv.
    func useStream(audio: Int64? = nil, subtitle: Int64? = nil) async {
        guard info != nil else { return }
        if let subtitle, subtitle < 0 {
            selectSubtitle(nil)
            return
        }
        if let audio { chosenAudio = audio }
        if let subtitle { chosenSubtitle = subtitle }
        choicesFor = info?.itemId
        await reloadHere(chosen: true)
    }

    /// Items another device queued: next after this one, or at the end.
    func enqueue(_ items: [MediaItem], next: Bool) {
        if next { queue.insert(contentsOf: items, at: 0) } else { queue.append(contentsOf: items) }
    }

    /// The extension mpv reads a subtitle file by (the Mac's
    /// `subtitle_extension`): an image track of a transcode comes as
    /// `Stream.pgssub` or `Stream.vobsub` and must be saved as `.sup` or `.mks`.
    static func subtitleExtension(_ url: String) -> String {
        let name = url.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? url
        let ext = name.split(separator: ".").last.map { String($0).lowercased() } ?? ""
        switch ext {
        case "pgssub", "pgs", "sup": return "sup"
        case "vobsub", "dvdsub", "mks": return "mks"
        case "srt", "ass", "ssa", "vtt": return ext
        default: return "srt"
        }
    }

    /// A subtitle downloaded into the library: the item again at its place,
    /// with that subtitle chosen (the server lists it only now).
    func useDownloadedSubtitle(_ index: Int64) async {
        guard info != nil else { return }
        chosenSubtitle = index
        choicesFor = info?.itemId
        await reloadHere(chosen: true)
    }

    /// Fetches a server subtitle file on a task of its own, then gives mpv
    /// the local path: a transcode's subtitles can take the server a minute
    /// to make, and mpv must not wait for that (CLAUDE.md).
    func selectServerSubtitle(_ subtitle: ExternalSubtitle) async {
        guard let info, let url = URL(string: subtitle.url) else { return }
        fetchingSubtitle = subtitle.index
        defer { fetchingSubtitle = nil }
        let request = Self.request(url, header: info.httpHeader)
        do {
            // To a file, not into memory: an image track (PGS, VobSub) is
            // 10 to 60 MB, and the move below is a rename, not a write.
            let (download, response) = try await URLSession.shared.download(for: request)
            defer { try? FileManager.default.removeItem(at: download) }
            guard isCurrent(info) else { return }
            guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw URLError(.badServerResponse) }
            let ext = Self.subtitleExtension(subtitle.url)
            let file = FileManager.default.temporaryDirectory.appending(path: "sub-\(info.itemId)-\(subtitle.index).\(ext)")
            try? FileManager.default.removeItem(at: file)
            try FileManager.default.moveItem(at: download, to: file)
            NSLog("PLAYER: subtitle file %@ ready=%d", file.lastPathComponent, (state.duration > 0 && !state.buffering) ? 1 : 0)
            // `sub-add` on a file that is not ready yet is lost: it waits.
            if state.duration > 0, !state.buffering {
                mpv?.addSubtitle(path: file.path, title: subtitle.title)
                addedSubtitles.append((file.path, subtitle.title))
            } else {
                subtitlesToRestore.append((file.path, subtitle.title))
            }
        } catch {
            guard isCurrent(info) else { return }
            // A subtitle that does not come is not fatal: the film plays on.
            // An image subtitle (PGS, VobSub) is drawn into the video by the
            // server instead, once (the Mac's `subtitle_file_failed`).
            if ["sup", "mks"].contains(Self.subtitleExtension(subtitle.url)), !burnedIn.contains(info.itemId) {
                burnedIn.insert(info.itemId)
                NSLog("PLAYER: the image subtitle %lld did not come; the server burns it in", subtitle.index)
                chosenSubtitle = subtitle.index
                burnIn = true
                choicesFor = info.itemId
                await reloadHere(chosen: true)
            }
        }
    }

    // MARK: Segments and Up next

    /// The segment the position is in, when it can be skipped: 3 s or
    /// longer, and not in its last second (inventory §3.4).
    var skippable: Segment? {
        let at = state.position
        return info?.segments.first { segment in
            segment.kind != .commercial && segment.kind != .unknown
                && segment.endSecs - segment.startSecs >= 3
                && at >= segment.startSecs && at < segment.endSecs - 1
        }
    }

    func skipSegment() {
        guard let segment = skippable else { return }
        seek(to: segment.endSecs)
    }

    /// Up next shows in the last 30 s or from the start of the credits.
    var upNext: MediaItem? {
        guard let next = nextItem, !upNextHidden, prefs?.upNextCard ?? true, state.duration > 0 else { return nil }
        let outro = info?.segments.first { $0.kind == .outro }?.startSecs
        let from = min(outro ?? .infinity, state.duration - 30)
        return state.position >= from ? next : nil
    }

    // MARK: Reports

    private func report(_ kind: ReportKind) {
        guard let info, let server else { return }
        let position = state.position, paused = state.paused
        Task {
            try? await server.report(kind: kind, infoItemId: info.itemId, mediaSourceId: info.mediaSourceId,
                                     playSessionId: info.playSessionId, playMethod: info.playMethod,
                                     positionSecs: position, paused: paused)
        }
    }

    private func sendReport(_ kind: ReportKind, info: PlayInfo, server: Server) async {
        do {
            try await server.report(kind: kind, infoItemId: info.itemId, mediaSourceId: info.mediaSourceId,
                                    playSessionId: info.playSessionId, playMethod: info.playMethod,
                                    positionSecs: state.position, paused: state.paused)
        } catch {
            // A download that played offline: its place goes to the server
            // at the next connection, so other devices resume there.
            if kind == .stopped, Downloads.shared.localFile(info.itemId) != nil {
                OfflinePositions.record(itemId: info.itemId, mediaSourceId: info.mediaSourceId, secs: state.position)
            }
        }
    }

    // MARK: The system: Lock Screen, interruptions, background

    /// The picture goes where the sound goes: AirPlay starts MPVUI's
    /// external playback, the phone's own output stops it. A film that
    /// played goes on playing (it does not wait for Play on the TV).
    func followAudioRoute() {
        guard let engine else { return }
        let airplay = AVAudioSession.sharedInstance().currentRoute.outputs.contains { $0.portType == .airPlay }
        let external = engine.externalPlayback
        if airplay, !external.isActive, !external.isPreparing {
            let wasPlaying = !state.paused
            NSLog("PLAYER: the sound went to AirPlay; the picture follows (playing %d)", wasPlaying ? 1 : 0)
            Task {
                try? await external.start()
                if wasPlaying, SyncBridge.shared.group == nil { engine.play() }
            }
        } else if !airplay, external.isActive || external.isPrepared {
            NSLog("PLAYER: the sound came back; the picture too")
            let wasPlaying = !state.paused
            external.stop()
            if wasPlaying, SyncBridge.shared.group == nil { engine.play() }
        }
    }

    /// MPVUI's external playback changed: AirPlay prepares, or it shows.
    private func externalPlaybackChanged(preparing: Bool, active: Bool) {
        if preparing {
            playingBeforeRoute = !state.paused
        } else if active, playingBeforeRoute {
            playingBeforeRoute = false
            // In a group the group says when it plays.
            if SyncBridge.shared.group == nil { engine?.play() }
        }
    }

    /// The external playback of MPVUI's player, followed here and not in a
    /// view: the small player and Picture in Picture show no PlayerView.
    private func watchExternalPlayback(_ engine: MPVPlayer) {
        systemTasks.append(Task { [weak self] in
            let changes = Observations { (engine.externalPlayback.isPreparing, engine.externalPlayback.isActive) }
            var last = (preparing: false, active: false)
            for await (preparing, active) in changes {
                guard let self else { return }
                if preparing != last.preparing { self.externalPlaybackChanged(preparing: preparing, active: active) }
                if active != last.active { self.externalPlaybackChanged(preparing: false, active: active) }
                last = (preparing, active)
            }
        })
    }

    /// The system's notifications, each a task over its stream, on the main
    /// actor; the close cancels them.
    private func observeSystem() {
        let center = NotificationCenter.default
        // A call, Siri, an alarm: pause; resume only when the system says so.
        // In a group the pause is a request to the group, as every pause of
        // the user is (the Mac's `sync_intent`): the others wait for this
        // player. The end of the call does not start the group again; the
        // group plays when a member presses Play. iOS 27 splits the old
        // interruption notification in two: the session went inactive (an
        // interruption, or the app's own deactivation, which is no pause),
        // and the recommendation to resume.
        systemTasks.append(Task { [weak self] in
            for await note in center.notifications(named: AVAudioSession.didBecomeInactiveNotification) {
                guard case .systemInterruption = AVAudioSession.DidBecomeInactiveMessage.makeMessage(note)?.deactivationResult else { continue }
                if let group = SyncBridge.shared.following {
                    group.user(intent: .pause)
                } else {
                    self?.mpv?.setPaused(true)
                }
            }
        })
        systemTasks.append(Task { [weak self] in
            for await note in center.notifications(named: AVAudioSession.resumptionRecommendationNotification) {
                guard AVAudioSession.ResumptionRecommendationMessage.makeMessage(note)?.recommendation == .shouldResume,
                      SyncBridge.shared.following == nil else { continue }
                self?.mpv?.setPaused(false)
            }
        })
        // Headphones out: pause, as every media app does. Not when AirPlay
        // ends: the film goes back to this phone and plays on.
        // The route of the sound leads the picture: AirPlay chosen anywhere
        // (Control Center too) sends the picture with it, and the phone's
        // speaker brings it back (Daniel, 2026-11-14: the sound went to the
        // Apple TV with the picture left on the phone).
        systemTasks.append(Task { [weak self] in
            for await note in center.notifications(named: AVAudioSession.routeChangeNotification) {
                let raw = note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt
                let previous = note.userInfo?[AVAudioSessionRouteChangePreviousRouteKey] as? AVAudioSessionRouteDescription
                let wasAirPlay = previous?.outputs.contains { $0.portType == .airPlay } ?? false
                if raw.flatMap(AVAudioSession.RouteChangeReason.init) == .oldDeviceUnavailable, !wasAirPlay {
                    // In a SyncPlay group the pause is the group's (spec §8.10).
                    if let group = SyncBridge.shared.group {
                        group.user(intent: .pause)
                    } else {
                        self?.mpv?.setPaused(true)
                    }
                }
                self?.followAudioRoute()
            }
        })
        // In the background mpv decodes no video (no GPU work there; the
        // sound plays on), and takes the picture back on return. Not while
        // Picture in Picture shows it: the system starts it as the app
        // leaves, so the check waits a moment (2026-11-14 on the phone: the
        // PiP window went black with the sound playing on).
        systemTasks.append(Task { [weak self] in
            for await _ in center.notifications(named: UIApplication.didEnterBackgroundNotification) {
                self?.report(.progress)
                Task { [weak self] in
                    try? await Task.sleep(for: .seconds(2))
                    guard let self, UIApplication.shared.applicationState == .background,
                          self.engine?.pictureInPicture.isActive != true else { return }
                    self.mpv?.set(property: "vid", "no")
                }
            }
        })
        systemTasks.append(Task { [weak self] in
            for await _ in center.notifications(named: UIApplication.willEnterForegroundNotification) {
                self?.mpv?.set(property: "vid", "auto")
            }
        })
    }

    private func remoteCommands() {
        let commands = MPRemoteCommandCenter.shared()
        commands.playCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { if let self, self.state.paused { self.togglePause() } }
            return .success
        }
        commands.pauseCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { if let self, !self.state.paused { self.togglePause() } }
            return .success
        }
        commands.togglePlayPauseCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.togglePause() }
            return .success
        }
        commands.skipBackwardCommand.preferredIntervals = [NSNumber(value: skipBack)]
        commands.skipBackwardCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { if let self { self.skip(-self.skipBack) } }
            return .success
        }
        commands.skipForwardCommand.preferredIntervals = [NSNumber(value: skipForward)]
        commands.skipForwardCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { if let self { self.skip(self.skipForward) } }
            return .success
        }
        commands.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else { return .commandFailed }
            MainActor.assumeIsolated { self?.seek(to: event.positionTime) }
            return .success
        }
    }

    /// Now Playing through MediaPlayer for this build; the iOS 27 Now
    /// Playing framework replaces it in phase 4 (spec §8.3).
    private func updateNowPlaying() {
        guard let info else { return }
        var now: [String: Any] = [
            MPMediaItemPropertyTitle: info.item.name,
            MPMediaItemPropertyPlaybackDuration: state.duration,
            MPNowPlayingInfoPropertyElapsedPlaybackTime: state.position,
            MPNowPlayingInfoPropertyPlaybackRate: state.paused ? 0.0 : speed,
            MPNowPlayingInfoPropertyMediaType: MPNowPlayingInfoMediaType.video.rawValue,
        ]
        if let series = info.item.seriesName {
            now[MPMediaItemPropertyArtist] = [series, info.item.episodeCode].compactMap { $0 }.joined(separator: " · ")
        }
        if let artwork { now[MPMediaItemPropertyArtwork] = artwork }
        MPNowPlayingInfoCenter.default().nowPlayingInfo = now
        nowPlayingAt = .now
    }

    private func loadArtwork(_ info: PlayInfo) {
        artwork = nil
        let item = info.item
        guard let address = item.still ?? item.wide ?? item.poster ?? item.backdrop, let url = URL(string: address) else { return }
        Task { [weak self] in
            guard let (data, _) = try? await URLSession.shared.data(from: url), let image = UIImage(data: data),
                  let self, self.isCurrent(info) else { return }
            self.artwork = Self.artwork(image)
            self.updateNowPlaying()
        }
    }

    /// The artwork's image callback runs on MediaPlayer's own queue, not
    /// the main actor. Made here, outside the main actor, so the closure
    /// carries no isolation check (2026-11-14: a closure made in the
    /// model trapped in `swift_task_isCurrentExecutor` on `accessQueue`).
    nonisolated private static func artwork(_ image: UIImage) -> MPMediaItemArtwork {
        MPMediaItemArtwork(boundsSize: image.size) { _ in image }
    }
}

extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
