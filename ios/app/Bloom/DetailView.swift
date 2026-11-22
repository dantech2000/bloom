import BloomCore
import SwiftUI

struct DetailView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horizontalSizeClass) private var width
    @Environment(\.openURL) private var openURL
    let item: MediaItem
    @State private var detail: Detail?
    @State private var season: MediaItem?
    @State private var episodes: [MediaItem] = []
    /// The season `episodes` belong to: right after a tap the old season's
    /// episodes still show while the new ones load.
    @State private var episodesOf: String?
    @State private var error: String?
    @State private var extras: SeerrExtras?
    /// "Request more seasons" waits for the user's yes (it starts downloads).
    @State private var askMore: Int64?
    /// The item's bookmarks (Jellyfin Enhanced).
    @State private var bookmarks: [Bookmark] = []
    /// Its audio and subtitle tracks, and the ones chosen for the next play.
    @State private var streams: [StreamOption] = []
    /// The file: its video, sound, tracks with their flags, and size. Of
    /// the version chosen, when the item has more than one.
    @State private var file: MediaFile?
    /// The files of a film or an episode (Jellyfin 12), its own first, and
    /// the one chosen for the next play (nil: the item's own).
    @State private var versions: [MediaVersion] = []
    @State private var version: String?
    @State private var audio: Int64?
    @State private var subtitle: Int64?
    /// The show and the season of an episode or a season page, for the
    /// links in its title (the Mac's series and season links).
    @State private var series: MediaItem?
    @State private var seasonItem: MediaItem?

    private var shown: MediaItem { detail?.item ?? item }
    /// The page is scrolled past its backdrop: the bar takes the page's black.
    @State private var scrolledPast = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Metrics.rowSpacing) {
                VStack(alignment: .leading, spacing: 14) {
                    // The backdrop ends in the page under the shade, with
                    // the title over its lower part on a phone.
                    ZStack(alignment: .bottomLeading) {
                        Picture(url: shown.backdrop ?? shown.wide, corner: 0, symbol: nil)
                            .frame(height: width == .regular ? 300 : 360)
                            .backgroundExtensionEffect()
                            // The extended copy takes taps meant for the
                            // links and buttons over it (the Home hero's
                            // "i" button, 2026-11-18); the picture has none.
                            .allowsHitTesting(false)
                        HeroShade()
                        if width != .regular {
                            titles.padding(.horizontal, Metrics.gutter)
                        }
                    }
                    HStack(alignment: .top, spacing: 28) {
                        // A wide screen (an iPad) has the Mac's floating poster
                        // beside the text; a phone stays in one column.
                        if width == .regular, let poster = shown.showPoster ?? shown.poster {
                            Picture(url: poster, corner: 18)
                                .frame(width: 220, height: 330)
                                .offset(y: -120)
                                .padding(.bottom, -120)
                        }
                        VStack(alignment: .leading, spacing: 14) {
                            if width == .regular { titles }
                            MetaChips(item: shown, tags: true)
                            if !shown.genres.isEmpty {
                                Text(shown.genres.joined(separator: " · "))
                                    .font(.footnote).foregroundStyle(Color.ink.opacity(0.6))
                            }
                            // A show's creators (Jellyfin Web 12; the Mac's
                            // "Creator" row), each a link to their page.
                            if let creators = detail?.creators, !creators.isEmpty {
                                CreatedBy(creators: creators)
                            }
                            actions
                                .padding(.top, 4)
                            TrackPickers(choices: trackChoices, audio: $audio, subtitle: $subtitle)
                            if let overview = shown.overview {
                                Overview(text: overview)
                            }
                            if versions.count > 1, shown.playable {
                                VersionPicker(versions: versions, chosen: $version)
                            }
                            if let file, shown.playable {
                                FileSection(file: file)
                            }
                            if let error {
                                Text(error).font(.subheadline).foregroundStyle(Color.fail)
                            }
                        }
                    }
                    .padding(.horizontal, width == .regular ? 40 : Metrics.gutter)
                }
                if !bookmarks.isEmpty {
                    BookmarkStrip(bookmarks: bookmarks) { play(shown, at: $0.secs) }
                }
                if let detail, !detail.seasons.isEmpty {
                    seasons(HiddenTitles.shared.visible(detail.seasons, on: .library))
                }
                if let detail, shown.kind == .season, !detail.episodes.isEmpty {
                    episodeList(detail.episodes)
                }
                if let detail, shown.kind == .episode, detail.episodes.count > 1 {
                    EpisodeShelf(title: "More from \(shown.seasonName ?? "this season")", episodes: detail.episodes, current: shown.id)
                }
                if let detail {
                    CastRow(cast: detail.cast)
                    // The collections that hold this title (Jellyfin 12.0);
                    // no row when there are none.
                    PosterRow(title: "Included In", items: detail.collections)
                    PosterRow(title: "More Like This", items: HiddenTitles.shared.visible(detail.similar, on: .library))
                }
                if let extras {
                    SeerrExtrasSection(extras: extras)
                }
            }
            .padding(.bottom, 32)
        }
        // Past the backdrop the page goes under a bar of the page's black
        // with the title in it: nothing of the page sits under the clock
        // and the buttons (the tester saw Play pinned under them).
        .onScrollGeometryChange(for: Bool.self) { geometry in
            // The title reaches the bar: the backdrop is 300 or 360 tall
            // and the bar ends about 110 points down.
            geometry.contentOffset.y + geometry.contentInsets.top > (width == .regular ? 170 : 210)
        } action: { _, past in
            withAnimation(.easeOut(duration: 0.15)) { scrolledPast = past }
        }
        .background(Color.page)
        .background(ZoomDismissGuard().frame(width: 0, height: 0).accessibilityHidden(true))
        .ignoresSafeArea(edges: .top)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(Color.page, for: .navigationBar)
        .toolbarBackgroundVisibility(scrolledPast ? .visible : .hidden, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Text(shown.name).font(.headline).lineLimit(1)
                    .opacity(scrolledPast ? 1 : 0)
                    .accessibilityHidden(!scrolledPast)
            }
            castAndSync
        }
        .task { await load() }
        // Another version is another file: its facts show, and the tracks
        // picked for the file before name nothing in it.
        .onChange(of: version) {
            file = (versions.first { $0.id == version } ?? versions.first(where: \.isDefault) ?? versions.first)?.file
            audio = nil
            subtitle = nil
        }
        .confirmationDialog("Request the missing seasons of \(shown.name)?", isPresented: Binding(get: { askMore != nil }, set: { if !$0 { askMore = nil } }),
                            titleVisibility: .visible) {
            Button("Request") {
                if let tmdb = askMore { Task { await model.seerrRequest(tmdbId: tmdb, mediaType: .tv, title: shown.name) } }
            }
        } message: {
            Text("Seerr asks the server's tools to download them.")
        }
    }

    /// Play on (another Jellyfin app, a Chromecast) and SyncPlay at the
    /// top right, where a cast button sits in most video apps; each opens
    /// the sheet the account menu opens.
    @ToolbarContentBuilder
    private var castAndSync: some ToolbarContent {
        ToolbarItemGroup(placement: .topBarTrailing) {
            if shown.playable || shown.isSeries || shown.kind == .season {
                let elsewhere = model.castDevice != nil || model.remote != nil
                Button("Play on", systemImage: elsewhere ? "tv.fill" : "tv") { model.showPlayOn = true }
                    .accessibilityValue(model.castDevice?.name ?? model.remote?.deviceName ?? "This device")
                    .accessibilityIdentifier("detail.play-on")
                if SyncBridge.shared.group != nil || model.syncAllowed {
                    Button("SyncPlay", systemImage: SyncBridge.shared.group != nil ? "person.2.fill" : "person.2") { model.showSyncPlay = true }
                        .accessibilityIdentifier("detail.syncplay")
                }
            }
        }
    }

    /// The show over an episode's name, and the name as the page's title.
    private var titles: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let name = shown.seriesName {
                HStack(spacing: 6) {
                    // Each link ends in its own chevron, which also parts them.
                    link(name, to: series, id: "detail.series-link")
                    if shown.kind == .episode, let season = shown.seasonName {
                        link(season, to: seasonItem, id: "detail.season-link")
                            .padding(.leading, 8)
                    }
                }
            }
            Text(shown.name).font(.display(32)).lineLimit(3)
                .accessibilityAddTraits(.isHeader)
        }
    }

    /// The show or the season over a title: a link once its page is known,
    /// quiet text until then. The tap target is 44 points tall.
    @ViewBuilder
    private func link(_ name: String, to target: MediaItem?, id: String) -> some View {
        let label = Text(name).font(.subheadline.weight(.semibold)).lineLimit(1)
        if let target {
            NavigationLink(value: Route.item(target)) {
                HStack(spacing: 3) {
                    label
                    Image(systemName: "chevron.right").font(.caption2.weight(.bold)).opacity(0.6)
                }
                .foregroundStyle(Color.ink.opacity(0.85))
                .frame(minHeight: 44)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityHint("Opens \(name)")
            .accessibilityIdentifier(id)
        } else {
            label.foregroundStyle(Color.ink.opacity(0.7)).frame(minHeight: 44)
        }
    }

    /// A season's episodes, a tap plays one, and the season's download
    /// over them (as on the show's page).
    private func episodeList(_ list: [MediaItem]) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            downloadSeason(shown.name, list)
            LazyVStack(alignment: .leading, spacing: 14) {
                ForEach(list, id: \.id) { episode in
                    Button { model.play(episode) } label: { EpisodeRow(episode: episode) }
                        .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, Metrics.gutter)
        }
    }

    /// "Download Season 1": every episode of the list not downloaded yet.
    @ViewBuilder
    private func downloadSeason(_ name: String, _ list: [MediaItem]) -> some View {
        if model.prefs.canDownload {
            Button("Download \(name)", systemImage: "arrow.down.circle") {
                Task {
                    guard let server = model.server else { return }
                    let wanted = list.filter { $0.playable && Downloads.shared.entry($0.id) == nil }
                    // One toast for the lot (the Mac's rule), not one per episode.
                    model.say(wanted.isEmpty ? "Every episode is downloaded already" : "Downloading \(wanted.count) episodes")
                    var failed = 0
                    for (index, episode) in wanted.enumerated() {
                        // A switch of server mid-loop: the rest belong to the old one.
                        guard model.server === server else { failed += wanted.count - index; break }
                        if await Downloads.shared.download(episode, server: server) != nil { failed += 1 }
                    }
                    if failed > 0 { model.say("\(failed) of them did not start") }
                }
            }
            .quietButton()
            .controlSize(.small)
            .padding(.horizontal, Metrics.gutter)
            .accessibilityIdentifier("season.download")
        }
    }

    /// Play or resume the whole width of the column; a series plays its
    /// next episode. Under it the quiet actions, each a glass disc with its
    /// word: one white primary per page (DESIGN.md).
    private var actions: some View {
        GlassEffectContainer {
            VStack(spacing: 16) {
                if let next = playTarget {
                    Button {
                        model.play(next, audio: next.id == shown.id ? audio : nil, subtitle: next.id == shown.id ? subtitle : nil,
                                   version: next.id == shown.id ? version : nil)
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "play.fill")
                            Text(playLabel(next))
                            if let left = timeLeft(next) {
                                Text(left).fontWeight(.regular).opacity(0.6)
                            }
                        }
                    }
                    .buttonStyle(MainButtonStyle(wide: true))
                    .accessibilityIdentifier("detail.play")
                    if let left = remainingSecs(next) {
                        EndsAt(remainingSecs: left).padding(.top, -6)
                    }
                }
                HStack(alignment: .top, spacing: 0) {
                    if let next = playTarget, next.resumeSecs > 0 {
                        action("Restart", label: "From the beginning", symbol: "arrow.counterclockwise") {
                            model.play(next, fromStart: true, audio: next.id == shown.id ? audio : nil, subtitle: next.id == shown.id ? subtitle : nil,
                                       version: next.id == shown.id ? version : nil)
                        }
                    }
                    action("Watched", label: shown.played ? "Mark unplayed" : "Mark played",
                           symbol: shown.played ? "checkmark.circle.fill" : "checkmark.circle") {
                        // A series or a season marks its episodes too: they load again.
                        Task { await model.togglePlayed(shown); await load(); await reloadEpisodes() }
                    }
                    action("Favorite", label: shown.favorite ? "Remove from favorites" : "Add to favorites",
                           symbol: shown.favorite ? "heart.fill" : "heart") {
                        Task { await model.toggleFavorite(shown); await load() }
                    }
                    if shown.playable, model.prefs.canDownload {
                        captioned(downloadWord) {
                            DownloadButton(item: shown, disc: true, version: version).buttonStyle(DiscButtonStyle())
                        }
                    }
                    captioned("More") { more }
                }
            }
        }
        .frame(maxWidth: 480)
    }

    /// A quiet action of the row: its disc and a word under it.
    private func action(_ word: String, label: String, symbol: String, _ run: @escaping () -> Void) -> some View {
        captioned(word) {
            Button(label, systemImage: symbol, action: run).buttonStyle(DiscButtonStyle())
        }
    }

    private func captioned<Control: View>(_ word: String, @ViewBuilder _ control: () -> Control) -> some View {
        VStack(spacing: 6) {
            // One slot for a button and for a menu, which sizes its label
            // by itself.
            control().frame(width: Metrics.disc, height: Metrics.disc)
            Text(word).font(.caption2).foregroundStyle(Color.ink.opacity(0.6)).lineLimit(1).minimumScaleFactor(0.8)
                .accessibilityHidden(true)
        }
        .frame(maxWidth: .infinity)
    }

    /// The word under the download disc, by the state of the download.
    private var downloadWord: String {
        switch Downloads.shared.entry(shown.id)?.state {
        case nil: "Download"
        case .downloading: "Pause"
        case .paused: "Resume"
        case .failed: "Try again"
        case .done: "Downloaded"
        }
    }

    /// The seconds a play of `target` lasts from where it starts.
    private func remainingSecs(_ target: MediaItem) -> Int64? {
        guard let runtime = target.runtimeSecs, runtime > target.resumeSecs else { return nil }
        return runtime - target.resumeSecs
    }

    /// "37m left" beside Resume.
    private func timeLeft(_ target: MediaItem) -> String? {
        guard target.resumeSecs > 0, let runtime = target.runtimeSecs, runtime > target.resumeSecs,
              let left = Words.runtime(runtime - target.resumeSecs) else { return nil }
        return "\(left) left"
    }

    /// The rest, as on the Mac's More menu.
    private var more: some View {
        Menu {
            if let trailer = shown.trailer, let url = URL(string: trailer) {
                Button("Watch the trailer", systemImage: "film") { openURL(url) }
            }
            Button("Refresh", systemImage: "arrow.clockwise") { Task { await load() } }
            if let web = shown.webUrl, let url = URL(string: web) {
                Button("Open in Jellyfin Web", systemImage: "safari") { openURL(url) }
            }
            if ListKind.playlist.takes(shown) || (model.prefs.canCollect && ListKind.collection.takes(shown)) {
                Divider()
                if ListKind.playlist.takes(shown) {
                    Button("Add to playlist…", systemImage: "text.badge.plus") { model.addRequest = AddRequest(item: shown, kind: .playlist) }
                }
                if model.prefs.canCollect, ListKind.collection.takes(shown) {
                    Button("Add to collection…", systemImage: "square.stack.3d.up") { model.addRequest = AddRequest(item: shown, kind: .collection) }
                }
            }
            if model.enhanced?.hiddenContent == true {
                HideMenu(item: shown)
            }
            if shown.isSeries, model.enhanced?.requestMore == true, let tmdb = extras?.tmdbId {
                Button("Request more seasons…", systemImage: "plus.circle") { askMore = tmdb }
            }
        } label: {
            Image(systemName: "ellipsis").glassDisc()
        }
        .buttonStyle(.plain)
        .accessibilityLabel("More")
        .accessibilityIdentifier("detail.more")
    }

    /// The tracks of the pickers in friendly words.
    private var trackChoices: [TrackChoice] {
        if let file {
            return file.tracks.map { TrackChoice(index: $0.index, kind: $0.kind, name: TrackName($0), isDefault: $0.isDefault) }
        }
        return streams.map { TrackChoice(index: $0.index, kind: $0.kind, name: TrackName(name: $0.title, detail: nil), isDefault: $0.isDefault) }
    }

    private var playTarget: MediaItem? {
        if shown.kind == .season, let list = detail?.episodes, !list.isEmpty {
            // The one begun, else the first not watched, else the first.
            return list.first { $0.resumeSecs > 0 } ?? list.first { !$0.played } ?? list.first
        }
        if shown.isSeries {
            let next = detail?.nextUp.flatMap { HiddenTitles.shared.isHidden($0, on: .nextUp) ? nil : $0 }
            return next ?? episodes.first
        }
        return shown.playable ? shown : nil
    }

    private func playLabel(_ target: MediaItem) -> String {
        let verb = target.resumeSecs > 0 ? "Resume" : "Play"
        if shown.isSeries || shown.kind == .season, let code = target.episodeCode { return "\(verb) \(code)" }
        return verb
    }

    private func seasons(_ seasons: [MediaItem]) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            SeasonChips(seasons: seasons, chosen: season?.id) { s in Task { await pick(s) } }
            if !episodes.isEmpty, let season, episodesOf == season.id {
                downloadSeason(season.name, episodes)
            }
            LazyVStack(alignment: .leading, spacing: 14) {
                ForEach(episodes, id: \.id) { episode in
                    Button { model.play(episode) } label: { EpisodeRow(episode: episode) }
                        .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, Metrics.gutter)
        }
    }

    private func load() async {
        guard let server = model.server else { return }
        do {
            let detail = try await server.detail(itemId: item.id)
            self.detail = detail
            if detail.item.playable, streams.isEmpty, file == nil {
                // The file with its tracks' flags; the plain list when the
                // bridge cannot read it.
                // Every version comes with its file; the item's own shows first.
                versions = (try? await server.mediaVersions(itemId: item.id)) ?? []
                file = (versions.first(where: \.isDefault) ?? versions.first)?.file
                if file == nil { streams = (try? await server.streams(itemId: item.id)) ?? [] }
            }
            if model.enhanced?.bookmarks == true, detail.item.playable {
                bookmarks = (try? await server.bookmarks(itemId: item.id)) ?? []
            }
            if model.enhanced != nil, extras == nil, [.movie, .series].contains(detail.item.kind) {
                Task { extras = try? await server.seerrExtras(itemId: item.id) }
            }
            if [.episode, .season].contains(detail.item.kind) {
                await loadParents(of: detail.item, server: server)
            }
            if season == nil, let first = detail.seasons.first(where: { $0.id == detail.nextUp?.seasonId }) ?? detail.seasons.first {
                await pick(first)
            }
        } catch {
            self.error = Words.error(error)
        }
    }

    /// The show and, for an episode, its season: the pages the title links to.
    private func loadParents(of item: MediaItem, server: Server) async {
        let seasonId = item.kind == .episode ? item.seasonId : nil
        async let show: MediaItem? = Self.fetch(item.seriesId, from: server)
        async let parent: MediaItem? = Self.fetch(seasonId, from: server)
        (series, seasonItem) = await (show, parent)
    }

    private static func fetch(_ id: String?, from server: Server) async -> MediaItem? {
        guard let id else { return nil }
        return try? await server.item(itemId: id)
    }

    /// The season on screen, loaded again (after Mark played).
    private func reloadEpisodes() async {
        if let season { await pick(season) }
    }

    /// Plays from a place (a bookmark).
    private func play(_ item: MediaItem, at secs: Double) {
        model.play(item, startSecs: secs)
    }

    private func pick(_ season: MediaItem) async {
        self.season = season
        guard let server = model.server else { return }
        let loaded = (try? await server.episodes(seriesId: item.id, seasonId: season.id)) ?? []
        // Two quick taps: the season chosen last shows its episodes.
        guard self.season?.id == season.id else { return }
        episodes = loaded
        episodesOf = season.id
    }
}

