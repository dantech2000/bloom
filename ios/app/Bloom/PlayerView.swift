import BloomCore
import MPVUI
import SwiftUI
import UIKit

/// The player (spec §8.3): the controls are SwiftUI in `.regular` glass
/// (Q13); a tap shows or hides them; the picture is mpv's.
struct PlayerView: View {
    @Environment(AppModel.self) private var app
    /// Compact in height: a phone in landscape, which drops the header's
    /// extras (the Mac's trims).
    @Environment(\.verticalSizeClass) private var verticalSize
    @ScaledMetric(relativeTo: .largeTitle) private var bigIcon: CGFloat = 44
    @ScaledMetric(relativeTo: .caption2) private var markSize: CGFloat = 7
    let request: PlayRequest
    @Bindable var model: PlayerModel

    var body: some View {
        ZStack {
            // MPVUI's sample-buffer view: the picture Picture in Picture
            // and AirPlay take (spec §17).
            Group {
                if let engine = model.engine {
                    MPVVideoPlayer(player: engine)
                } else {
                    Color.black
                }
            }
            .ignoresSafeArea()
            .task { await model.start(request, server: app.server) }
            .contentShape(.rect)
            .onTapGesture { model.tapVideo() }
            .accessibilityLabel("Video")
            .accessibilityHint("Double tap to show the controls")
            .accessibilityAction(.magicTap) { model.togglePause() }

            if let external = model.engine?.externalPlayback, external.isActive || external.isPreparing {
                // The picture is on the AirPlay device; the controls still drive it.
                VStack(spacing: 10) {
                    Image(systemName: "airplay.video").font(.system(size: bigIcon))
                    Text(external.isPreparing ? "Preparing for AirPlay…" : "Playing on AirPlay").font(.headline)
                    if let error = external.lastError {
                        Text(String(describing: error)).font(.footnote).foregroundStyle(Color.fail)
                    }
                }
                .foregroundStyle(.secondary)
            } else if model.state.buffering, model.error == nil {
                ProgressView().controlSize(.large)
            }
            if let error = model.error {
                VStack(spacing: 12) {
                    Text(error).multilineTextAlignment(.center)
                    Button("Close") { close() }.buttonStyle(.glass)
                }
                .padding(24)
                .glassEffect(.regular, in: .rect(cornerRadius: 24))
            }
            if model.pauseScreen, !model.controls {
                PauseScreen(item: model.info?.item ?? request.item)
                    .transition(.opacity)
                    .allowsHitTesting(false)
            }
            // Hidden by fading, not removed: an open menu keeps its anchor.
            controls
                .opacity(model.controls ? 1 : 0)
                .allowsHitTesting(model.controls)
                .accessibilityHidden(!model.controls)
            if model.showInfo {
                InfoCard(model: model)
            }
            if !model.pauseScreen || model.controls {
                corner
            }
        }
        .background(.black)
        .persistentSystemOverlays(.hidden)
        .statusBarHidden()
        .onAppear { landscape(true) }
        .onDisappear { landscape(false) }
        .animation(.easeOut(duration: 0.15), value: model.controls)
        .animation(.easeOut(duration: 0.15), value: model.skippable?.kind)
        .accessibilityAction(.escape) { close() }
        // One sheet at a time over the video (CLAUDE.md, "One popup at a
        // time"). SyncPlay has its own here: the root's sheet is under this
        // cover.
        .sheet(item: sheet) { sheet in
            Group {
                switch sheet {
                case .episodes: EpisodePicker(model: model)
                case .bookmarks: BookmarksSheet(model: model)
                case .subtitleSearch: SubtitleSearchSheet(model: model)
                case .syncPlay: SyncPlaySheet()
                }
            }
            .bloomTheme()
            .toastHost()
            .environment(app)
        }
    }

    /// A sheet over the video.
    enum PlayerSheet: String, Identifiable {
        case episodes, bookmarks, subtitleSearch, syncPlay
        var id: String { rawValue }
    }

