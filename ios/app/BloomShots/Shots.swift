import XCTest

/// Walks the app against dev/mock-jellyfin and writes screenshots to
/// SHOT_DIR. The mock serves BLOOM_MOCK_CLIP for every stream, so a play
/// shows a picture. The app runs muted and sends no playback report.
@MainActor
final class Shots: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }
    /// The mock's host and port, as dev/ios-next started it (its own port
    /// per run under BLOOM_MOCK_PORT).
    private var mockHost: String { MockServer.host }
    private var dir: String { env["SHOT_DIR"] ?? "/tmp/bloom-shots" }

    override func setUp() async throws {
        continueAfterFailure = true
        // Each test from the mock's first state: a test before may have
        // saved settings, made a playlist, hidden a title or rotated.
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

    private func launch(_ extra: [String: String] = [:], controlsStay: Bool = true) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = MockServer.environment(controlsStay: controlsStay)
        app.launchEnvironment.merge(extra) { $1 }
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "Home did not load")
        sleep(3) // the pictures
        return app
    }

    private func firstCard(_ app: XCUIApplication) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'card.'")).firstMatch
    }

    /// Play on a detail page that is opening: waits for the button and for
    /// the page to settle (the zoom, then the layout of its buttons: a tap
    /// during that misses), then taps the copy on screen (the action row
    /// is a ViewThatFits, and only one copy is hittable).
    private func tapPlay(_ app: XCUIApplication, timeout: TimeInterval = 15) {
        let play = app.buttons["detail.play"]
        XCTAssertTrue(play.waitForExistence(timeout: timeout), "No Play button on the detail page")
        sleep(2)
        let plays = app.buttons.matching(identifier: "detail.play").allElementsBoundByIndex
        (plays.first(where: \.isHittable) ?? play).tap()
    }

    /// Closes the Play on sheet, then any sheet under it (the account
    /// sheet); fails when one stays.
    private func closePlayOn(_ app: XCUIApplication) {
        for _ in 0..<3 where app.buttons["Done"].exists {
            let sheet = app.navigationBars["Play on"].buttons["Done"]
            (sheet.exists ? sheet : app.buttons["Done"].firstMatch).tap()
            _ = app.buttons["Done"].waitForNonExistence(timeout: 3)
        }
        XCTAssertFalse(app.buttons["Done"].exists, "A sheet stayed open over Home")
    }

    /// With a target chosen, its accessory sits over the tab bar and
    /// covers the lowest cards (a tap there opens the Play on sheet): the
    /// page goes up, and a card well above it.
    private func cardAboveAccessory(_ app: XCUIApplication) -> XCUIElement {
        app.swipeUp()
        sleep(1)
        let cards = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'card.'"))
        let card = cards.allElementsBoundByIndex.first { $0.isHittable && $0.frame.maxY < app.frame.height * 0.75 && $0.frame.minX >= 0 }
        return card ?? firstCard(app)
    }

    /// The first card's detail page, then Play.
    private func playFirstCard(_ app: XCUIApplication) {
        firstCard(app).tap()
        tapPlay(app)
    }

    /// Waits until `element` matches `format`; false at the deadline.
    private func becomes(_ element: XCUIElement, _ format: String, _ args: CVarArg..., timeout: TimeInterval = 15) -> Bool {
        let predicate = NSPredicate(format: format, argumentArray: args)
        return XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: predicate, object: element)], timeout: timeout) == .completed
    }

    func testBrowse() {
        let app = launch()
        shot("01-home")
        app.swipeUp()
        sleep(1)
        shot("02-home-rows")
        app.swipeDown()

        XCTAssertTrue(firstCard(app).exists, "Home has no card")
        firstCard(app).tap()
        XCTAssertTrue(app.buttons["detail.play"].waitForExistence(timeout: 15), "The detail page did not load")
        sleep(2)
        shot("03-detail")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 10), "Back did not return to Home")
        // The tab bar can be in its shrunken form (current tab and Search
        // only); a tap on the current tab opens it again.
        if !app.buttons["Library"].exists { app.buttons["Home"].firstMatch.tap() }
        XCTAssertTrue(app.buttons["Library"].firstMatch.waitForExistence(timeout: 5), "No Library tab")
        app.buttons["Library"].firstMatch.tap()
        XCTAssertTrue(app.cells.firstMatch.waitForExistence(timeout: 10), "The Library tab lists no library")
        shot("04-library")
        app.cells.firstMatch.tap()
        XCTAssertTrue(firstCard(app).waitForExistence(timeout: 10), "The library page has no card")
        sleep(2)
        shot("05-library-page")

        app.buttons["Search"].firstMatch.tap()
        XCTAssertTrue(app.searchFields.firstMatch.waitForExistence(timeout: 5), "No search field")
        sleep(1)
        app.typeText("the")
        let found = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'card.' AND label CONTAINS[c] 'the'")).firstMatch
        XCTAssertTrue(found.waitForExistence(timeout: 15), "Search found no title with \"the\"")
        shot("06-search")
    }

    func testPlay() {
        let app = launch()
        playFirstCard(app)
        XCTAssertTrue(app.descendants(matching: .any)["Video"].waitForExistence(timeout: 15), "The player did not open")
        // The mock's clip has an intro over its first 12 s.
        let skip = app.buttons["player.skip"]
        XCTAssertTrue(skip.waitForExistence(timeout: 20), "No Skip Intro button")
        shot("07-player-skip-intro")
        // Paused from here: XCUITest's taps wait for an idle app, and a
        // playing video never is (spec §18).
        app.buttons["player.pause"].tap()
        sleep(1)
        // The subtitle file from the server, fetched and shown.
        // A tap toggles the controls: show them only if they are hidden.
        let subtitles = app.buttons["player.subtitles"]
        XCTAssertTrue(subtitles.waitForExistence(timeout: 5))
        subtitles.tap()
        sleep(1)
        shot("08-player-subtitle-menu")
        for line in app.debugDescription.split(separator: "\n") where line.contains("SRT") || line.contains("Off") {
            print("SHOTS-TREE: \(line)")
        }
        app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'English'")).firstMatch.tap()
        sleep(2)
        shot("09-player-subtitle-shown")
        // Skip the intro: the position passes 12 s.
        if skip.exists { skip.tap() }
        sleep(2)
        sleep(1)
        shot("10-player-after-skip")
        app.buttons["player.settings"].tap()
        sleep(1)
        shot("10b-player-quality-menu")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.2, dy: 0.2)).tap()
        sleep(1)
        app.buttons["player.close"].tap()
        sleep(2)
        shot("11-after-close")
    }

    /// An episode to its next one, three ways: the Up next card (in the
    /// last 30 s), the end of the episode (autoplay), and the Next button.
    /// An episode's page links to its season and its show, and shows the
    /// other episodes of the season.
    func testEpisodeLinks() {
        let app = launch()
        let episode = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'card.' AND label CONTAINS 'Chapter'")).firstMatch
        for _ in 0..<4 where !episode.exists {
            app.swipeUp()
            sleep(1)
        }
        XCTAssertTrue(episode.waitForExistence(timeout: 10), "No episode card on Home")
        episode.tap()
        let season = app.buttons["detail.season-link"]
        XCTAssertTrue(season.waitForExistence(timeout: 15), "No link to the season")
        XCTAssertTrue(app.buttons["detail.series-link"].exists, "No link to the show")
        sleep(2)
        shot("16a-episode-links")
        for _ in 0..<3 where !app.descendants(matching: .any)["detail.season-episodes"].exists {
            app.swipeUp()
            sleep(1)
        }
        XCTAssertTrue(app.descendants(matching: .any)["detail.season-episodes"].waitForExistence(timeout: 5), "No row of the season's episodes")
        shot("16b-episode-season-row")
        for _ in 0..<3 where !season.isHittable {
            app.swipeDown()
            sleep(1)
        }
        season.tap()
        // The season's page lists its episodes and links to the show.
        let show = app.buttons["detail.series-link"]
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label CONTAINS 'Chapter'")).firstMatch.waitForExistence(timeout: 15), "The season page shows no episodes")
        XCTAssertFalse(app.buttons["detail.season-link"].exists, "A season page links to itself")
        // A season plays its next episode and downloads as a whole, as the show's page does.
        let play = app.buttons["detail.play"]
        XCTAssertTrue(play.waitForExistence(timeout: 5), "A season page has no Play")
        XCTAssertTrue(play.label.contains("S1:E"), "The season's Play says \(play.label)")
        XCTAssertTrue(app.buttons["season.download"].exists, "A season page has no Download")
        sleep(2)
        shot("16c-season-page")
        XCTAssertTrue(show.waitForExistence(timeout: 5))
        show.tap()
        XCTAssertTrue(app.descendants(matching: .any)["season.download"].waitForExistence(timeout: 15)
            || app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Season'")).firstMatch.waitForExistence(timeout: 5),
            "The show's page did not open")
        shot("16d-show-page")
    }

    func testUpNext() {
        // The controls stay: the test taps the bar while a video plays, and
        // hidden controls take no tap (a tap went to Previous once).
        let app = launch(["BLOOM_CONTROLS_STAY": "1"])
        // An episode of Next Up: its card names the chapter.
        let episode = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'card.' AND label CONTAINS 'Chapter'")).firstMatch
        for _ in 0..<4 where !episode.exists {
            app.swipeUp()
            sleep(1)
        }
        XCTAssertTrue(episode.waitForExistence(timeout: 10), "No episode card on Home")
        episode.tap()
        tapPlay(app)
        XCTAssertTrue(app.descendants(matching: .any)["Video"].waitForExistence(timeout: 15), "The player did not open")
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15))
        sleep(3)
        pause.tap()
        sleep(1)
        let title = app.staticTexts["player.title"]
        XCTAssertTrue(app.buttons["player.next"].exists, "The episode has no Next button")
        let first = title.label

        // 1. The Up next card near the end, and Play Now.
        app.sliders["player.position"].adjust(toNormalizedSliderPosition: 0.9)
        let playNow = app.buttons["player.upnext.play"]
        XCTAssertTrue(playNow.waitForExistence(timeout: 10), "No Up next card in the last 30 s")
        shot("12a-player-up-next")
        playNow.tap()
        let changed = { (from: String) in NSPredicate(format: "label != %@", from) }
        wait(for: [expectation(for: changed(first), evaluatedWith: title)], timeout: 20)
        let second = title.label
        print("SHOTS: Up next went from \(first) to \(second)")

        // 2. The end of the episode plays the next one (autoplay).
        XCTAssertTrue(pause.waitForExistence(timeout: 15))
        sleep(3)
        if pause.label == "Pause" { pause.tap() }
        sleep(1)
        app.sliders["player.position"].adjust(toNormalizedSliderPosition: 0.97)
        sleep(1)
        pause.tap()
        wait(for: [expectation(for: changed(second), evaluatedWith: title)], timeout: 30)
        let third = title.label
        print("SHOTS: the end went from \(second) to \(third)")
        shot("12b-player-autoplay")

        // 3. The Next button.
        sleep(3)
        if pause.label == "Pause" { pause.tap() }
        sleep(1)
        app.buttons["player.next"].tap()
        wait(for: [expectation(for: changed(third), evaluatedWith: title)], timeout: 20)
        print("SHOTS: Next went from \(third) to \(title.label)")
        if pause.label == "Pause" { pause.tap() }
        sleep(1)
        app.buttons["player.close"].tap()
    }

    /// The gear's speed and info card, and the pause screen.
    func testPlayerExtras() {
        let app = launch()
        playFirstCard(app)
        let settings = app.buttons["player.settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 15), "The player did not open")
        sleep(3)
        // Paused, the app goes idle: XCUITest waits for idle before each
        // tap, and a playing video never is (60 s per tap, then a stale one).
        app.buttons["player.pause"].tap()
        sleep(1)
        settings.tap()
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Speed'")).firstMatch.tap()
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH '1.5'")).firstMatch.tap()
        sleep(1)
        settings.tap()
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Speed · 1.5×'")).firstMatch.waitForExistence(timeout: 3),
                      "The speed did not change")
        app.buttons["player.info"].firstMatch.tap()
        let card = app.descendants(matching: .any)["player.info-card"]
        XCTAssertTrue(card.waitForExistence(timeout: 5), "No playback info card")
        sleep(2)
        XCTAssertTrue(app.staticTexts["Video"].exists, "The info card has no video line")
        shot("27-player-info")
        app.buttons.matching(NSPredicate(format: "label == 'Close' AND identifier != 'player.close'")).firstMatch.tap()
        // The controls go (a tap on the video, beside the play button in the
        // middle), and after 5 s the pause screen comes.
        app.descendants(matching: .any)["Video"].coordinate(withNormalizedOffset: CGVector(dx: 0.2, dy: 0.5)).tap()
        XCTAssertTrue(app.descendants(matching: .any)["player.pause-screen"].waitForExistence(timeout: 10), "No pause screen")
        shot("28-pause-screen")
    }

    /// Tracks chosen on the detail page: the subtitle file goes into the
    /// player at the start (it no longer waits under "From the server").
    func testTracks() {
        let app = launch()
        firstCard(app).tap()
        let subtitles = app.buttons["detail.subtitles"]
        XCTAssertTrue(subtitles.waitForExistence(timeout: 15), "No subtitle choice on the detail page")
        XCTAssertTrue(app.buttons["detail.audio"].exists, "No audio choice on the detail page")
        sleep(2)
        subtitles.tap()
        app.buttons["English · SRT"].firstMatch.tap()
        XCTAssertTrue(becomes(subtitles, "label CONTAINS 'English'", timeout: 10), "The choice does not show: \(subtitles.label)")
        shot("37-detail-tracks")
        tapPlay(app)
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15))
        sleep(4)
        pause.tap()
        app.buttons["player.subtitles"].tap()
        // The menu is open: its entry for the file is there.
        // The player names a track in words ("English"), its format as a detail.
        let entry = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'English'")).firstMatch
        XCTAssertTrue(entry.waitForExistence(timeout: 5), "The subtitle menu did not open")
        shot("38-player-tracks")
        XCTAssertFalse(app.staticTexts["From the server"].exists, "The chosen subtitle file was not loaded")
    }

    /// The small player over the pages: the full player goes small, the
    /// pages work under it, it comes back full screen, and it closes.
    func testMiniPlayer() {
        let app = launch()
        playFirstCard(app)
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15), "The player did not open")
        sleep(3)
        pause.tap()
        app.buttons["player.minimize"].tap()
        let mini = app.descendants(matching: .any)["mini.player"]
        XCTAssertTrue(mini.waitForExistence(timeout: 5), "No small player")
        XCTAssertFalse(app.buttons["player.close"].exists, "The full player is still open")
        // The pages work under it.
        app.buttons["Library"].firstMatch.tap()
        sleep(1)
        shot("39-mini-player")
        app.buttons["mini.expand"].tap()
        XCTAssertTrue(app.buttons["player.close"].waitForExistence(timeout: 5), "The full player did not come back")
        app.buttons["player.minimize"].tap()
        XCTAssertTrue(app.buttons["mini.close"].waitForExistence(timeout: 5))
        app.buttons["mini.close"].tap()
        XCTAssertTrue(mini.waitForNonExistence(timeout: 10), "The small player did not close")
    }

    /// A menu of the bar stays open past the controls' timer and while the
    /// user scrolls in it (2026-11-14: the timer hid the controls and took
    /// the open menu with them). The controls may hide while paused here
    /// (BLOOM_HIDE_WHEN_PAUSED): a playing video never lets XCUITest idle.
    func testMenusStayOpen() {
        let app = launch(["BLOOM_HIDE_WHEN_PAUSED": "1"], controlsStay: false)
        playFirstCard(app)
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15))
        pause.tap()
        // The controls hide 2.5 s after the pause; a tap on the video brings them.
        sleep(4)
        app.descendants(matching: .any)["Video"].coordinate(withNormalizedOffset: CGVector(dx: 0.2, dy: 0.5)).tap()
        let settings = app.buttons["player.settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 5))
        settings.tap()
        let info = app.buttons["Playback info"]
        XCTAssertTrue(info.waitForExistence(timeout: 5), "The settings menu did not open")
        // Past the timer (2.5 s; a window many times that: the menu must
        // not close in it), and a scroll in the menu: a swipe from its
        // Quality heading, which no tap chooses.
        sleep(5)
        XCTAssertTrue(info.exists, "The menu closed when the controls' timer ran out")
        let heading = app.staticTexts["Quality"].firstMatch
        if heading.exists {
            heading.swipeUp(velocity: .fast)
        } else {
            print("SHOTS: no Quality heading to scroll the menu by")
        }
        sleep(3)
        XCTAssertTrue(info.exists, "The menu closed while it scrolled")
        shot("40-menu-stays-open")
        // A choice lets the controls hide again.
        info.tap()
        XCTAssertTrue(becomes(settings, "hittable == false", timeout: 15), "The controls did not hide after a choice")
    }

    /// The connect screen as a person uses it: the address, the username
    /// and the password typed on the keyboard (no BLOOM_SERVER shortcut).
    func testConnect() {
        let app = XCUIApplication()
        app.launchEnvironment = ["BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CHROMECAST_MOCK": "1"]
        app.launchArguments = ["-accounts", "", "-activeUser", ""]
        app.launch()
        let address = app.textFields["connect.address"]
        XCTAssertTrue(address.waitForExistence(timeout: 15), "No connect screen")
        shot("11-connect")
        address.tap()
        address.typeText(mockHost + "\n")
        let username = app.textFields["connect.username"]
        XCTAssertTrue(username.waitForExistence(timeout: 15), "No sign-in fields")
        shot("12-sign-in")
        username.tap()
        username.typeText("tester")
        let password = app.secureTextFields["connect.password"]
        password.tap()
        password.typeText("\n")
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "Home did not load after the sign-in")
        sleep(2)
        shot("13-signed-in")
    }

    /// A download, then the Downloads tab, then a play from this device.
    func testDownload() {
        let app = launch()
        let card = firstCard(app)
        let id = card.identifier.replacingOccurrences(of: "card.", with: "")
        card.tap()
        let download = app.buttons["download.\(id)"]
        if !download.waitForExistence(timeout: 15) {
            shot("13x-no-download")
        }
        XCTAssertTrue(download.exists, "No download button")
        sleep(2)
        download.tap()
        let done = app.descendants(matching: .any).matching(NSPredicate(format: "label == 'Downloaded'")).firstMatch
        XCTAssertTrue(done.waitForExistence(timeout: 60), "The download did not finish")
        shot("14-downloaded")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        sleep(1)
        if !app.buttons["Downloads"].exists { app.buttons["Home"].firstMatch.tap() }
        app.buttons["Downloads"].firstMatch.tap()
        sleep(1)
        shot("15-downloads-tab")
        // The row, by its title (the first cell can be the section's header).
        app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'Paper Lanterns'")).firstMatch.tap()
        XCTAssertTrue(app.buttons["player.close"].waitForExistence(timeout: 20), "The download did not play")
        sleep(3)
        shot("16-playing-download")
        app.buttons["player.close"].tap()
        sleep(1)
        // Leave no download behind for the next run.
        app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'Paper Lanterns'")).firstMatch.swipeLeft()
        app.buttons["Remove"].firstMatch.tap()
    }

    /// Quick Connect: the code, then the approval (the mock approves at its
    /// second poll), then Home. Needs the mock with BLOOM_MOCK_QUICKCONNECT=1.
    func testQuickConnect() throws {
        guard env["SHOT_QUICKCONNECT"] == "1" else { throw XCTSkip("needs BLOOM_MOCK_QUICKCONNECT on the mock") }
        let app = XCUIApplication()
        app.launchEnvironment = ["BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CHROMECAST_MOCK": "1"]
        app.launchArguments = ["-accounts", "", "-activeUser", ""]
        app.launch()
        let address = app.textFields["connect.address"]
        XCTAssertTrue(address.waitForExistence(timeout: 15))
        address.tap()
        address.typeText(mockHost + "\n")
        let quick = app.buttons["Use Quick Connect"]
        XCTAssertTrue(quick.waitForExistence(timeout: 15), "No Quick Connect button")
        quick.tap()
        sleep(1)
        shot("17-quick-connect-code")
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "Home did not load after the approval")
    }

    /// Play on: choose the mock's Jellyfin device, then Play sends the item
    /// to it instead of opening the player. Needs BLOOM_MOCK_SESSION=1.
    func testPlayOn() throws {
        guard env["SHOT_SESSION"] == "1" else { throw XCTSkip("needs BLOOM_MOCK_SESSION on the mock") }
        let app = launch()
        app.buttons["account"].firstMatch.tap()
        sleep(1)
        app.buttons["Play on"].firstMatch.tap()
        let device = app.buttons.matching(NSPredicate(format: "label CONTAINS 'Mock TV'")).firstMatch
        XCTAssertTrue(device.waitForExistence(timeout: 10), "The mock device is not listed")
        device.tap()
        sleep(1)
        shot("18-play-on")
        closePlayOn(app)
        let card = cardAboveAccessory(app)
        let item = String(card.identifier.dropFirst("card.".count))
        card.tap()
        tapPlay(app)
        // The mock took the command for its session, with this item.
        let sent = MockServer.wait(timeout: 20) {
            MockServer.remoteCommands().contains { command in
                command["Session"] as? String == "mock-session" && (command["ItemIds"] as? [String] ?? []).contains(item)
                    && command["PlayCommand"] as? String == "PlayNow"
            }
        }
        XCTAssertTrue(sent, "No PlayNow of \(item) reached the mock's session: \(MockServer.remoteCommands())")
        XCTAssertFalse(app.buttons["player.close"].exists, "The player opened on this device")
        shot("19-played-on-remote")
    }

    /// The settings on the server: the subtitle mode and a home row change,
    /// and the mock keeps what the saves sent (its log has them). Before
    /// them, the device's rows: the cellular limit of Playback and the
    /// switch of the Jellyfin Enhanced subtitle style.
    func testSettings() {
        let app = launch()
        app.buttons["account"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Settings"].firstMatch.waitForExistence(timeout: 5), "No account sheet")
        sleep(1)
        shot("24x-account-sheet")
        app.buttons["Settings"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Playback"].firstMatch.waitForExistence(timeout: 5), "No Settings sheet")
        sleep(1)
        shot("24y-settings")
        app.buttons["Playback"].firstMatch.tap()
        let cellular = app.descendants(matching: .any)["settings.cellularQuality"].firstMatch
        XCTAssertTrue(cellular.waitForExistence(timeout: 10), "No cellular limit in Settings › Playback")
        XCTAssertTrue(cellular.label.contains("cellular"), "The cellular row reads \(cellular.label)")
        app.navigationBars.buttons.firstMatch.tap()
        app.collectionViews.buttons["Jellyfin Enhanced"].firstMatch.tap()
        let style = app.switches["enhanced.subtitle_style"].firstMatch
        XCTAssertTrue(style.waitForExistence(timeout: 10), "No switch for the Enhanced subtitle style")
        shot("25a-settings-enhanced")
        app.navigationBars.buttons.firstMatch.tap()
        app.buttons["Subtitles"].firstMatch.tap()
        let mode = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Subtitle mode'")).firstMatch
        XCTAssertTrue(mode.waitForExistence(timeout: 10), "The subtitle settings did not load")
        shot("25-settings-subtitles")
        mode.tap()
        app.buttons["Always"].firstMatch.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'whatever the audio language is'")).firstMatch
            .waitForExistence(timeout: 10), "The mode's text did not follow the choice")
        app.navigationBars.buttons.firstMatch.tap()
        // The settings list's Home, not the tab bar's under the sheet.
        app.collectionViews.buttons["Home"].firstMatch.tap()
        // The rows in order; Add a row offers only a kind not on the list.
        XCTAssertTrue(app.descendants(matching: .any)["home.row.nextup"].waitForExistence(timeout: 5), "No Next Up row")
        // Add a row lists only the kinds not on the list.
        XCTAssertFalse(app.buttons["home.add.nextup"].exists, "Add a row offers a row already on the list")
        let add = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'home.add.'")).firstMatch
        for _ in 0..<2 where !add.exists { app.swipeUp() }
        XCTAssertTrue(add.waitForExistence(timeout: 5), "Add a row offers nothing")
        let key = String(add.identifier.dropFirst("home.add.".count))
        add.tap()
        XCTAssertTrue(app.descendants(matching: .any)["home.row.\(key)"].waitForExistence(timeout: 10), "The row was not added")
        XCTAssertFalse(app.buttons["home.add.\(key)"].exists, "The added kind is still offered")
        shot("26-settings-home")
    }

    /// Settings › Audio offers "Original language" (Jellyfin 12.0) after
    /// "Any language"; the choice reaches the server as "OriginalLanguage".
    func testOriginalLanguage() {
        let app = launch()
        app.buttons["account"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Settings"].firstMatch.waitForExistence(timeout: 5), "No account sheet")
        app.buttons["Settings"].firstMatch.tap()
        let audio = app.collectionViews.buttons["Audio"].firstMatch
        XCTAssertTrue(audio.waitForExistence(timeout: 5), "No Audio in Settings")
        audio.tap()
        let picker = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Audio language'")).firstMatch
        XCTAssertTrue(picker.waitForExistence(timeout: 10), "The audio settings did not load")
        picker.tap()
        let original = app.buttons["Original language"].firstMatch
        XCTAssertTrue(original.waitForExistence(timeout: 5), "No Original language among the audio languages")
        sleep(1)
        shot("25b-audio-language-menu")
        original.tap()
        let saved = MockServer.wait(timeout: 10) {
            let me = MockServer.json("/Users/Me") as? [String: Any]
            return (me?["Configuration"] as? [String: Any])?["AudioLanguagePreference"] as? String == "OriginalLanguage"
        }
        XCTAssertTrue(saved, "The server did not get OriginalLanguage")
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label CONTAINS 'Original language'")).firstMatch
            .waitForExistence(timeout: 5), "The picker does not show the choice")
        sleep(1)
        shot("25c-audio-original")
    }

    /// Playlists: a film goes into a new playlist from its page's More
    /// menu; the playlist page lists it and plays it.
    func testLists() {
        let app = launch()
        let card = firstCard(app)
        let title = card.label.components(separatedBy: ",").first ?? ""
        card.tap()
        let more = app.buttons["detail.more"]
        XCTAssertTrue(more.waitForExistence(timeout: 15))
        sleep(2)
        let mores = app.buttons.matching(identifier: "detail.more").allElementsBoundByIndex
        (mores.first(where: \.isHittable) ?? more).tap()
        app.buttons["Add to playlist…"].firstMatch.tap()
        let field = app.textFields["New playlist"]
        XCTAssertTrue(field.waitForExistence(timeout: 5), "No Add to playlist sheet")
        field.tap()
        field.typeText("Road trip")
        app.buttons["Create"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["toast"].waitForExistence(timeout: 5), "No toast after the add")
        app.buttons["Library"].firstMatch.tap()
        app.buttons["Playlists"].firstMatch.tap()
        let playlist = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Road trip'")).firstMatch
        XCTAssertTrue(playlist.waitForExistence(timeout: 10), "The new playlist is not listed")
        playlist.tap()
        let entry = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", title)).firstMatch
        XCTAssertTrue(entry.waitForExistence(timeout: 10), "The playlist does not hold \(title)")
        shot("29-playlist")
        app.buttons["Play"].firstMatch.tap()
        XCTAssertTrue(app.buttons["player.close"].waitForExistence(timeout: 15), "Play on the playlist did not open the player")
        app.buttons["player.pause"].tap()
        app.buttons["player.close"].tap()
    }

    /// SyncPlay against a real server (dev/jellyfin-local, SHOT_SYNCPLAY
    /// set to its address): a new group, an item played in it, the start
    /// the group gives, and a pause through the group. The mock has no
    /// SyncPlay.
    func testSyncPlay() throws {
        guard let server = env["SHOT_SYNCPLAY"], !server.isEmpty else { throw XCTSkip("needs SHOT_SYNCPLAY=<dev/jellyfin-local address>") }
        let app = XCUIApplication()
        app.launchEnvironment = [
            "BLOOM_SERVER": server, "BLOOM_USER": "bloom-test", "BLOOM_PASSWORD": "bloom-test",
            "BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CONTROLS_STAY": "1", "BLOOM_CHROMECAST_MOCK": "1",
        ]
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 60), "Home did not load")
        sleep(2)
        shot("30a-syncplay-home")
        app.buttons["account"].firstMatch.tap()
        app.buttons["SyncPlay"].firstMatch.tap()
        let new = app.buttons["syncplay.new"]
        XCTAssertTrue(new.waitForExistence(timeout: 10), "No New group button")
        new.tap()
        let leave = app.buttons["syncplay.leave"]
        XCTAssertTrue(leave.waitForExistence(timeout: 10), "The group was not made")
        shot("30-syncplay-group")
        app.buttons["Done"].firstMatch.tap()
        sleep(1)
        playFirstCard(app)
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 30), "The group's item did not open the player")
        // The group starts it once this device reports Ready.
        let playing = expectation(for: NSPredicate(format: "label == 'Pause'"), evaluatedWith: pause)
        wait(for: [playing], timeout: 30)
        shot("31-syncplay-playing")
        pause.tap()
        let paused = expectation(for: NSPredicate(format: "label == 'Play'"), evaluatedWith: pause)
        wait(for: [paused], timeout: 15)
        shot("32-syncplay-paused")
        app.buttons["player.close"].tap()
        sleep(1)
        // The account button is on Home.
        app.navigationBars.buttons.firstMatch.tap()
        sleep(1)
        app.buttons["account"].firstMatch.tap()
        app.buttons["SyncPlay"].firstMatch.tap()
        // With a queue the sheet is longer: Leave is at its end.
        sleep(2)
        app.swipeUp()
        XCTAssertTrue(leave.waitForExistence(timeout: 10))
        leave.tap()
        XCTAssertTrue(app.buttons["syncplay.new"].waitForExistence(timeout: 10), "Did not leave the group")
    }

    /// Jellyfin Enhanced on the mock (BLOOM_MOCK_ENHANCED): the Seerr rows
    /// of a detail page, Hide…, the Discover row of Search, and a bookmark
    /// in the player. No Seerr request is ever sent.
    func testEnhanced() {
        let app = launch()
        firstCard(app).tap()
        let recommended = app.staticTexts["Recommended on Seerr"]
        let more = app.buttons["detail.more"]
        XCTAssertTrue(more.waitForExistence(timeout: 15))
        sleep(2)
        app.swipeUp()
        XCTAssertTrue(recommended.waitForExistence(timeout: 10), "No Seerr row on the detail page")
        shot("33-seerr-rows")
        app.swipeDown()
        sleep(1)
        let mores = app.buttons.matching(identifier: "detail.more").allElementsBoundByIndex
        (mores.first(where: \.isHittable) ?? more).tap()
        app.buttons["Hide…"].firstMatch.tap()
        app.buttons["Everywhere"].firstMatch.tap()
        XCTAssertTrue(app.descendants(matching: .any)["toast"].waitForExistence(timeout: 5), "No toast after Hide")
        // Search: the Discover row.
        app.buttons["Search"].firstMatch.tap()
        let field = app.searchFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.typeText("film")
        XCTAssertTrue(app.staticTexts["Discover on Seerr"].waitForExistence(timeout: 10), "No Discover row")
        shot("34-seerr-discover")
    }

    /// The title index of Search: a text with a typing error finds the
    /// film from the index (the mock's server search matches the exact
    /// text alone, so its answer has no such card), and the match stays
    /// after the server answers.
    func testSearchIndex() {
        let app = launch()
        app.buttons["Search"].firstMatch.tap()
        let field = app.searchFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        // The mock holds the server's answers: a card that shows meanwhile
        // can only come from the local index.
        MockServer.holdSearches(true)
        defer { MockServer.holdSearches(false) }
        field.typeText("cartograper")
        let card = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'card.' AND label CONTAINS 'Cartographer'")).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 10), "No local match for a typing error")
        XCTAssertEqual(MockServer.count("SearchesAnswered"), 0, "The server answered a search while the mock held it")
        // The server's search goes out after the debounce; once the mock
        // answered it, the local match must still be there.
        XCTAssertTrue(MockServer.wait(timeout: 20) { MockServer.count("SearchesAsked") > 0 }, "The app asked the server no search")
        MockServer.holdSearches(false)
        XCTAssertTrue(MockServer.wait(timeout: 20) {
            let asked = MockServer.count("SearchesAsked")
            return asked > 0 && MockServer.count("SearchesAnswered") == asked
        }, "The mock did not answer the search")
        // The app shows the answer within a frame or two; a window many
        // times that, in which the card must not go.
        sleep(3)
        XCTAssertTrue(card.exists, "The server's answer took the local match away")
        shot("50-search-index")
    }

    /// Bookmarks in the player (BLOOM_MOCK_ENHANCED).
    func testBookmarks() {
        let app = launch()
        playFirstCard(app)
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15))
        sleep(3)
        pause.tap()
        app.buttons["player.bookmarks"].tap()
        let add = app.buttons["bookmark.add"]
        XCTAssertTrue(add.waitForExistence(timeout: 5), "No bookmarks sheet")
        add.tap()
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label CONTAINS 'Bookmark'")).firstMatch.waitForExistence(timeout: 5),
                      "The bookmark is not listed")
        shot("35-bookmarks")
    }

    /// The dashboard on dev/jellyfin-local (SHOT_SYNCPLAY, its address):
    /// read only, each section loads.
    func testDashboard() throws {
        guard let server = env["SHOT_SYNCPLAY"], !server.isEmpty else { throw XCTSkip("needs SHOT_SYNCPLAY=<dev/jellyfin-local address>") }
        let app = XCUIApplication()
        app.launchEnvironment = [
            "BLOOM_SERVER": server, "BLOOM_USER": "bloom-test", "BLOOM_PASSWORD": "bloom-test",
            "BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CHROMECAST_MOCK": "1", "BLOOM_TEST_SPLASH": "1",
        ]
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 60), "Home did not load")
        sleep(2)
        app.buttons["account"].firstMatch.tap()
        let dashboard = app.buttons["account.dashboard"]
        // Saved profiles of other tests can push it below the sheet's fold.
        if !dashboard.waitForExistence(timeout: 5) { app.swipeUp() }
        XCTAssertTrue(dashboard.waitForExistence(timeout: 10), "No Dashboard for an administrator")
        dashboard.tap()
        for (section, expected) in [("Overview", "Version"), ("Users", "bloom-test"), ("Scheduled tasks", "Run"),
                                    ("Libraries", "Movies"), ("Activity", nil), ("Plugins", nil), ("Logs", nil), ("API keys", nil)] {
            let link = app.buttons[section].firstMatch
            if !link.exists { app.swipeUp() }
            XCTAssertTrue(link.waitForExistence(timeout: 5), "No \(section) section")
            link.tap()
            if let expected {
                XCTAssertTrue(app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH %@", expected)).firstMatch
                    .waitForExistence(timeout: 15), "\(section) did not show \(expected)")
            } else {
                XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH 'could not load'")).firstMatch
                    .waitForExistence(timeout: 4), "\(section) could not load")
            }
            shot("36-dashboard-\(section.lowercased().replacingOccurrences(of: " ", with: "-"))")
            app.navigationBars.buttons.firstMatch.tap()
            sleep(1)
        }
        try brandingImage(app)
    }

    /// A deeper walk of the dashboard on dev/jellyfin-local (SHOT_SYNCPLAY):
    /// a user's page and its access editor, a task's page, a log file, and
    /// the New user and New key prompts closed with Cancel. Read only: no
    /// Run, Scan, Save, Delete, Revoke, Uninstall or power action is tapped.
    func testDashboardWalk() throws {
        guard let server = env["SHOT_SYNCPLAY"], !server.isEmpty else { throw XCTSkip("needs SHOT_SYNCPLAY=<dev/jellyfin-local address>") }
        let app = XCUIApplication()
        app.launchEnvironment = [
            "BLOOM_SERVER": server, "BLOOM_USER": "bloom-test", "BLOOM_PASSWORD": "bloom-test",
            "BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CHROMECAST_MOCK": "1",
        ]
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 60), "Home did not load")
        sleep(2)
        app.buttons["account"].firstMatch.tap()
        let dashboard = app.buttons["account.dashboard"]
        if !dashboard.waitForExistence(timeout: 5) { app.swipeUp() }
        XCTAssertTrue(dashboard.waitForExistence(timeout: 10), "No Dashboard for an administrator")
        dashboard.tap()
        sleep(1)
        shot("40-walk-dashboard")
        let back = { app.navigationBars.buttons.firstMatch.tap(); sleep(1) }
        func open(_ section: String) {
            let link = app.buttons[section].firstMatch
            if !link.exists { app.swipeUp() }
            XCTAssertTrue(link.waitForExistence(timeout: 5), "No \(section) section")
            link.tap()
            sleep(3)
        }
        func tap(_ text: XCUIElement) {
            XCTAssertTrue(text.waitForExistence(timeout: 10), "No \(text)")
            text.tap()
            sleep(3)
        }

        open("Overview")
        shot("41-walk-overview")
        app.swipeUp(); sleep(1)
        shot("41b-walk-overview-end")
        back()

        open("Users")
        shot("42-walk-users")
        app.buttons["New user"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 5), "No New user prompt")
        shot("42b-walk-new-user")
        app.buttons["Cancel"].tap(); sleep(1)
        app.staticTexts["bloom-test"].firstMatch.tap(); sleep(3)
        shot("42c-walk-user")
        let access = app.buttons["Access and permissions"].firstMatch
        if access.waitForExistence(timeout: 5) {
            access.tap(); sleep(3)
            shot("42d-walk-access")
            app.swipeUp(); sleep(1)
            shot("42e-walk-access-end")
            // Nothing changed, so Discard leaves at once.
            let discard = app.buttons["Discard"].firstMatch
            if discard.exists { discard.tap() } else { back() }
            sleep(1)
            let confirm = app.sheets.buttons["Discard"].firstMatch
            if confirm.waitForExistence(timeout: 2) { confirm.tap(); sleep(1) }
        }
        back(); back()

        open("Libraries"); shot("43-walk-libraries"); back()
        open("Devices"); shot("44-walk-devices"); back()
        open("Activity"); shot("45-walk-activity")
        app.swipeUp(); sleep(1); shot("45b-walk-activity-more"); back()

        open("Scheduled tasks")
        shot("46-walk-tasks")
        tap(app.staticTexts["Update Plugins"].firstMatch)
        shot("46b-walk-task")
        back(); back()

        open("Plugins"); shot("47-walk-plugins"); back()

        open("Logs")
        shot("48-walk-logs")
        tap(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH '.log' OR label ENDSWITH '.txt'")).firstMatch)
        shot("48b-walk-log")
        app.swipeUp(); sleep(1)
        shot("48c-walk-log-scrolled")
        back()
        let find = app.searchFields.firstMatch
        XCTAssertTrue(find.waitForExistence(timeout: 5), "No log search")
        find.tap(); find.typeText("zzzz"); sleep(1)
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'zzzz'")).firstMatch.waitForExistence(timeout: 5),
                      "No empty search state")
        shot("48d-walk-log-no-match")
        // iOS 26 closes a search with an X ("Close"); older ones say Cancel.
        // iOS 26 closes the search with an X beside the field, which has no
        // label in the tree: tap the spot right of the field.
        let field = find.frame
        app.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: field.maxX + 30, dy: field.midY)).tap()
        sleep(1)
        back()

        open("API keys")
        shot("49-walk-keys")
        app.buttons["New key"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 5), "No New key prompt")
        shot("49b-walk-new-key")
        app.buttons["Cancel"].tap(); sleep(1)
        back()

        open("Branding")
        shot("50-walk-branding")
        app.swipeUp(); sleep(1)
        shot("50b-walk-branding-end")
        back()
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH 'could not load'")).firstMatch.exists)
    }

    /// Branding: upload a splash image (the app's own, BLOOM_TEST_SPLASH)
    /// and remove it again, so the container server ends as it started.
    private func brandingImage(_ app: XCUIApplication) throws {
        let link = app.buttons["Branding"].firstMatch
        if !link.exists { app.swipeUp() }
        XCTAssertTrue(link.waitForExistence(timeout: 5), "No Branding section")
        link.tap()
        let state = app.staticTexts["branding.splash.state"]
        for _ in 0..<4 where !state.exists { app.swipeUp() }
        XCTAssertTrue(state.waitForExistence(timeout: 15), "Branding did not show the splash state")
        // A custom image left by a failed run goes first.
        if state.label.hasPrefix("A custom image") { removeSplash(app, state) }
        XCTAssertTrue(state.label.hasPrefix("No custom image"), "State says \(state.label)")
        // The page can still be settling from the swipe: a tap then opens no menu.
        let upload = app.buttons["branding.upload"]
        let test = app.buttons["Test image"]
        for _ in 0..<3 where !test.exists {
            upload.tap()
            _ = test.waitForExistence(timeout: 3)
        }
        XCTAssertTrue(test.exists, "No Test image entry")
        test.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'A custom image'")).firstMatch
            .waitForExistence(timeout: 20), "The upload did not set an image")
        sleep(2)
        shot("37-dashboard-branding-image")
        removeSplash(app, state)
        XCTAssertTrue(state.label.hasPrefix("No custom image"), "The removal left \(state.label)")
        shot("38-dashboard-branding-removed")
    }

    private func removeSplash(_ app: XCUIApplication, _ state: XCUIElement) {
        let remove = app.buttons["branding.remove"]
        XCTAssertTrue(remove.waitForExistence(timeout: 5), "No Remove button")
        remove.tap()
        let confirm = app.buttons["Remove"].firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5), "No confirm for Remove")
        confirm.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'No custom image'")).firstMatch
            .waitForExistence(timeout: 20), "The removal did not clear the image")
    }

    /// This phone as a target: the mock's socket sends a message, a Play, a
    /// pause and a Stop, as another Jellyfin app would. Needs
    /// BLOOM_MOCK_SOCKET=1 on the mock.
    func testRemoteControl() throws {
        guard env["SHOT_SOCKET"] == "1" else { throw XCTSkip("needs BLOOM_MOCK_SOCKET on the mock") }
        let app = launch()
        let card = firstCard(app)
        XCTAssertTrue(card.waitForExistence(timeout: 10))
        let item = String(card.identifier.dropFirst("card.".count))

        XCTAssertTrue(push(#"{"MessageType":"GeneralCommand","Data":{"Name":"DisplayMessage","Arguments":{"Header":"Hello","Text":"from the mock"}}}"#),
                      "The app's socket did not open")
        let toast = app.descendants(matching: .any)["toast"]
        XCTAssertTrue(toast.waitForExistence(timeout: 5), "No toast for DisplayMessage")
        XCTAssertTrue(toast.label.contains("from the mock"), "Toast says \(toast.label)")
        shot("22-remote-message")

        XCTAssertTrue(push(#"{"MessageType":"Play","Data":{"ItemIds":["\#(item)"],"PlayCommand":"PlayNow","StartPositionTicks":300000000}}"#))
        let close = app.buttons["player.close"]
        XCTAssertTrue(close.waitForExistence(timeout: 15), "A Play from another device did not open the player")
        sleep(3)
        let pause = app.buttons["player.pause"]
        let before = pause.label
        XCTAssertTrue(push(#"{"MessageType":"Playstate","Data":{"Command":"PlayPause"}}"#))
        XCTAssertTrue(becomes(pause, "label != %@", before, timeout: 10), "PlayPause from another device changed nothing")
        shot("23-remote-paused")

        XCTAssertTrue(push(#"{"MessageType":"Play","Data":{"ItemIds":["\#(item)"],"PlayCommand":"PlayNext"}}"#))
        let queued = app.descendants(matching: .any)["toast"]
        XCTAssertTrue(queued.waitForExistence(timeout: 10), "No word for PlayNext")
        XCTAssertTrue(queued.label.contains("queued"), "Toast says \(queued.label)")
        XCTAssertTrue(close.exists, "PlayNext replaced the player")

        XCTAssertTrue(push(#"{"MessageType":"Playstate","Data":{"Command":"Stop"}}"#))
        let gone = NSPredicate(format: "exists == false")
        let closed = expectation(for: gone, evaluatedWith: close)
        wait(for: [closed], timeout: 10)
        shot("24-remote-stopped")
    }

    /// Sends a message over every socket the mock holds; true when one
    /// took it. Retries while the app's socket opens.
    private func push(_ message: String) -> Bool {
        let base = MockServer.base
        for _ in 0..<20 {
            var request = URLRequest(url: URL(string: base + "/mock/socket")!)
            request.httpMethod = "POST"
            request.httpBody = Data(message.utf8)
            let done = DispatchSemaphore(value: 0)
            nonisolated(unsafe) var sent = 0
            URLSession.shared.dataTask(with: request) { data, _, _ in
                if let data, let answer = try? JSONSerialization.jsonObject(with: data) as? [String: Int] {
                    sent = answer["Sockets"] ?? 0
                }
                done.signal()
            }.resume()
            done.wait()
            if sent > 0 { return true }
            sleep(1)
        }
        return false
    }

    /// Chromecast, on the core's fake device only: choose it, play an item
    /// to it, and the accessory shows it playing.
    func testChromecast() {
        // The fake device drops its socket when the app goes away, as a
        // socket that died while the phone slept (spec §8.4).
        let app = launch(["BLOOM_CAST_DROP_AWAY": "1"])
        app.buttons["account"].firstMatch.tap()
        sleep(1)
        app.buttons["Play on"].firstMatch.tap()
        sleep(3)
        shot("20-play-on-chromecast")
        // The fake device is the only one in the Chromecast section.
        // The core's fake device: name "Mock TV", model "Mock".
        let device = app.buttons.matching(NSPredicate(format: "label == 'Mock TV, Mock'")).firstMatch
        XCTAssertTrue(device.waitForExistence(timeout: 10), "The fake Chromecast is not listed")
        device.tap()
        sleep(3)
        closePlayOn(app)
        cardAboveAccessory(app).tap()
        tapPlay(app)
        // The load reached the fake device: the app says so once the
        // device took it ("{device} did not take it" otherwise).
        let playing = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS 'Playing on Mock TV'")).firstMatch
        XCTAssertTrue(playing.waitForExistence(timeout: 20), "No word that the fake device took the item")
        shot("21-casting")
        XCTAssertFalse(app.buttons["player.close"].exists, "The player opened on this device")

        // Away and back: the fake device dropped the socket while the app
        // was away; the session connects again and reads the device. The
        // controls answer over the new socket: a mute reaches the device.
        // (The fake Jellyfin app reports its item only on a command, so
        // the transport does not show here; the real app reports itself.)
        XCUIDevice.shared.press(.home)
        sleep(5)
        app.activate()
        sleep(2)
        // The cast accessory over the tab bar opens the Play on sheet.
        let accessory = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Mock TV'")).firstMatch
        XCTAssertTrue(accessory.waitForExistence(timeout: 10), "No cast accessory after the return")
        accessory.tap()
        let mute = app.buttons["Mute"].firstMatch
        XCTAssertTrue(mute.waitForExistence(timeout: 15), "Not connected again after the return")
        XCTAssertFalse(app.staticTexts["Connecting…"].exists, "Not connected again after the return")
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Not connected'")).firstMatch.exists)
        mute.tap()
        XCTAssertTrue(app.buttons["Unmute"].firstMatch.waitForExistence(timeout: 10), "The mute did not reach the device")
        shot("24-cast-after-return")
        app.buttons["Unmute"].firstMatch.tap()
    }

    /// Without the local network the Play on sheet says so and offers
    /// Settings; it never calls a device offline (spec §8.4). The
    /// simulator never refuses: BLOOM_LOCAL_NETWORK_DENIED stands in.
    func testLocalNetworkDenied() {
        let app = launch(["BLOOM_CHROMECAST_MOCK": "0", "BLOOM_LOCAL_NETWORK_DENIED": "1"])
        app.buttons["account"].firstMatch.tap()
        sleep(1)
        app.buttons["Play on"].firstMatch.tap()
        let sentence = app.staticTexts["Bloom may not use the local network"]
        XCTAssertTrue(sentence.waitForExistence(timeout: 10), "The refusal is not said")
        XCTAssertTrue(app.buttons["Open Settings"].exists, "No way to Settings")
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label CONTAINS[c] 'offline'")).firstMatch.exists)
        shot("25-local-network-denied")
    }

    /// The server goes away while the app is on its tabs (spec §16.9):
    /// the accessory says "{server} is offline" with Retry, the Downloads
    /// tab is selected, Home is the offline page; Retry with the server
    /// back loads the pages again. The app talks to the mock through a
    /// relay (Relay.swift) that the test stops and starts.
    func testOffline() throws {
        guard env["SHOT_SOCKET"] == "1" else { throw XCTSkip("needs BLOOM_MOCK_SOCKET on the mock") }
        // A free port, kept for the restart below.
        let relay = Relay(to: MockServer.port)
        XCTAssertTrue(relay.start(), "The relay did not listen")
        defer { relay.stop() }
        let app = launch(["BLOOM_SERVER": "http://127.0.0.1:\(relay.port)"])

        // The socket closes with the server; one look says it is gone.
        relay.stop()
        let accessory = app.descendants(matching: .any)["offline.accessory"]
        XCTAssertTrue(accessory.waitForExistence(timeout: 30), "No offline accessory")
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label ENDSWITH 'is offline'")).firstMatch.exists,
                      "The accessory does not name the server")
        XCTAssertTrue(app.navigationBars["Downloads"].waitForExistence(timeout: 5), "The Downloads tab is not selected")
        shot("30-offline-downloads")

        app.buttons["Home"].firstMatch.tap()
        XCTAssertTrue(app.descendants(matching: .any)["offline.home"].waitForExistence(timeout: 5), "Home is not the offline page")
        // Home's page says it; the accessory would say it twice.
        XCTAssertTrue(accessory.waitForNonExistence(timeout: 5), "The accessory shows over the offline Home")
        shot("31-offline-home")

        // Retry while the server is still away: still offline, and said.
        // Home's own Retry: the identifier does not reach the tree from
        // inside ContentUnavailableView, so the label finds it.
        let retry = app.buttons.matching(NSPredicate(format: "identifier == 'offline.retry' OR label == 'Retry'")).firstMatch
        retry.tap()
        let toast = app.descendants(matching: .any)["toast"]
        XCTAssertTrue(toast.waitForExistence(timeout: 10), "No word after a failed Retry")
        XCTAssertTrue(app.descendants(matching: .any)["offline.home"].exists, "A failed Retry ended the offline state")

        XCTAssertTrue(relay.start(), "The relay did not listen again")
        XCTAssertTrue(retry.waitForExistence(timeout: 5))
        retry.tap()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 15), "Home did not load again")
        app.buttons["Downloads"].firstMatch.tap()
        XCTAssertTrue(accessory.waitForNonExistence(timeout: 5), "The accessory stayed after the return")
        shot("32-back-online")
    }

    /// News of the server reloads an open library grid in place (spec
    /// §4.7): it keeps its scroll position, and a title made a favorite
    /// elsewhere shows on the Favorites filter.
    func testLibraryNews() throws {
        guard env["SHOT_SOCKET"] == "1" else { throw XCTSkip("needs BLOOM_MOCK_SOCKET on the mock") }
        let app = launch()
        app.buttons["Library"].firstMatch.tap()
        sleep(1)
        app.cells.firstMatch.tap()
        let cards = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'card.'"))
        XCTAssertTrue(cards.firstMatch.waitForExistence(timeout: 10), "The library did not load")
        sleep(2)
        app.swipeUp()
        sleep(2)
        let ids = cards.allElementsBoundByIndex.filter(\.isHittable).map(\.identifier)
        guard let middle = ids.dropFirst(ids.count / 2).first else { return XCTFail("No cards on screen") }
        let card = app.descendants(matching: .any)[middle]
        let before = card.frame
        let pages = MockServer.count("LibraryPages")
        XCTAssertTrue(push(#"{"MessageType":"LibraryChanged","Data":{"ItemsAdded":[],"ItemsUpdated":[],"ItemsRemoved":[]}}"#),
                      "The app's socket did not open")
        // 3 s of quiet, then the reload: the mock serves the grid again.
        XCTAssertTrue(MockServer.wait(timeout: 30) { MockServer.count("LibraryPages") > pages }, "The news reloaded no page of the grid")
        sleep(2) // the answer laid out
        XCTAssertTrue(card.exists, "The card left the grid")
        XCTAssertEqual(card.frame, before, "The grid lost its scroll position")

        app.buttons["library.filter"].firstMatch.tap()
        app.buttons["Favorites"].firstMatch.tap()
        sleep(3)
        let favorites = Set(cards.allElementsBoundByIndex.map(\.identifier))
        guard let id = ids.first(where: { !favorites.contains($0) }) else { return XCTFail("Every card is a favorite") }
        let item = String(id.dropFirst("card.".count))
        XCTAssertTrue(mockRequest("POST", "/FavoriteItems/\(item)"), "The mock took no favorite")
        defer { mockRequest("DELETE", "/FavoriteItems/\(item)") }
        XCTAssertTrue(push(#"{"MessageType":"UserDataChanged","Data":{"UserDataList":[]}}"#))
        XCTAssertTrue(app.descendants(matching: .any)[id].waitForExistence(timeout: 20), "The grid did not load again on news")
        shot("33-library-news")
    }

    /// A request to the mock as another client would make it; true on a 2xx.
    @discardableResult
    private func mockRequest(_ method: String, _ path: String) -> Bool {
        MockServer.request(method, path).status / 100 == 2
    }
}