/// A track a picker offers, in friendly words.
struct TrackChoice: Identifiable {
    let index: Int64
    let kind: TrackKind
    let name: TrackName
    let isDefault: Bool
    var id: Int64 { index }
}

/// A menu entry for a track: its name, the format as a quiet second line,
/// and a checkmark when chosen. The menu takes the image as the entry's
/// icon, the first text as its title and the second as its second line
/// (a `Label` with both texts lost the second line).
struct TrackEntry: View {
    let name: TrackName
    let on: Bool
    let choose: () -> Void

    var body: some View {
        Button(action: choose) {
            if on { Image(systemName: "checkmark") }
            Text(name.name)
            if let detail = name.detail { Text(detail) }
        }
        // The name and its detail, read as one ("English · SRT").
        .accessibilityLabel(name.full)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// The version for the next play (the Mac's Version box): one row in the
/// quiet pane of the track pickers, the files in a menu. The file section
/// under it and the track pickers follow the choice.
struct VersionPicker: View {
    let versions: [MediaVersion]
    @Binding var chosen: String?

    private var current: MediaVersion? {
        versions.first { $0.id == chosen } ?? versions.first(where: \.isDefault) ?? versions.first
    }

    var body: some View {
        Menu {
            ForEach(versions, id: \.id) { version in
                TrackEntry(name: TrackName(name: version.name, detail: version.detail), on: version.id == current?.id) {
                    chosen = version.isDefault ? nil : version.id
                }
            }
        } label: {
            TrackPickers.row("Version", symbol: "square.stack.3d.up", value: current?.name ?? "Default")
        }
        .accessibilityIdentifier("detail.version")
        .buttonStyle(.plain)
        .background(Color.ink.opacity(0.06), in: .rect(cornerRadius: Metrics.controlCorner))
        .frame(maxWidth: 480)
    }
}

/// Audio and subtitles for the next play (the Mac's track rows): a row
/// each in one quiet pane, the choice at the right.
struct TrackPickers: View {
    let choices: [TrackChoice]
    @Binding var audio: Int64?
    @Binding var subtitle: Int64?

    var body: some View {
        let audios = choices.filter { $0.kind == .audio }
        let subtitles = choices.filter { $0.kind == .subtitle }
        if audios.count > 1 || !subtitles.isEmpty {
            VStack(spacing: 0) {
                if audios.count > 1 {
                    Menu {
                        TrackEntry(name: TrackName(name: "Default", detail: Self.defaultName(audios)), on: audio == nil) { audio = nil }
                        ForEach(audios) { choice in
                            TrackEntry(name: choice.name, on: audio == choice.index) { audio = choice.index }
                        }
                    } label: {
                        Self.row("Audio", symbol: "speaker.wave.2", value: audios.first { $0.index == audio }?.name.name ?? "Default")
                    }
                    .accessibilityIdentifier("detail.audio")
                }
                if audios.count > 1, !subtitles.isEmpty {
                    Rectangle().fill(Color.ink.opacity(0.1)).frame(height: 0.5).padding(.leading, 50)
                }
                if !subtitles.isEmpty {
                    Menu {
                        TrackEntry(name: TrackName(name: "Default", detail: Self.defaultName(subtitles)), on: subtitle == nil) { subtitle = nil }
                        TrackEntry(name: TrackName(name: "Off", detail: nil), on: subtitle == -1) { subtitle = -1 }
                        ForEach(subtitles) { choice in
                            TrackEntry(name: choice.name, on: subtitle == choice.index) { subtitle = choice.index }
                        }
                    } label: {
                        Self.row("Subtitles", symbol: "captions.bubble",
                            value: subtitle == -1 ? "Off" : (subtitles.first { $0.index == subtitle }?.name.name ?? "Default"))
                    }
                    .accessibilityIdentifier("detail.subtitles")
                }
            }
            .buttonStyle(.plain)
            .background(Color.ink.opacity(0.06), in: .rect(cornerRadius: Metrics.controlCorner))
            .frame(maxWidth: 480)
        }
    }

    /// What "Default" plays: the track the file marks as its default.
    private static func defaultName(_ list: [TrackChoice]) -> String? {
        list.first(where: \.isDefault)?.name.name
    }

    static func row(_ title: String, symbol: String, value: String) -> some View {
        HStack(spacing: 12) {
            Image(systemName: symbol).frame(width: 24).foregroundStyle(Color.ink.opacity(0.7))
            Text(title)
            Spacer(minLength: 12)
            Text(value).foregroundStyle(Color.ink.opacity(0.6)).lineLimit(1)
            Image(systemName: "chevron.up.chevron.down").font(.caption2.weight(.semibold)).foregroundStyle(Color.ink.opacity(0.4))
        }
        .font(.subheadline)
        .padding(.horizontal, 14)
        .frame(minHeight: 48)
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
    }
}

/// The description: four lines, and the whole text on a tap when it is
/// longer.
struct Overview: View {
    let text: String
    @State private var open = false

    var body: some View {
        // About four lines of a phone hold this many letters.
        let long = text.count > 220
        VStack(alignment: .leading, spacing: 4) {
            Text(text)
                .font(.callout)
                .foregroundStyle(Color.ink.opacity(0.7))
                .lineSpacing(2)
                .lineLimit(open || !long ? nil : 4)
            if long {
                Text(open ? "Less" : "More").font(.footnote.weight(.semibold))
            }
        }
        .frame(maxWidth: 640, alignment: .leading)
        .contentShape(.rect)
        .onTapGesture { if long { open.toggle() } }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(long ? .isButton : [])
        .accessibilityHint(long ? (open ? "Shows less" : "Shows the whole text") : "")
    }
}

/// The item's bookmarks (Jellyfin Enhanced): a tap plays from one.
struct BookmarkStrip: View {
    let bookmarks: [Bookmark]
    let play: (Bookmark) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Metrics.shelfSpacing) {
            Text("Bookmarks").font(.title3.weight(.semibold))
            ScrollView(.horizontal) {
                HStack(spacing: 8) {
                    ForEach(bookmarks, id: \.id) { bookmark in
                        Button("\(Words.clock(bookmark.secs))\(bookmark.label.isEmpty ? "" : " · \(bookmark.label)")", systemImage: "bookmark.fill") {
                            play(bookmark)
                        }
                        .quietButton()
                    }
                }
            }
            .scrollIndicators(.hidden)
        }
        .padding(.horizontal, Metrics.gutter)
    }
}