    /// The player's switch that is on, as one sheet; a dismissal turns it
    /// off. The bookmarks load again after their sheet, for the marks on
    /// the bar.
    private var sheet: Binding<PlayerSheet?> {
        Binding { shownSheet } set: { new in
            guard new == nil else { return }
            switch shownSheet {
            case .episodes: model.showEpisodes = false
            case .bookmarks:
                model.showBookmarks = false
                Task { await model.loadBookmarks() }
            case .subtitleSearch: model.showSubtitleSearch = false
            case .syncPlay: model.showSyncPlay = false
            case nil: break
            }
        }
    }

    private var shownSheet: PlayerSheet? {
        if model.showEpisodes { return .episodes }
        if model.showBookmarks { return .bookmarks }
        if model.showSubtitleSearch { return .subtitleSearch }
        if model.showSyncPlay { return .syncPlay }
        return nil
    }

    /// The symbol of a skip button; SF Symbols has a number for 5, 10, 15
    /// and 30 seconds, a plain arrow for the rest.
    static func skipSymbol(_ base: String, _ secs: Double) -> String {
        [5, 10, 15, 30].contains(Int(secs)) ? "\(base).\(Int(secs))" : base
    }

    static func delay(_ secs: Double) -> String {
        secs == 0 ? "None" : secs.formatted(.number.precision(.fractionLength(2)).sign(strategy: .always())) + " s"
    }

    /// The words of a skip button for a segment's kind.
    static func skipLabel(_ kind: SegmentKind) -> String {
        switch kind {
        case .intro: "Skip Intro"
        case .outro: "Skip Credits"
        case .recap: "Skip Recap"
        case .preview: "Skip Preview"
        case .commercial, .unknown: "Skip"
        }
    }

    // MARK: Header and bar

    private var controls: some View {
        GlassEffectContainer {
            controlStack
        }
    }

    private var controlStack: some View {
        VStack {
            HStack(alignment: .top) {
                // Close and the title, as small as the title lets it be.
                HStack(spacing: 12) {
                    Button("Close", systemImage: "chevron.down") { close() }
                        .labelStyle(.iconOnly)
                        .accessibilityIdentifier("player.close")
                    Text(model.info?.title ?? request.item.title).font(.headline).lineLimit(1)
                        .accessibilityIdentifier("player.title")
                }
                .padding(.horizontal, 16).padding(.vertical, 10)
                .glassEffect(.regular, in: .capsule)
                Spacer(minLength: 12)
                if verticalSize != .compact { headerExtras }
            }
            Spacer()
            HStack(spacing: 16) {
                transport
                if cornerBeside { cornerContent }
            }
            Spacer()
            bar
        }
        .padding(16)
    }

    /// Rating, "Ends at" and the favourite mark (an iPad; a phone drops them).
    private var headerExtras: some View {
        HStack(spacing: 12) {
            if app.enhanced?.ratingInPlayer == true, let item = model.info?.item {
                let rating = [item.officialRating, item.communityRating.map(Words.stars)].compactMap { $0 }
                if !rating.isEmpty {
                    Text(rating.joined(separator: "  ")).font(.footnote)
                }
            }
            if let ends = model.endsAt {
                Text("Ends at \(ends.formatted(date: .omitted, time: .shortened))")
                    .font(.footnote).foregroundStyle(.secondary).monospacedDigit()
            }
            Button(model.favorite ? "Remove from favorites" : "Add to favorites",
                   systemImage: model.favorite ? "heart.fill" : "heart") {
                Task { await model.toggleFavorite() }
            }
            .labelStyle(.iconOnly)
            .accessibilityIdentifier("player.favorite")
        }
        .padding(.horizontal, 16).padding(.vertical, 10)
        .glassEffect(.regular, in: .capsule)
    }

    /// Play, pause and the skips, in the middle of the picture.
    private var transport: some View {
        HStack(spacing: 34) {
            if model.previousEpisode != nil || model.nextItem != nil {
                Button("Previous", systemImage: "backward.end.fill") { Task { await model.previous() } }
                    .font(.title2)
                    .accessibilityIdentifier("player.previous")
            }
            Button("Back \(Int(model.skipBack)) seconds", systemImage: Self.skipSymbol("gobackward", model.skipBack)) {
                model.skip(-model.skipBack)
            }
            .font(.title)
            Button(model.state.paused ? "Play" : "Pause", systemImage: model.state.paused ? "play.fill" : "pause.fill") {
                model.togglePause()
            }
            .font(.system(size: bigIcon))
            .accessibilityIdentifier("player.pause")
            Button("Forward \(Int(model.skipForward)) seconds", systemImage: Self.skipSymbol("goforward", model.skipForward)) {
                model.skip(model.skipForward)
            }
            .font(.title)
            if model.nextItem != nil {
                Button("Next", systemImage: "forward.end.fill") { Task { await model.playNext() } }
                    .font(.title2)
                    .accessibilityIdentifier("player.next")
            }
        }
        .labelStyle(.iconOnly)
        .padding(.horizontal, 28).padding(.vertical, 14)
        .glassEffect(.regular, in: .capsule)
    }

