import XCTest

/// The random walk of spec §11: seeded random input against the mock
/// server, with checks after every step. A failure prints the seed and the
/// steps; MONKEY_SEED replays a run, MONKEY_STEPS sets its length.
///
/// Safety (spec §10): the mock server only, the fake Chromecast, no
/// reports, muted; a deny list keeps the walk away from writes and from
/// the end of the session even on the mock.
@MainActor
final class Monkey: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }
    private var dir: String { env["SHOT_DIR"] ?? "/tmp/bloom-shots" }

    /// Labels the walk never taps.
    private let deny = ["Sign Out", "Remove", "Delete", "Forget", "Revoke", "Restart", "Shut Down",
                        "Uninstall", "Request", "Servers & profiles", "Sign In the Device", "Change server"]

    override func setUp() {
        // From the mock's first state, upright, whatever ran before.
        MockServer.reset()
        XCUIDevice.shared.orientation = .portrait
    }

    override func tearDown() {
        // The walk may end in landscape; the next test starts upright.
        XCUIDevice.shared.orientation = .portrait
        // And with the mock as it was: the walk taps what it finds.
        MockServer.reset()
    }

    private struct Random {
        var state: UInt64
        mutating func next() -> UInt64 {
            state &+= 0x9E37_79B9_7F4A_7C15
            var z = state
            z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
            z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
            return z ^ (z >> 31)
        }
        mutating func below(_ n: Int) -> Int { n <= 0 ? 0 : Int(next() % UInt64(n)) }
        mutating func unit() -> Double { Double(next() % 10_000) / 10_000 }
    }

    func testRandomWalk() throws {
        let seed = UInt64(env["MONKEY_SEED"] ?? "") ?? UInt64.random(in: 1...999_999)
        let steps = Int(env["MONKEY_STEPS"] ?? "") ?? 120
        var random = Random(state: seed)
        var log: [String] = []
        print("MONKEY: seed \(seed), \(steps) steps")

        let app = XCUIApplication()
        app.launchEnvironment = MockServer.environment(controlsStay: false)
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "Home did not load")

        for step in 1...steps {
            let action = act(app, &random)
            log.append("\(step): \(action)")
            print("MONKEY: \(step) \(action)")
            if let broken = check(app) {
                let png = XCUIScreen.main.screenshot().pngRepresentation
                try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
                try? png.write(to: URL(fileURLWithPath: "\(dir)/monkey-\(seed)-step\(step).png"))
                XCTFail("MONKEY failed at step \(step) (seed \(seed)): \(broken)\n" + log.suffix(15).joined(separator: "\n"))
                return
            }
        }
        print("MONKEY: \(steps) steps, no check broken (seed \(seed))")
    }

    /// One random action; its description for the log.
    private func act(_ app: XCUIApplication, _ random: inout Random) -> String {
        let window = app.windows.firstMatch.frame
        func point(_ x: Double, _ y: Double) -> XCUICoordinate {
            app.coordinate(withNormalizedOffset: CGVector(dx: x, dy: y))
        }
        switch random.below(100) {
        case 0..<40:
            // A control on screen, not denied.
            // On screen by its frame first: asking an off-screen element (a
            // hero slide to the side) whether it is hittable fails the test.
            // One snapshot of the screen: a query per button fails the test
            // when the button leaves between the list and the question (a
            // row that reloads), and a snapshot's failure can be caught.
            let onScreen = window.insetBy(dx: 1, dy: 1)
            guard let root = try? app.snapshot() else { return "no snapshot of the screen" }
            var buttons: [(label: String, frame: CGRect)] = []
            func collect(_ element: XCUIElementSnapshot) {
                if element.elementType == .button {
                    let frame = element.frame
                    if !frame.isEmpty, onScreen.contains(CGPoint(x: frame.midX, y: frame.midY)),
                       !deny.contains(where: { element.label.localizedCaseInsensitiveContains($0) }) {
                        buttons.append((element.label, frame))
                    }
                }
                element.children.forEach(collect)
            }
            collect(root)
            guard !buttons.isEmpty else { return "no control to tap" }
            let (label, frame) = buttons[random.below(buttons.count)]
            // At its centre, as a finger would: what is really there takes
            // the tap (XCUITest refuses a hittability query on a clipped one).
            app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
            return "tap \"\(label)\""
        case 40..<52:
            // A point, away from the top bar's account button and the edges.
            let (x, y) = (0.1 + random.unit() * 0.8, 0.15 + random.unit() * 0.7)
            point(x, y).tap()
            return String(format: "tap point %.2f,%.2f", x, y)
        case 52..<72:
            // A swipe in one of four directions at about 200, 600 or 1,000 pt/s.
            let speeds: [XCUIGestureVelocity] = [200, 600, 1000]
            let speed = speeds[random.below(3)]
            let (dx, dy): (Double, Double) = [(0, -0.35), (0, 0.35), (-0.35, 0), (0.35, 0)][random.below(4)]
            let start = point(0.5, 0.5)
            start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: dx * window.width, dy: dy * window.height)),
                        withVelocity: speed, thenHoldForDuration: 0)
            return "swipe \(dx),\(dy) at \(speed.rawValue) pt/s"
        case 72..<78:
            // An edge swipe from the left: the system's Back.
            let start = point(0.01, 0.5)
            start.press(forDuration: 0.05, thenDragTo: point(0.7, 0.5), withVelocity: 800, thenHoldForDuration: 0)
            return "edge swipe back"
        case 78..<84:
            // A long press: a card's menu, or nothing.
            let (x, y) = (0.15 + random.unit() * 0.7, 0.3 + random.unit() * 0.5)
            point(x, y).press(forDuration: 1.1)
            return String(format: "long press %.2f,%.2f", x, y)
        case 84..<89:
            let back = app.navigationBars.buttons.firstMatch
            if back.exists, back.isHittable { back.tap(); return "back" }
            return "back (none)"
        case 89..<93:
            let orientation: UIDeviceOrientation = random.below(2) == 0 ? .landscapeLeft : .portrait
            XCUIDevice.shared.orientation = orientation
            return "rotate \(orientation == .portrait ? "portrait" : "landscape")"
        case 93..<96:
            XCUIDevice.shared.press(.home)
            sleep(1)
            app.activate()
            _ = app.wait(for: .runningForeground, timeout: 10)
            return "home and return"
        default:
            let field = app.searchFields.firstMatch
            if field.exists, field.isHittable {
                field.tap()
                field.typeText(["the", "a", "night", "zz"][random.below(4)])
                return "type in search"
            }
            // The Search tab, when the tab bar is there (not in the player
            // or under a sheet).
            let search = app.buttons["Search"].firstMatch
            guard search.exists else { return "open Search (no tab bar)" }
            search.tap()
            return "open Search"
        }
    }

    /// The checks of spec §11 that hold on correct screens; nil when all
    /// hold. (A "hittable outside the window" check fired on correct screens,
    /// as the review warned: carousels and scroll views hold such elements.)
    private func check(_ app: XCUIApplication) -> String? {
        // A link (a trailer, a Seerr card, Jellyfin Web, Settings) leaves the
        // app for Safari or Settings: back to the app, the walk goes on. A
        // crash is an app that is not running at all.
        if app.state == .runningBackground || app.state == .runningBackgroundSuspended {
            print("MONKEY: a link left the app; back to it")
            app.activate()
            _ = app.wait(for: .runningForeground, timeout: 10)
        }
        guard app.state == .runningForeground else { return "the app is not running (state \(app.state.rawValue))" }
        return nil
    }
}