/// The seasons of a series as chips; the chosen one is lit (the glass
/// highlight), the others are quiet words.
struct SeasonChips: View {
    let seasons: [MediaItem]
    let chosen: String?
    let pick: (MediaItem) -> Void

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                ForEach(seasons, id: \.id) { season in
                    let on = chosen == season.id
                    Button { pick(season) } label: {
                        Text(season.name)
                            .font(.subheadline.weight(on ? .semibold : .regular))
                            .foregroundStyle(Color.ink.opacity(on ? 1 : 0.6))
                            .padding(.horizontal, 16)
                            .frame(minHeight: 44)
                            .background(Color.ink.opacity(on ? 0.14 : 0), in: .capsule)
                            .contentShape(.capsule)
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
            .padding(.horizontal, Metrics.gutter)
        }
        .scrollIndicators(.hidden)
    }
}

/// Seerr's rows for a title, and its links to Seerr, Sonarr and Radarr.
struct SeerrExtrasSection: View {
    @Environment(\.openURL) private var openURL
    let extras: SeerrExtras

    var body: some View {
        SeerrRow(title: "Recommended on Seerr", cards: extras.recommended)
        SeerrRow(title: "Similar on Seerr", cards: extras.similar)
        if !extras.links.isEmpty {
            HStack(spacing: 10) {
                ForEach(extras.links, id: \.url) { link in
                    Button(link.name, systemImage: "arrow.up.right.square") {
                        if let url = URL(string: link.url) { openURL(url) }
                    }
                    .quietButton()
                }
            }
            .padding(.horizontal, Metrics.gutter)
        }
    }
}

