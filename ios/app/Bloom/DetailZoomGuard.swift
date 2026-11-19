import SwiftUI
import UIKit

/// Keeps a pull at the top of a detail page from closing it. The page
/// comes in with SwiftUI's zoom transition (Routes.swift), and iOS lets a
/// zoomed page be dragged down to close: on a page that scrolls, a pull
/// back to the top did that (the tester, three times). SwiftUI has no
/// switch for it, so this finds the zoom's own drag recognizers on the
/// page's controller and turns them off while the page shows. The swipe
/// from the left edge and the back button still close the page.
struct ZoomDismissGuard: UIViewRepresentable {
    func makeUIView(context: Context) -> GuardView { GuardView() }
    func updateUIView(_ view: GuardView, context: Context) {}

    final class GuardView: UIView {
        private var turnedOff: [UIGestureRecognizer] = []

        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil {
                // Off the screen: the recognizers go back as they were.
                turnedOff.forEach { $0.isEnabled = true }
                turnedOff = []
                return
            }
            // The transition adds its recognizers as the page lands.
            for delay in [0.0, 0.3, 0.8] {
                DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in self?.guardPage() }
            }
        }

        private func guardPage() {
            guard window != nil, let controller = owner() else { return }
            var views: [UIView] = []
            var view: UIView? = controller.view
            while let current = view {
                views.append(current)
                view = current.superview
            }
            // BLOOM_ZOOM_GUARD=0: the page as iOS has it, for a test of the guard.
            let on = ProcessInfo.processInfo.environment["BLOOM_ZOOM_GUARD"] != "0"
            for view in views where on {
                for recognizer in view.gestureRecognizers ?? [] where Self.isZoomDismiss(recognizer) && recognizer.isEnabled {
                    recognizer.isEnabled = false
                    turnedOff.append(recognizer)
                }
            }
            if ProcessInfo.processInfo.environment["BLOOM_ZOOM_PROBE"] == "1" {
                for view in views {
                    for recognizer in view.gestureRecognizers ?? [] {
                        NSLog("ZOOMPROBE: %@ name=%@ enabled=%d on %@", String(describing: type(of: recognizer)),
                              recognizer.name ?? "-", recognizer.isEnabled ? 1 : 0, String(describing: type(of: view)))
                    }
                }
            }
        }

        /// The zoom's drag down to close, by the name UIKit gives it
        /// ("com.apple.UIKit.ZoomInteractiveDismissSwipeDown" on iOS 27,
        /// seen with BLOOM_ZOOM_PROBE=1). The leading-edge pan and the
        /// pinch are recognizers of their own and stay.
        private static func isZoomDismiss(_ recognizer: UIGestureRecognizer) -> Bool {
            let name = recognizer.name ?? ""
            return name.localizedCaseInsensitiveContains("Zoom") && name.localizedCaseInsensitiveContains("SwipeDown")
        }

        /// The controller that holds this page: the first one up the
        /// responder chain.
        private func owner() -> UIViewController? {
            var responder: UIResponder? = self
            while let next = responder?.next {
                if let controller = next as? UIViewController { return controller }
                responder = next
            }
            return nil
        }
    }
}
