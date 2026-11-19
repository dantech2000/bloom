// The native iOS app (specs/ios-native): SwiftUI over the Rust core.
import BloomCore
import OSLog
import SwiftUI
import UIKit

@main
struct BloomApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    init() {
        installPanicHook()
    }

    var body: some Scene {
        WindowGroup {
            if ProcessInfo.processInfo.environment["BLOOM_SPIKE"] == "1" {
                SpikeView()
            } else {
                RootView()
            }
        }
    }
}

/// Holds the orientations the app allows. On an iPhone the pages are
/// portrait and the player landscape, as in the GPUI app (spec §8.3); an
/// iPad allows every orientation. Asking for portrait while landscape is
/// still allowed leaves the scene where it is (spike S3, 2026-11-14), so
/// the mask changes first and the request follows.
final class AppDelegate: NSObject, UIApplicationDelegate {
    static var orientations: UIInterfaceOrientationMask =
        UIDevice.current.userInterfaceIdiom == .phone ? .portrait : .all

    func application(_ application: UIApplication,
                     supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
        Self.orientations
    }

    /// iOS woke the app for its downloads: the session is made again (its
    /// events come to it), and the handler goes when they are delivered.
    func application(_ application: UIApplication, handleEventsForBackgroundURLSession identifier: String,
                     completionHandler: @escaping () -> Void) {
        guard identifier == Downloads.sessionId else { return completionHandler() }
        nonisolated(unsafe) let completion = completionHandler
        DownloadDelegate.backgroundCompletion.withLock { $0 = { completion() } }
        _ = Downloads.shared
    }
}

/// The colours of DESIGN.md: one ink; colour only for state.
extension Color {
    static let ink = Color(red: 0xf5 / 255, green: 0xf5 / 255, blue: 0xf7 / 255)
    static let page = Color(red: 0x10 / 255, green: 0x10 / 255, blue: 0x10 / 255)
    /// The text of a white button (DESIGN.md, "Ink on White").
    static let inkOnWhite = Color(red: 0x12 / 255, green: 0x12 / 255, blue: 0x12 / 255)
    static let live = Color(red: 0x7e / 255, green: 0xe7 / 255, blue: 0x87 / 255)
    static let fail = Color(red: 0xff / 255, green: 0x7b / 255, blue: 0x72 / 255)
    /// Amber: "needs a look" (the Mac's `State::Warn`).
    static let warn = Color(red: 0xe3 / 255, green: 0xb3 / 255, blue: 0x41 / 255)
}

/// The system switch with "on" in the state green. The app's tint is the
/// ink, and a switch fills its "on" track with the tint: a white thumb on
/// an off-white track, which reads as neither on nor off.
struct BloomSwitch: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        Toggle(configuration).toggleStyle(.switch).tint(.live)
    }
}

extension View {
    /// The look of a page or a presentation: the ink as the tint, and
    /// switches that show their state. A sheet does not take these from
    /// the page under it, so each presentation sets them.
    func bloomTheme() -> some View {
        tint(.ink).toggleStyle(BloomSwitch())
    }
}

/// The log of the views (Console, subsystem dev.bloom.ios.next).
enum ViewLog {
    static let pages = Logger(subsystem: "dev.bloom.ios.next", category: "pages")
}

extension Words {
    /// A community rating: "★ 7.4".
    static func stars(_ rating: Float) -> String {
        "★ " + rating.formatted(.number.precision(.fractionLength(1)))
    }
}

extension Font {
    /// A display title (spec §7, Q2): Google Sans Bold, scaled with Dynamic
    /// Type like the text style it stands for.
    static func display(_ size: CGFloat, relativeTo style: Font.TextStyle = .largeTitle) -> Font {
        .custom("GoogleSans-Bold", size: size, relativeTo: style)
    }
}
