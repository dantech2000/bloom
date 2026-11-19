import XCTest

/// The performance budget (spec §7; specs/ios-native/README.md,
/// "Performance budget"): the hitches of a scroll, the cold launch, and
/// the time and memory of the detail page and the player. Against
/// dev/mock-jellyfin with extra films (BLOOM_MOCK_EXTRA_MOVIES), so the
/// grid and the search have pages to scroll.
///
/// `dev/ios-next perf [name]` runs them on a simulator, `dev/ios-next
/// perf-device [udid]` on a phone; `dev/ios-next test` skips this class
/// (slow, and noisy beside other work). XCTest does not hand a measured
/// value to the test, so the budget is checked after the run: the script
/// reads the metrics from the result bundle (`xcresulttool get
/// test-results metrics`) and fails a test over its budget. A simulator's
/// numbers are not the budget: it renders on the Mac's GPU, with the
/// Mac's CPU. Only the phone's numbers count.
@MainActor
final class Performance: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }

    /// Five measured runs (XCTest adds one first run it does not count).
    private func options(manual: Bool = true) -> XCTMeasureOptions {
        let options = XCTMeasureOptions()
        options.iterationCount = Int(env["PERF_ITERATIONS"] ?? "") ?? 5
        if manual { options.invocationOptions = [.manuallyStop] }
        return options
    }

    override func setUp() {
        continueAfterFailure = false
        MockServer.reset()
        XCUIDevice.shared.orientation = .portrait
    }

    private func newApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = MockServer.environment()
        return app
    }

    private func launch() -> XCUIApplication {
        let app = newApp()
        app.launch()
        allowLocalNetwork()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "Home did not load")
        sleep(3) // the pictures, so a scroll draws what a user sees
        return app
    }

    /// A phone asks for the local network at the first start of the app
    /// (the mock is on the Mac); the simulator does not ask.
    private func allowLocalNetwork() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let allow = springboard.buttons["Allow"]
        if allow.waitForExistence(timeout: 3) { allow.tap() }
    }

    private var cards: XCUIElementQuery {
        XCUIApplication().descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'card.'"))
    }

    /// A fast flick from `from` to `to` (normalised offsets in the window),
    /// as a thumb gives it.
    private func flick(_ app: XCUIApplication, from: CGVector, to: CGVector) {
        let start = app.coordinate(withNormalizedOffset: from)
        start.press(forDuration: 0.01, thenDragTo: app.coordinate(withNormalizedOffset: to), withVelocity: .fast,
                    thenHoldForDuration: 0)
    }

    /// Flicks up `count` times inside the measure, and back to the top
    /// outside it, so each run scrolls the same content.
    private func measureVerticalScroll(_ app: XCUIApplication, count: Int = 3) {
        measure(metrics: [XCTOSSignpostMetric.scrollingAndDecelerationMetric], options: options()) {
            for _ in 0..<count { app.swipeUp(velocity: .fast) }
            stopMeasuring()
            for _ in 0..<(count + 2) { app.swipeDown(velocity: .fast) }
            sleep(1)
        }
    }

    // MARK: Scroll hitches (budget: hitch time ratio under 5 ms/s)

    /// Home from the hero down through its rows.
    func testScrollHome() {
        let app = launch()
        measureVerticalScroll(app)
    }

    /// One row of cards across, from its first card to its end.
    func testScrollRow() {
        let app = launch()
        app.swipeUp()
        sleep(1)
        let height = app.frame.height
        let card = cards.allElementsBoundByIndex.first {
            $0.isHittable && $0.frame.midY > height * 0.3 && $0.frame.midY < height * 0.7
        }
        guard let card else { return XCTFail("No row of cards in the middle of Home") }
        let y = card.frame.midY / height
        measure(metrics: [XCTOSSignpostMetric.scrollingAndDecelerationMetric], options: options()) {
            for _ in 0..<3 { flick(app, from: CGVector(dx: 0.9, dy: y), to: CGVector(dx: 0.1, dy: y)) }
            stopMeasuring()
            for _ in 0..<5 { flick(app, from: CGVector(dx: 0.1, dy: y), to: CGVector(dx: 0.9, dy: y)) }
            sleep(1)
        }
    }

    /// The grid of the Movies library.
    func testScrollLibrary() {
        let app = launch()
        if !app.buttons["Library"].exists { app.buttons["Home"].firstMatch.tap() }
        XCTAssertTrue(app.buttons["Library"].firstMatch.waitForExistence(timeout: 5), "No Library tab")
        app.buttons["Library"].firstMatch.tap()
        let movies = app.cells.containing(NSPredicate(format: "label CONTAINS 'Movies'")).firstMatch
        XCTAssertTrue(app.cells.firstMatch.waitForExistence(timeout: 10), "The Library tab lists no library")
        (movies.exists ? movies : app.cells.firstMatch).tap()
        XCTAssertTrue(cards.firstMatch.waitForExistence(timeout: 10), "The library page has no card")
        sleep(3)
        measureVerticalScroll(app, count: 4)
    }

    /// The results of a search with many matches ("the": every extra film).
    func testScrollSearch() {
        let app = launch()
        app.buttons["Search"].firstMatch.tap()
        XCTAssertTrue(app.searchFields.firstMatch.waitForExistence(timeout: 5), "No search field")
        sleep(1)
        app.typeText("the")
        let found = cards.matching(NSPredicate(format: "label CONTAINS[c] 'the'")).firstMatch
        XCTAssertTrue(found.waitForExistence(timeout: 15), "Search found no title with \"the\"")
        sleep(3)
        // The keyboard goes down at the first scroll; that one is not measured.
        app.swipeUp()
        app.swipeDown()
        sleep(1)
        measureVerticalScroll(app)
    }

    // MARK: Launch (budget: first frame within 400 ms)

    /// A cold start to the first frame. The app signs in to the mock at
    /// each start (BLOOM_SERVER); the metric ends at the first frame.
    func testColdLaunch() {
        // The first start asks for the local network on a phone; answer it
        // before the measure.
        _ = launch()
        measure(metrics: [XCTApplicationLaunchMetric()], options: options(manual: false)) {
            newApp().launch()
        }
    }

    // MARK: Pages (watched, no gate)

    /// From a tap on a card to the detail page's Play button. The time
    /// holds XCUITest's own queries (about 0.1 to 0.3 s), so it is a trend,
    /// not a budget.
    func testOpenDetail() {
        let app = launch()
        let metrics: [any XCTMetric] = [XCTClockMetric(), XCTMemoryMetric(application: app), XCTCPUMetric(application: app)]
        measure(metrics: metrics, options: options()) {
            let card = cards.allElementsBoundByIndex.first { $0.isHittable } ?? cards.firstMatch
            card.tap()
            XCTAssertTrue(app.buttons["detail.play"].firstMatch.waitForExistence(timeout: 15), "The detail page did not load")
            stopMeasuring()
            sleep(1)
            app.navigationBars.buttons.element(boundBy: 0).tap()
            XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 10), "Back did not return to Home")
            sleep(1)
        }
    }

    /// From Play on a detail page to the video on screen, then the player
    /// closed (paused first: a playing video keeps the app busy, and
    /// XCUITest waits for an idle app before a tap, spec §18).
    func testOpenPlayer() {
        let app = launch()
        cards.firstMatch.tap()
        let play = app.buttons["detail.play"]
        XCTAssertTrue(play.waitForExistence(timeout: 15), "No Play button on the detail page")
        sleep(2)
        let metrics: [any XCTMetric] = [XCTClockMetric(), XCTMemoryMetric(application: app), XCTCPUMetric(application: app)]
        measure(metrics: metrics, options: options()) {
            let plays = app.buttons.matching(identifier: "detail.play").allElementsBoundByIndex
            (plays.first(where: \.isHittable) ?? play).tap()
            XCTAssertTrue(app.descendants(matching: .any)["Video"].waitForExistence(timeout: 20), "The player did not open")
            stopMeasuring()
            let pause = app.buttons["player.pause"]
            if pause.waitForExistence(timeout: 10) { pause.tap() }
            sleep(1)
            app.buttons["player.close"].tap()
            XCTAssertTrue(play.waitForExistence(timeout: 10), "Close did not return to the detail page")
            sleep(2)
        }
    }
}