/// The other episodes of an episode's season, sideways, with the one of
/// the page marked and scrolled into view (the Mac's row on an episode
/// page). A tap opens the episode's page.
struct EpisodeShelf: View {
    let title: String
    let episodes: [MediaItem]
    let current: String

    var body: some View {
        VStack(alignment: .leading, spacing: Metrics.shelfSpacing) {
            Text(title).font(.title3.weight(.semibold)).padding(.horizontal, Metrics.gutter)
            ScrollViewReader { proxy in
                ScrollView(.horizontal) {
                    LazyHStack(alignment: .top, spacing: Metrics.cardSpacing) {
                        ForEach(episodes, id: \.id) { episode in
                            CardLink(item: episode) { EpisodeCard(episode: episode, current: episode.id == current) }
                                .containerRelativeFrame(.horizontal) { width, _ in width < 600 ? width * 0.6 : Metrics.cardWidth(width, minimum: 280) }
                                .id(episode.id)
                        }
                    }
                    .scrollTargetLayout()
                }
                .contentMargins(.horizontal, Metrics.gutter, for: .scrollContent)
                .scrollTargetBehavior(.viewAligned)
                .scrollIndicators(.hidden)
                .onAppear { proxy.scrollTo(current, anchor: .leading) }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("detail.season-episodes")
    }
}

/// One episode in a row: its still, its number and name; the episode of
/// the page has a light ring and says so.
struct EpisodeCard: View {
    let episode: MediaItem
    let current: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picture(url: episode.still ?? episode.wide ?? episode.backdrop, symbol: "play.rectangle")
                .aspectRatio(16 / 9, contentMode: .fit)
                .overlay(alignment: .bottom) { ProgressLine(progress: episode.progress) }
                .overlay(alignment: .topTrailing) { Badge(item: episode) }
                .overlay {
                    if current {
                        RoundedRectangle(cornerRadius: 12).strokeBorder(Color.ink, lineWidth: 2)
                    }
                }
            Text([episode.episodeCode, episode.name].compactMap { $0 }.joined(separator: " · "))
                .font(.footnote.weight(.medium)).lineLimit(1)
            Text(current ? "This episode" : Words.runtime(episode.runtimeSecs) ?? " ")
                .font(.caption).foregroundStyle(.secondary).lineLimit(1)
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(current ? .isSelected : [])
    }
}

struct EpisodeRow: View {
    let episode: MediaItem

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Picture(url: episode.still ?? episode.wide ?? episode.backdrop, corner: 10, symbol: "play.rectangle")
                .frame(width: 150, height: 84)
                .overlay(alignment: .bottom) { ProgressLine(progress: episode.progress) }
                .overlay(alignment: .topTrailing) { Badge(item: episode) }
            VStack(alignment: .leading, spacing: 4) {
                Text([episode.episodeCode, episode.name].compactMap { $0 }.joined(separator: " · "))
                    .font(.subheadline.weight(.medium)).lineLimit(2)
                if let runtime = Words.runtime(episode.runtimeSecs) {
                    Text(runtime).font(.caption).foregroundStyle(.secondary)
                }
                if let overview = episode.overview {
                    Text(overview).font(.caption).foregroundStyle(.secondary).lineLimit(3)
                }
            }
            Spacer(minLength: 0)
        }
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
    }
}

