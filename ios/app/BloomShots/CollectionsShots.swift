import XCTest

/// The views of Jellyfin 12.0 against dev/mock-jellyfin: a library's
/// collections, playlists and studios, a studio's page, "Included In" on a
/// title's page, and the Upcoming row of Home. Screenshots go to SHOT_DIR
/// with a "collections-" prefix.
@MainActor
final class CollectionsShots: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }
    private var dir: String { env["SHOT_DIR"] ?? "/tmp/bloom-shots" }

    override func setUp() async throws {
        continueAfterFailure = true
        MockServer.reset()
        XCUIDevice.shared.orientation = .portrait
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? png.write(to: URL(fileURLWithPath: "\(dir)/collections-\(name).png"))
    }

    private func launch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = MockServer.environment()
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "Home did not load")
        sleep(2) // the pictures
        return app
    }

    private func cards(_ app: XCUIApplication) -> XCUIElementQuery {
        app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'card.'"))
    }

    private func card(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        cards(app).matching(NSPredicate(format: "label CONTAINS %@", title)).firstMatch
    }

    private func scrollTo(_ app: XCUIApplication, _ element: XCUIElement, swipes: Int = 8) -> Bool {
        for _ in 0..<swipes {
            if element.exists, element.isHittable { return true }
            app.swipeUp(velocity: .slow)
        }
        return element.exists && element.isHittable
    }

    /// Opens the Movies library from the Library tab.
    private func openMovies(_ app: XCUIApplication) {
        app.buttons["Library"].firstMatch.tap()
        let movies = app.cells.containing(.staticText, identifier: "Movies").firstMatch
        XCTAssertTrue(movies.waitForExistence(timeout: 10), "No Movies library")
        movies.tap()
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 10), "The library has no card")
    }

    /// Picks a view of the library from its View menu.
    private func pick(_ app: XCUIApplication, _ view: String) {
        let menu = app.buttons["library.view"].firstMatch
        XCTAssertTrue(menu.waitForExistence(timeout: 5), "No View menu on the library")
        menu.tap()
        let entry = app.buttons[view].firstMatch
        XCTAssertTrue(entry.waitForExistence(timeout: 5), "No \(view) in the View menu")
        entry.tap()
        XCTAssertTrue(app.navigationBars[view].waitForExistence(timeout: 10), "The library did not show its \(view)")
    }

    /// Movies › Collections lists the collection of its films; Playlists is
    /// empty and says so; Studios lists the studios, and one opens its films.
    func testLibraryViews() {
        let app = launch()
        openMovies(app)
        pick(app, "Collections")
        XCTAssertTrue(card(app, "The Harbour Films").waitForExistence(timeout: 10), "No collection of the library")
        sleep(1)
        shot("01-movies-collections")
        pick(app, "Playlists")
        XCTAssertTrue(app.staticTexts["No playlists"].waitForExistence(timeout: 10), "An empty Playlists view says nothing")
        shot("02-movies-playlists")
        pick(app, "Studios")
        let studio = card(app, "Harbour Pictures")
        XCTAssertTrue(studio.waitForExistence(timeout: 10), "No studio of the library")
        XCTAssertTrue(card(app, "North Light Studio").exists, "The second studio is missing")
        XCTAssertFalse(card(app, "Night Desk Network").exists, "A network of the shows is in the films' studios")
        XCTAssertFalse(app.buttons["library.sort"].exists, "Studios come by name: no Sort menu")
        sleep(1)
        shot("03-movies-studios")
        studio.tap()
        XCTAssertTrue(app.navigationBars["Harbour Pictures"].waitForExistence(timeout: 10), "The studio did not open its page")
        XCTAssertTrue(card(app, "The Long Harbour").waitForExistence(timeout: 10), "The studio's page has no film of it")
        XCTAssertFalse(card(app, "Paper Lanterns").exists, "A film of the other studio is on the page")
        sleep(1)
        shot("04-studio-page")
    }

    /// A film in a collection has "Included In" with the collection, which
    /// opens; a film in none has no such row.
    func testIncludedIn() {
        let app = launch()
        openMovies(app)
        let film = card(app, "The Long Harbour")
        XCTAssertTrue(film.waitForExistence(timeout: 10), "No The Long Harbour card")
        film.tap()
        XCTAssertTrue(app.buttons["detail.play"].waitForExistence(timeout: 10), "The film's page did not open")
        let heading = app.staticTexts["Included In"]
        XCTAssertTrue(scrollTo(app, heading), "No Included In row on a film of a collection")
        let collection = card(app, "The Harbour Films")
        XCTAssertTrue(scrollTo(app, collection, swipes: 2), "No collection card in Included In")
        sleep(1)
        shot("05-included-in")
        collection.tap()
        XCTAssertTrue(app.navigationBars["The Harbour Films"].waitForExistence(timeout: 10), "The collection did not open")
        sleep(1)
        shot("06-collection-page")
    }

    /// With "upcoming" in the home rows, Home has the episodes not aired
    /// yet with the day each airs.
    func testUpcomingRow() {
        let prefs: [String: Any] = ["Id": "usersettings", "Client": "emby", "CustomPrefs": [
            "homesection0": "upcoming", "homesection1": "resume", "homesection2": "nextup", "homesection3": "latestmedia",
        ]]
        let body = try? JSONSerialization.data(withJSONObject: prefs)
        XCTAssertEqual(MockServer.request("POST", "/DisplayPreferences/usersettings?userId=u1&client=emby", body: body).status, 204)
        let app = launch()
        let heading = app.staticTexts["Upcoming"]
        XCTAssertTrue(scrollTo(app, heading, swipes: 3), "No Upcoming row on Home")
        let episode = cards(app).matching(NSPredicate(format: "label CONTAINS 'The Night Desk'")).firstMatch
        XCTAssertTrue(episode.waitForExistence(timeout: 5), "The Upcoming row has no episode")
        XCTAssertTrue(episode.label.contains("Dec 2026"), "An upcoming episode does not say when it airs: \(episode.label)")
        sleep(1)
        shot("07-home-upcoming")
    }
}
