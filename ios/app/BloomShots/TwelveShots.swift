import CryptoKit
import XCTest

/// What Jellyfin 12 added to the player and the pages, on the phone:
/// "Are you still watching?", the frame steps, Play All on a library of
/// shows, Mark played on a folder, "Created by" on a show's page. Against
/// dev/mock-jellyfin, muted, no reports, the fake Chromecast only.
@MainActor
final class TwelveShots: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }
    private var dir: String { env["SHOT_DIR"] ?? "/tmp/bloom-shots" }

    override func setUp() async throws {
        continueAfterFailure = false
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
        sleep(3) // the pictures
        return app
    }

    /// The mock's id of a name (its `ident`).
    private func ident(_ text: String) -> String {
        Insecure.MD5.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    private func cards(_ app: XCUIApplication) -> XCUIElementQuery {
        app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'card.'"))
    }

    private func openTab(_ app: XCUIApplication, _ name: String) {
        if !app.buttons[name].exists { app.buttons["Home"].firstMatch.tap() }
        XCTAssertTrue(app.buttons[name].firstMatch.waitForExistence(timeout: 5), "No \(name) tab")
        app.buttons[name].firstMatch.tap()
    }

    private func tapPlay(_ app: XCUIApplication) {
        let play = app.buttons["detail.play"]
        XCTAssertTrue(play.waitForExistence(timeout: 15), "No Play button on the detail page")
        sleep(2)
        let plays = app.buttons.matching(identifier: "detail.play").allElementsBoundByIndex
        (plays.first(where: \.isHittable) ?? play).tap()
    }

    private func becomes(_ element: XCUIElement, _ format: String, _ args: CVarArg..., timeout: TimeInterval = 15) -> Bool {
        let predicate = NSPredicate(format: format, argumentArray: args)
        return XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: predicate, object: element)], timeout: timeout) == .completed
    }

    /// An episode of Next Up, playing, then paused (a paused app goes idle,
    /// so the taps that follow do not wait on a playing video).
    private func playEpisode(_ app: XCUIApplication) {
        let episode = cards(app).matching(NSPredicate(format: "label CONTAINS 'Chapter'")).firstMatch
        for _ in 0..<4 where !episode.exists {
            app.swipeUp()
            sleep(1)
        }
        XCTAssertTrue(episode.waitForExistence(timeout: 10), "No episode card on Home")
        episode.tap()
        tapPlay(app)
        let pause = app.buttons["player.pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15), "The player did not open")
        sleep(3)
        if pause.label == "Pause" { pause.tap() }
        sleep(1)
    }

    /// Plays the last seconds of the item: the end comes by itself.
    private func playToTheEnd(_ app: XCUIApplication) {
        app.sliders["player.position"].adjust(toNormalizedSliderPosition: 0.97)
        sleep(1)
        let pause = app.buttons["player.pause"]
        if pause.label == "Play" { pause.tap() }
    }

    /// With a rule of one episode and no time (BLOOM_STILL_WATCHING=1:0),
    /// the end of the first episode asks before the next one: it waits
    /// paused under the question; Continue plays it, and Stop at the next
    /// end closes the player.
    func testStillWatching() {
        let app = launch(["BLOOM_STILL_WATCHING": "1:0"])
        playEpisode(app)
        let title = app.staticTexts["player.title"]
        let first = title.label
        playToTheEnd(app)
        let question = app.alerts["Are you still watching?"]
        XCTAssertTrue(question.waitForExistence(timeout: 40), "No question at the end of the episode")
        XCTAssertTrue(becomes(title, "label != %@", first), "The next episode did not load under the question")
        XCTAssertEqual(app.buttons["player.pause"].label, "Play", "The next episode plays under the question")
        sleep(1)
        shot("j12-01-still-watching")
        question.buttons["Continue"].tap()
        XCTAssertTrue(question.waitForNonExistence(timeout: 5), "The question stayed after Continue")
        XCTAssertTrue(becomes(app.buttons["player.pause"], "label == 'Pause'"), "Continue did not play the episode")
        print("SHOTS: still watching went from \(first) to \(title.label)")

        // The next end asks again (one episode a session); Stop closes.
        sleep(2)
        app.buttons["player.pause"].tap()
        sleep(1)
        playToTheEnd(app)
        XCTAssertTrue(question.waitForExistence(timeout: 40), "No question at the second end")
        question.buttons["Stop"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["Video"].waitForNonExistence(timeout: 15), "Stop did not close the player")
        shot("j12-02-still-watching-stopped")
    }

    /// Settings › Playback has the choice; Off is saved in the display
    /// preferences under the web's key and value.
    func testStillWatchingSetting() {
        let app = launch()
        app.buttons["account"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Settings"].firstMatch.waitForExistence(timeout: 5), "No account sheet")
        app.buttons["Settings"].firstMatch.tap()
        XCTAssertTrue(app.buttons["Playback"].firstMatch.waitForExistence(timeout: 5), "No Settings sheet")
        app.buttons["Playback"].firstMatch.tap()
        let choice = app.descendants(matching: .any)["settings.stillWatching"].firstMatch
        for _ in 0..<4 where !(choice.exists && choice.isHittable) {
            app.swipeUp()
            sleep(1)
        }
        XCTAssertTrue(choice.waitForExistence(timeout: 10), "No still-watching choice in Settings › Playback")
        XCTAssertTrue(choice.label.contains("still watching"), "The choice reads \(choice.label)")
        shot("j12-03-settings-still-watching")
        choice.tap()
        let off = app.buttons["Off"].firstMatch
        XCTAssertTrue(off.waitForExistence(timeout: 5), "No Off in the choice")
        sleep(1)
        shot("j12-04-settings-still-watching-menu")
        off.tap()
        let saved = MockServer.wait(timeout: 15) {
            let display = MockServer.json("/DisplayPreferences/usersettings?userId=u1&client=emby") as? [String: Any]
            return (display?["CustomPrefs"] as? [String: Any])?["stillWatchingPrompt"] as? String == "disabled"
        }
        XCTAssertTrue(saved, "Off was not saved as stillWatchingPrompt=disabled")
    }

    /// Paused, the bar has a frame back and a frame forward; 25 frames
    /// on (a 24 fps clip) is more than a second, and the video stays
    /// paused. Playing, the steps are gone.
    func testFrameStep() {
        let app = launch()
        playEpisode(app)
        let forward = app.buttons["player.frameForward"]
        let back = app.buttons["player.frameBack"]
        XCTAssertTrue(forward.waitForExistence(timeout: 5), "No frame step on the paused bar")
        XCTAssertTrue(back.exists, "No frame back on the paused bar")
        let position = app.sliders["player.position"]
        let before = position.value as? String ?? ""
        shot("j12-05-frame-steps")
        for _ in 0..<25 { forward.tap() }
        XCTAssertTrue(becomes(position, "value != %@", before, timeout: 10), "25 frames on did not move the position (\(before))")
        back.tap()
        back.tap()
        sleep(1)
        XCTAssertEqual(app.buttons["player.pause"].label, "Play", "A frame step started the video")
        print("SHOTS: frame steps from \(before) to \(position.value as? String ?? "")")
        app.buttons["player.pause"].tap()
        XCTAssertTrue(forward.waitForNonExistence(timeout: 5), "The frame steps stayed while the video plays")
        app.buttons["player.pause"].tap()
        app.buttons["player.close"].tap()
    }

    /// Play All on the Shows library: the first show stands for its next
    /// episode, which plays.
    func testPlayAllShows() {
        let app = launch()
        openTab(app, "Library")
        let shows = app.staticTexts["Shows"].firstMatch
        XCTAssertTrue(shows.waitForExistence(timeout: 10), "No Shows library")
        shows.tap()
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 10), "The Shows library has no card")
        sleep(2)
        let play = app.buttons["library.play"].firstMatch
        XCTAssertTrue(play.waitForExistence(timeout: 5), "No Play menu on a library of shows")
        play.tap()
        sleep(1)
        shot("j12-06-library-play-all")
        app.buttons["Play All"].firstMatch.tap()
        let title = app.staticTexts["player.title"]
        XCTAssertTrue(title.waitForExistence(timeout: 20), "Play All did not open the player")
        // Sorted by name, Station Eleven North comes first; its next
        // episode not watched is S1:E2 (the mock watched S1:E1).
        XCTAssertTrue(becomes(title, "label CONTAINS 'Chapter'"), "The player plays no episode: \(title.label)")
        sleep(2)
        shot("j12-07-play-all-episode")
        let pause = app.buttons["player.pause"]
        if pause.label == "Pause" { pause.tap() }
        print("SHOTS: Play All on Shows plays \(title.label)")
        app.buttons["player.close"].tap()
    }

    /// A folder of a folder library has Mark played in its menu; the
    /// server gets it.
    func testMarkFolderPlayed() {
        XCTAssertEqual(MockServer.request("POST", "/mock/folders").status, 204, "The mock has no folders")
        let app = launch()
        openTab(app, "Library")
        let folders = app.staticTexts["Folders"].firstMatch
        XCTAssertTrue(folders.waitForExistence(timeout: 10), "No Folders library")
        folders.tap()
        let folder = cards(app).matching(NSPredicate(format: "label CONTAINS 'Holiday Clips'")).firstMatch
        XCTAssertTrue(folder.waitForExistence(timeout: 10), "No folder card")
        sleep(2)
        folder.press(forDuration: 1.2)
        let mark = app.buttons["Mark played"].firstMatch
        XCTAssertTrue(mark.waitForExistence(timeout: 5), "No Mark played on a folder")
        shot("j12-08-folder-menu")
        mark.tap()
        let id = ident("folder holiday clips")
        let played = MockServer.wait(timeout: 15) {
            let page = MockServer.json("/Items?ids=\(id)") as? [String: Any]
            let item = (page?["Items"] as? [[String: Any]])?.first
            return (item?["UserData"] as? [String: Any])?["Played"] as? Bool == true
        }
        XCTAssertTrue(played, "The folder was not marked played on the server")
    }

    /// A show's page says who created it, and a name opens the person.
    func testCreatedBy() {
        let app = launch()
        app.buttons["Search"].firstMatch.tap()
        XCTAssertTrue(app.searchFields.firstMatch.waitForExistence(timeout: 5), "No search field")
        sleep(1)
        app.typeText("Station Eleven")
        let card = cards(app).matching(NSPredicate(format: "label CONTAINS 'Station Eleven North'")).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 15), "Search did not find the show")
        card.tap()
        let line = app.descendants(matching: .any)["detail.createdBy"].firstMatch
        XCTAssertTrue(line.waitForExistence(timeout: 15), "No Created by on the show's page")
        let name = app.buttons["detail.creator.Noor Halvorsen"].firstMatch
        XCTAssertTrue(name.exists, "Created by does not name Noor Halvorsen")
        XCTAssertTrue(app.buttons["detail.creator.Ade Brandt"].firstMatch.exists, "Created by does not name Ade Brandt")
        sleep(1)
        shot("j12-09-created-by")
        name.tap()
        let role = cards(app).matching(NSPredicate(format: "label CONTAINS 'Station Eleven North'")).firstMatch
        XCTAssertTrue(role.waitForExistence(timeout: 15), "The creator's page does not list the show")
        sleep(1)
        shot("j12-10-creator-page")
    }
}