/// "Created by" and the names, one quiet line under the genres.
struct CreatedBy: View {
    let creators: [PersonName]

    var body: some View {
        HStack(spacing: 4) {
            Text("Created by").foregroundStyle(Color.ink.opacity(0.6))
            ForEach(Array(creators.enumerated()), id: \.element.id) { index, person in
                NavigationLink(value: Route.person(id: person.id, name: person.name)) {
                    Text(person.name + (index < creators.count - 1 ? "," : ""))
                        .foregroundStyle(Color.ink)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(person.name)
                .accessibilityIdentifier("detail.creator.\(person.name)")
            }
        }
        .font(.footnote)
        .lineLimit(1)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("detail.createdBy")
    }
}

struct CastRow: View {
    let cast: [CastMember]

    var body: some View {
        if !cast.isEmpty {
            Shelf(title: "Cast & Crew") {
                ForEach(cast, id: \.id) { person in
                    NavigationLink(value: Route.person(id: person.id, name: person.name)) {
                        VStack(spacing: 6) {
                            Picture(url: person.image, corner: 48, symbol: "person.fill").frame(width: 96, height: 96)
                            Text(person.name).font(.caption.weight(.medium)).lineLimit(1)
                            Text(person.role).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                        }
                        .frame(width: 96)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }
}