    private var bar: some View {
        let duration = max(model.state.duration, 1)
        let position = model.scrubbing ?? model.state.position
        return VStack(spacing: 8) {
            if let scrub = model.scrubbing {
                ScrubPreview(model: model, at: scrub)
            }
            HStack {
                Text(Words.clock(position)).monospacedDigit()
                Slider(value: Binding(get: { position }, set: { model.scrub(to: $0) }), in: 0...duration) { editing in
                    if !editing { model.scrub(to: nil) }
                }
                .accessibilityIdentifier("player.position")
                // Bookmarks as small marks over the track (Jellyfin Enhanced).
                .overlay {
                    GeometryReader { box in
                        ForEach(model.bookmarks, id: \.id) { bookmark in
                            Image(systemName: "diamond.fill")
                                .font(.system(size: markSize))
                                .foregroundStyle(Color.ink)
                                .position(x: box.size.width * min(1, bookmark.secs / duration), y: -6)
                        }
                    }
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
                }
                .accessibilityLabel("Position")
                .accessibilityValue("\(Words.clock(position)) of \(Words.clock(duration))")
                Text("-" + Words.clock(max(duration - position, 0))).monospacedDigit()
            }
            .font(.footnote)
            // A view of its own: it reads nothing that changes with the
            // time, so an open menu is not rebuilt (and closed) each tick.
            PlayerTools(model: model)
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
        .glassEffect(.regular, in: .rect(cornerRadius: 24))
    }

    // MARK: Skip and Up next, at the bottom right above the bar

    private var corner: some View {
        GlassEffectContainer {
            cornerStack
        }
    }

    /// With the controls up on a phone in landscape, the room above the bar
    /// is the transport's: Skip and Up next sit beside it instead.
    private var cornerBeside: Bool { model.controls && verticalSize == .compact }

    private var cornerStack: some View {
        VStack {
            Spacer()
            HStack {
                Spacer()
                if !cornerBeside { cornerContent }
            }
        }
        .padding(.horizontal, 24)
        .padding(.bottom, model.controls ? 160 : 32)
    }

    @ViewBuilder
    private var cornerContent: some View {
        if let next = model.upNext {
            upNextCard(next)
        } else if let segment = model.skippable {
            Button(Self.skipLabel(segment.kind)) { model.skipSegment() }
                .buttonStyle(.glass)
                .controlSize(.large)
                .accessibilityIdentifier("player.skip")
                .transition(.opacity)
        }
    }

    private func upNextCard(_ next: MediaItem) -> some View {
        HStack(spacing: 12) {
            Picture(url: next.still ?? next.wide ?? next.backdrop, corner: 10).frame(width: 128, height: 72)
            VStack(alignment: .leading, spacing: 4) {
                Text("Up next").font(.caption).foregroundStyle(.secondary)
                Text([next.episodeCode, next.name].compactMap { $0 }.joined(separator: " · ")).font(.subheadline.weight(.medium)).lineLimit(2)
                HStack {
                    Button("Play Now") { Task { await model.playNext() } }
                        .primaryButton()
                        .accessibilityIdentifier("player.upnext.play")
                    Button("Hide") { model.upNextHidden = true }
                        .buttonStyle(.glass)
                        .accessibilityIdentifier("player.upnext.hide")
                }
                .controlSize(.small)
            }
        }
        .padding(12)
        .frame(maxWidth: 380)
        .glassEffect(.regular, in: .rect(cornerRadius: 20))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Up next")
    }

    // MARK: Close and orientation

    private func close() {
        app.closePlayer()
    }

    /// A phone turns to landscape while the player is open; the layout works
    /// at any size, since iOS 27 can ignore the request. The idiom decides
    /// only whether to ask, never the layout (spec §4). The mask changes
    /// first: asking for portrait while landscape is allowed does nothing
    /// (spike S3).
    private func landscape(_ on: Bool) {
        guard UIDevice.current.userInterfaceIdiom == .phone,
              let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { return }
        let mask: UIInterfaceOrientationMask = on ? .landscape : .portrait
        AppDelegate.orientations = mask
        scene.keyWindow?.rootViewController?.setNeedsUpdateOfSupportedInterfaceOrientations()
        scene.requestGeometryUpdate(.iOS(interfaceOrientations: mask)) { _ in }
    }
}

/// The episodes of the season playing, as a sheet over the video; a tap
/// plays one in this player.
struct EpisodePicker: View {
    let model: PlayerModel

