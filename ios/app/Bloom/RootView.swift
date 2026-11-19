import BloomCore
import SwiftUI

/// The fixed tabs of spec §6.1: Home, Library, Downloads, Search. On an
/// iPad the same tabs are a sidebar, with each library one click away.
struct RootView: View {
    @State private var model = AppModel()
    @Environment(\.horizontalSizeClass) private var width

    var body: some View {
        Group {
            switch model.screen {
            case .loading:
                Splash()
            case .connect:
                ConnectView()
            case .failed(let why):
                OfflinePage(why: why)
            case .main:
                tabs
            }
        }
        .toastHost()
        .environment(model)
        .preferredColorScheme(.dark)
        .bloomTheme()
        .task { await model.start() }
        // One sheet at a time (CLAUDE.md, "One popup at a time"). A
        // presentation does not take the tint of the page under it.
        .sheet(item: sheet) { sheet in
            Group {
                switch sheet {
                case .account: AccountSheet()
                case .settings: SettingsSheet()
                case .playOn: PlayOnSheet()
                case .dashboard: DashboardSheet()
                case .syncPlay: SyncPlaySheet()
                case .add(let request): AddToListSheet(request: request)
                }
            }
            .bloomTheme()
            .toastHost()
            .environment(model)
        }
        .fullScreenCover(item: $model.playing) { request in
            PlayerView(request: request, model: model.player ?? model.playerFor(request)).bloomTheme().toastHost().environment(model)
        }
        .overlay {
            if model.miniPlayer, let player = model.player {
                // Outside `.environment(model)` above: the model goes in here.
                MiniPlayer(model: player).environment(model)
            }
        }
    }

    /// The sheet of the root. The model keeps a switch per sheet, set from
    /// many places; this is the one of them that shows, in this order, and
    /// a dismissal turns off the one that showed.
    private var sheet: Binding<AppSheet?> {
        Binding { shownSheet } set: { new in
            guard new == nil else { return }
            switch shownSheet {
            case .account: model.account = false
            case .settings: model.settings = false
            case .playOn: model.showPlayOn = false
            case .dashboard: model.showDashboard = false
            case .syncPlay: model.showSyncPlay = false
            case .add: model.addRequest = nil
            case nil: break
            }
        }
    }

    private var shownSheet: AppSheet? {
        if model.account { return .account }
        if model.settings { return .settings }
        if model.showPlayOn { return .playOn }
        if model.showDashboard { return .dashboard }
        if model.showSyncPlay { return .syncPlay }
        if let request = model.addRequest { return .add(request) }
        return nil
    }

    private var tabs: some View {
        TabView(selection: $model.tab) {
            Tab("Home", systemImage: "house", value: AppTab.home) {
                NavigationStack {
                    // Offline, Home is the offline page (spec §16.9).
                    if model.offline { OfflineHome() } else { HomeView() }
                }
            }
            Tab("Library", systemImage: "square.stack", value: AppTab.library) {
                NavigationStack { LibraryTab() }
            }
            Tab("Downloads", systemImage: "arrow.down.circle", value: AppTab.downloads) {
                NavigationStack { DownloadsView() }
            }
            Tab(value: AppTab.search, role: .search) {
                NavigationStack { SearchView() }
            }
        }
        .tabViewStyle(.sidebarAdaptable)
        .tabBarMinimizeBehavior(.onScrollDown)
        // What plays on another device, above the tab bar (spec §6.1);
        // a server gone away comes first (spec §16.9), except on Home,
        // whose page says it already.
        .tabViewBottomAccessory(isEnabled: (model.offline && model.tab != .home) || model.remote != nil || model.castDevice != nil) {
            if model.offline && model.tab != .home {
                OfflineAccessory()
            } else if model.castDevice != nil {
                CastAccessory()
            } else {
                RemoteAccessory()
            }
        }
    }
}

/// The account button on each tab's root (spec §6.1).
struct AccountButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Button { model.account = true } label: {
            Image(systemName: "person.crop.circle")
        }
        .accessibilityLabel("Account")
        .accessibilityIdentifier("account")
    }
}

/// A sheet of the root.
enum AppSheet: Identifiable, Equatable {
    case account, settings, playOn, dashboard, syncPlay
    case add(AddRequest)

    var id: String {
        switch self {
        case .account: "account"
        case .settings: "settings"
        case .playOn: "playOn"
        case .dashboard: "dashboard"
        case .syncPlay: "syncPlay"
        case .add(let request): "add." + request.id
        }
    }

    static func == (a: AppSheet, b: AppSheet) -> Bool { a.id == b.id }
}

/// The presentations that can show the toast, the root first. A sheet or
/// the player covers the root's overlay, so each presentation has one,
/// and only the one that appeared last (the one on top) shows the toast.
@Observable
final class ToastHosts {
    static let shared = ToastHosts()
    private(set) var stack: [UUID] = []
    /// How many views of a host are on screen. A host on a `Group` is on
    /// each of its views, and a switch of view (the splash to the tabs)
    /// can show the new one before the old one goes.
    @ObservationIgnored private var shown: [UUID: Int] = [:]

    func push(_ id: UUID) {
        shown[id, default: 0] += 1
        if !stack.contains(id) { stack.append(id) }
    }

