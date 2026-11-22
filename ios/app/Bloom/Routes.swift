import BloomCore
import SwiftUI

/// The namespace of the zoom from a card to its page: one per navigation
/// stack, so a card on a pushed page zooms too (one `navigationDestination`
/// per stack: SwiftUI uses only the one nearest the root).
extension EnvironmentValues {
    @Entry var zoomSpace: Namespace.ID? = nil
    @Entry var inCollection: String? = nil
}

struct ZoomSource: ViewModifier {
    let id: String
    let space: Namespace.ID?

    func body(content: Content) -> some View {
        if let space {
            content.matchedTransitionSource(id: id, in: space)
        } else {
            content
        }
    }
}

/// The destinations of a navigation stack, set once at its root.
struct Destinations: ViewModifier {
    let namespace: Namespace.ID
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content
            .environment(\.zoomSpace, namespace)
            .navigationDestination(for: Route.self) { route in
                destination(route).environment(\.zoomSpace, namespace)
            }
    }

    @ViewBuilder
    private func destination(_ route: Route) -> some View {
        Group {
            switch route {
            case .item(let item):
                if item.kind == .playlist {
                    PlaylistPage(playlist: item)
                } else if item.kind == .studio {
                    LibraryPage(source: .studio(item.id, item.name))
                } else if [.boxSet, .folder, .collectionFolder].contains(item.kind) {
                    LibraryPage(source: GridSource(item))
                } else {
                    // Reduce Motion: a plain push, no zoom (spec §6.2).
                    if reduceMotion {
                        DetailView(item: item)
                    } else {
                        DetailView(item: item).navigationTransition(.zoom(sourceID: item.id, in: namespace))
                    }
                }
            case .library(let library):
                LibraryPage(source: GridSource(library))
            case .favorites:
                LibraryPage(source: .favorites)
            case .playlists:
                PlaylistsPage()
            case .person(let id, let name):
                BrowsePersonPage(id: id, name: name)
            }
        }
    }
}