    var body: some View {
        NavigationStack {
            ScrollViewReader { scroller in
                List(model.seasonEpisodes, id: \.id) { episode in
                    Button {
                        Task { await model.play(episode) }
                    } label: {
                        EpisodeRow(episode: episode)
                            .overlay(alignment: .topLeading) {
                                if episode.id == model.info?.itemId {
                                    Text("Now playing").font(.caption2.weight(.semibold))
                                        .padding(.horizontal, 6).padding(.vertical, 2)
                                        .background(.black.opacity(0.6), in: .capsule).padding(6)
                                }
                            }
                    }
                    .buttonStyle(.plain)
                    .id(episode.id)
                }
                .listStyle(.plain)
                .onAppear { scroller.scrollTo(model.info?.itemId, anchor: .center) }
            }
            .navigationTitle(model.info?.item.seriesName ?? "Episodes")
            .navigationBarTitleDisplayMode(.inline)
        }
        .presentationDetents([.large])
        .bloomTheme()
        .preferredColorScheme(.dark)
    }
}

/// The frame, the time and the chapter under the thumb while scrubbing.
struct ScrubPreview: View {
    let model: PlayerModel
    let at: Double

    var body: some View {
        VStack(spacing: 4) {
            if let frame = model.previewFrame(at: at) {
                Image(decorative: frame, scale: 1)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 200)
                    .clipShape(.rect(cornerRadius: 10))
            }
            Text([model.chapter(at: at), Words.clock(at)].compactMap { $0 }.joined(separator: " · "))
                .font(.footnote.weight(.medium)).monospacedDigit()
        }
        .padding(8)
        .glassEffect(.regular, in: .rect(cornerRadius: 14))
        .accessibilityHidden(true)
    }
}

/// After 5 s of pause with the controls hidden: the title and the
/// overview over a dimmed picture (the Mac's pause screen).
struct PauseScreen: View {
    let item: MediaItem

    var body: some View {
        ZStack(alignment: .bottomLeading) {
            Color.black.opacity(0.55)
            VStack(alignment: .leading, spacing: 10) {
                Text("Paused").font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                Text(item.title).font(.largeTitle.bold()).lineLimit(2)
                if let overview = item.overview {
                    Text(overview).font(.body).foregroundStyle(.secondary).lineLimit(4)
                }
            }
            .frame(maxWidth: 560, alignment: .leading)
            .padding(40)
        }
        .ignoresSafeArea()
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("player.pause-screen")
    }
}

/// How the item plays, refreshed every second while it shows.
struct InfoCard: View {
    let model: PlayerModel

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Playback info").font(.headline)
                Spacer()
                Button("Close", systemImage: "xmark") { model.showInfo = false }.labelStyle(.iconOnly)
            }
            ForEach(model.infoLines, id: \.0) { name, value in
                HStack(alignment: .firstTextBaseline) {
                    Text(name).foregroundStyle(.secondary)
                    Spacer(minLength: 12)
                    Text(value).monospacedDigit().multilineTextAlignment(.trailing)
                }
                .font(.footnote)
            }
        }
        .padding(16)
        .frame(width: 300)
        .glassEffect(.regular, in: .rect(cornerRadius: 20))
        // At the left: the corner at the right holds Skip and Up next.
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .padding(.top, 90)
        .padding(.leading, 16)
        .accessibilityIdentifier("player.info-card")
    }
}