    func pop(_ id: UUID) {
        shown[id, default: 1] -= 1
        guard shown[id] == 0 else { return }
        shown[id] = nil
        stack.removeAll { $0 == id }
    }
}

private struct ToastHost: ViewModifier {
    @State private var id = UUID()

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .top) {
                if ToastHosts.shared.stack.last == id { Toast() }
            }
            .onAppear { ToastHosts.shared.push(id) }
            .onDisappear { ToastHosts.shared.pop(id) }
    }
}

extension View {
    /// Shows the app's toast over this presentation while it is on top.
    func toastHost() -> some View { modifier(ToastHost()) }
}

/// The app's toast (spec §6.3): glass, at the top, gone after 2.5 s.
struct Toast: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Group {
            if let text = model.toast {
                // A long sentence takes two lines inside the gutters, not
                // the width of the screen.
                Text(text)
                    .font(.subheadline.weight(.medium))
                    .multilineTextAlignment(.center)
                    .lineLimit(3)
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
                    .frame(minHeight: 44)
                    .glassEffect(.regular, in: .rect(cornerRadius: 22))
                    .padding(.horizontal, Metrics.gutter)
                    .padding(.top, 8)
                    .transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
                    .task(id: text) {
                        try? await Task.sleep(for: .seconds(2.5))
                        // A toast that moves to another host (a sheet that
                        // closes) cancels this wait: it stays for the new one.
                        guard !Task.isCancelled else { return }
                        if model.toast == text { model.toast = nil }
                    }
                    .accessibilityIdentifier("toast")
            }
        }
        .animation(.easeOut(duration: 0.2), value: model.toast)
    }
}


/// The server could not be reached (inventory §1.3): why, a countdown to
/// the next try (5 s, doubling to a minute), Try Again now, the downloads
/// when there are some, and the server list.
struct OfflinePage: View {
    @Environment(AppModel.self) private var model
    let why: String
    @State private var wait = 5
    @State private var left = 5

    var body: some View {
        ContentUnavailableView {
            Label("The server could not load", systemImage: "wifi.exclamationmark")
        } description: {
            VStack(spacing: 8) {
                Text(why)
                Text("Trying again in \(left) s").font(.footnote).monospacedDigit().foregroundStyle(.secondary)
            }
        } actions: {
            Button("Try Again") { Task { await model.reload() } }
                .primaryButton()
            if !Downloads.shared.entries.isEmpty {
                Button("Open Downloads") { model.goOffline(why, announce: false) }
            }
            Button("Servers & profiles…") { model.showServers() }
        }
        .background(Color.page)
        .task {
            // The countdown; each failed try doubles the wait, to a minute.
            while !Task.isCancelled {
                left = wait
                while left > 0, !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(1))
                    left -= 1
                }
                guard !Task.isCancelled else { return }
                await model.reload()
                wait = min(wait * 2, 60)
            }
        }
    }
}

/// The server is offline, in the tab bar's accessory (the Mac's top-bar
/// chip, spec §16.9): its name and Retry; a tap elsewhere opens the
/// downloads.
struct OfflineAccessory: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "wifi.slash")
            Text("\(model.serverName) is offline")
                .font(.subheadline.weight(.medium))
                .lineLimit(1)
            Spacer()
            if model.retrying {
                ProgressView()
            } else {
                Button("Retry") { Task { await model.retry() } }
                    .font(.subheadline.weight(.semibold))
                    .accessibilityIdentifier("offline.retry")
            }
        }
        .padding(.horizontal)
        .contentShape(.rect)
        .onTapGesture { model.tab = .downloads }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("offline.accessory")
    }
}

/// Home while offline: why, Retry, and the downloads.
struct OfflineHome: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ContentUnavailableView {
            Label("\(model.serverName) is offline", systemImage: "wifi.slash")
        } description: {
            Text(model.offlineWhy)
        } actions: {
            Button("Retry") { Task { await model.retry() } }
                .primaryButton()
                .disabled(model.retrying)
                .accessibilityIdentifier("offline.retry")
            Button("Open Downloads") { model.tab = .downloads }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.page)
        .toolbar { ToolbarItem(placement: .topBarTrailing) { AccountButton() } }
        .accessibilityIdentifier("offline.home")
    }
}

/// What shows while the app opens (the Mac's splash, inventory §1.1): the
/// icon blooms in with a soft glow, the name fades in under it. It is
/// only there while the pages load, so it never adds a wait; Reduce
/// Motion shows it still.
struct Splash: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var shown = false

    var body: some View {
        VStack(spacing: 18) {
            Image("BloomMark")
                .resizable()
                .scaledToFit()
                .frame(width: 112, height: 112)
                .clipShape(.rect(cornerRadius: 26))
                .shadow(color: .white.opacity(shown ? 0.18 : 0), radius: 30)
                .scaleEffect(shown || reduceMotion ? 1 : 0.82)
                .opacity(shown || reduceMotion ? 1 : 0)
            Text("Bloom")
                .font(.title2.weight(.semibold))
                .opacity(shown || reduceMotion ? 1 : 0)
            ProgressView().opacity(shown ? 0.6 : 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.page)
        .onAppear {
            withAnimation(.easeOut(duration: 0.6)) { shown = true }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Bloom is opening")
    }
}
