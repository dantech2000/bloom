import XCTest

/// The film page and the player after the tester's findings of
/// 2026-11-18: a pull at the top keeps the page, a scrolled page has a
/// clean top, Play on and SyncPlay on the page and in the player, the
/// File section, friendly track names, and the words under the player's
/// tools. Against dev/mock-jellyfin, muted, no reports, the fake
/// Chromecast only.
@MainActor
final class DetailShots: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }
    private var dir: String { env["SHOT_DIR"] ?? "/tmp/bloom-shots" }

    override func setUp() async throws {
        continueAfterFailure = true
        MockServer.reset()
        XCUIDevice.shared.orientation = .portrait
    }

    override func tearDown() async throws {
        XCUIDevice.shared.orientation = .portrait
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? png.write(to: URL(fileURLWithPath: "\(dir)/\(name).png"))
    }

    private func launch(_ extra: [String: String] = [:]) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = MockServer.environment()
        app.launchEnvironment.merge(extra) { $1 }
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "Home did not load")
        sleep(2)
        return app
    }

    /// The page of a film found by Search (a film of the mock with a file).
    private func openFilm(_ app: XCUIApplication, _ title: String = "Paper Lanterns") {
        app.buttons["Search"].firstMatch.tap()
        XCTAssertTrue(app.searchFields.firstMatch.waitForExistence(timeout: 5), "No search field")
        sleep(1)
        app.typeText(title)
        let card = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'card.' AND label CONTAINS[c] %@", title)).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 15), "Search did not find \(title)")
        card.tap()
        XCTAssertTrue(app.buttons["detail.play"].waitForExistence(timeout: 15), "The detail page did not load")
        sleep(2)
    }

    private func tapPlay(_ app: XCUIApplication) {
        let plays = app.buttons.matching(identifier: "detail.play").allElementsBoundByIndex
        (plays.first(where: \.isHittable) ?? app.buttons["detail.play"]).tap()
    }

    private func becomes(_ element: XCUIElement, _ format: String, timeout: TimeInterval = 15) -> Bool {
        let predicate = NSPredicate(format: format)
        return XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: predicate, object: element)], timeout: timeout) == .completed
    }

    /// A pull down at the top of a film's page, as a person scrolls back
    /// up: the page stays (it closed to Home three times for the tester).
    func testPullDownKeepsPage() {
        let app = launch()
        openFilm(app)
        let play = app.buttons["detail.play"].firstMatch
        // Slow and fast pulls from the middle of the page.
        let middle = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35))
        middle.press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)))
        sleep(2)
        XCTAssertTrue(play.exists && play.isHittable, "A slow pull closed the page")
        app.swipeDown(velocity: .fast)
        sleep(2)
        XCTAssertTrue(play.exists && play.isHittable, "A fast pull closed the page")
        // Down the page and back up with a few flicks.
        app.swipeUp()
        sleep(1)
        app.swipeDown()
        app.swipeDown()
        app.swipeDown()
        sleep(2)
        XCTAssertTrue(play.exists && play.isHittable, "A scroll back to the top closed the page")
        shot("d01-after-pulls")
        // The back button still goes back.
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(becomes(play, "exists == false", timeout: 10), "Back did not leave the page")
    }

    /// The top of a scrolled page: the back button and the page's tools
    /// over a clean bar, nothing of the page under the clock.
    func testScrolledTop() {
        let app = launch()
        openFilm(app)
        shot("d02-detail-top")
        // A short scroll: Play comes up to the bar.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.8))
            .press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)))
        sleep(2)
        shot("d03a-detail-scrolled-short")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.4))
            .press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.7)))
        sleep(2)
        let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.8))
        start.press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25)))
        sleep(2)
        shot("d03-detail-scrolled")
        app.swipeUp()
        sleep(2)
        shot("d04-detail-scrolled-more")
    }

    /// The File section, "Ends at", and the friendly track names in the
    /// pickers.
    func testFileAndTracks() {
        let app = launch()
        openFilm(app)
        XCTAssertTrue(app.descendants(matching: .any)["detail.ends-at"].exists, "No \"Ends at\" near Play")
        let subtitles = app.buttons["detail.subtitles"]
        XCTAssertTrue(subtitles.waitForExistence(timeout: 10), "No subtitle choice")
        subtitles.tap()
        sleep(1)
        shot("d05-detail-subtitle-menu")
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'English (Forced, image)'")).firstMatch.exists,
                      "The image track has no friendly name")
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Spanish (SDH)'")).firstMatch.exists,
                      "The SDH track has no friendly name")
        XCTAssertFalse(app.buttons.matching(NSPredicate(format: "label CONTAINS 'PGSSUB'")).firstMatch.exists, "A codec as a name")
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Spanish (SDH)'")).firstMatch.tap()
        XCTAssertTrue(becomes(subtitles, "label CONTAINS 'Spanish'", timeout: 10), "The choice does not show: \(subtitles.label)")
        app.buttons["detail.audio"].tap()
        sleep(1)
        shot("d06-detail-audio-menu")
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'French'")).firstMatch.tap()
        sleep(1)
        app.swipeUp()
        sleep(1)
        let file = app.descendants(matching: .any)["detail.file"]
        XCTAssertTrue(file.waitForExistence(timeout: 5), "No File section")
        XCTAssertTrue(app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH 'Video:' AND label CONTAINS '4K' AND label CONTAINS 'Dolby Vision'")).firstMatch.exists,
                      "The File section has no resolution and range")
        shot("d07-detail-file")
    }

    /// Play on and SyncPlay from the page's top bar open their sheets.
    func testCastAndSyncFromPage() {
        let app = launch()
        openFilm(app)
        let playOn = app.buttons["detail.play-on"].firstMatch
        XCTAssertTrue(playOn.waitForExistence(timeout: 5), "No Play on button on the page")
        playOn.tap()
        XCTAssertTrue(app.navigationBars["Play on"].waitForExistence(timeout: 5), "The Play on sheet did not open")
        sleep(2)
        shot("d08-page-play-on")
        app.navigationBars["Play on"].buttons["Done"].tap()
        XCTAssertTrue(becomes(app.navigationBars["Play on"], "exists == false", timeout: 5))
        let sync = app.buttons["detail.syncplay"].firstMatch
        if sync.waitForExistence(timeout: 3) {
            sync.tap()
            sleep(2)
            shot("d09-page-syncplay")
        } else {
            print("SHOTS: SyncPlay is not allowed for the mock's user")
        }
    }

    /// After the first plays the tools are icons alone, with their names
    /// for VoiceOver.
    func testPlayerToolsPlain() {
        let app = launch(["BLOOM_TOOL_WORDS": "0"])
        openFilm(app)
        tapPlay(app)
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15), "The player did not open")
        sleep(3)
        pause.tap()
        sleep(1)
        XCTAssertEqual(app.buttons["player.play-on"].label, "Play on")
        XCTAssertEqual(app.buttons["player.settings"].label, "Settings")
        shot("d14-player-tools-plain")
    }

    /// The player: the words under the tools, the friendly track names,
    /// and Play on from the bar handing the film to the fake Chromecast.
    func testPlayerToolsAndCast() {
        let app = launch(["BLOOM_TOOL_WORDS": "1"])
        openFilm(app)
        tapPlay(app)
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15), "The player did not open")
        sleep(3)
        pause.tap()
        sleep(1)
        shot("d10-player-tool-words")
        app.buttons["player.subtitles"].tap()
        sleep(1)
        shot("d11-player-subtitle-menu")
        XCTAssertFalse(app.buttons.matching(NSPredicate(format: "label CONTAINS 'SRT ·' OR label CONTAINS 'SUBRIP'")).firstMatch.exists,
                       "A codec in a track's name")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.2, dy: 0.2)).tap()
        sleep(1)
        if !app.buttons["player.play-on"].isHittable {
            app.descendants(matching: .any)["Video"].coordinate(withNormalizedOffset: CGVector(dx: 0.2, dy: 0.5)).tap()
            sleep(1)
        }
        app.buttons["player.play-on"].tap()
        XCTAssertTrue(app.navigationBars["Play on"].waitForExistence(timeout: 5), "The Play on sheet did not open over the player")
        sleep(2)
        shot("d12-player-play-on")
        let device = app.buttons.matching(NSPredicate(format: "label == 'Mock TV, Mock'")).firstMatch
        XCTAssertTrue(device.waitForExistence(timeout: 10), "The fake Chromecast is not listed")
        device.tap()
        let playing = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS 'Playing on Mock TV'")).firstMatch
        XCTAssertTrue(playing.waitForExistence(timeout: 20), "The film did not go to the fake device")
        XCTAssertTrue(becomes(app.buttons["player.close"], "exists == false", timeout: 10), "The player stayed open")
        shot("d13-handed-to-cast")
    }
}
