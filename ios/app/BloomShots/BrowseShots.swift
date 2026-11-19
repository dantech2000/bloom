import XCTest

/// Browsing against dev/mock-jellyfin: the hero's info button, Home's top
/// when scrolled, "See all", Home for a new user, the library's filters,
/// the watched ticks, people in Search and their page, and the empty
/// Search. Screenshots go to SHOT_DIR with a "browse-" prefix.
@MainActor
final class BrowseShots: XCTestCase {
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
        try? png.write(to: URL(fileURLWithPath: "\(dir)/browse-\(name).png"))
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

    private func any(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any)[id]
    }

    private func cards(_ app: XCUIApplication) -> XCUIElementQuery {
        app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'card.'"))
    }

    /// Scrolls the page up until `element` is on screen; false when it
    /// never shows.
    private func scrollTo(_ app: XCUIApplication, _ element: XCUIElement, swipes: Int = 8) -> Bool {
        for _ in 0..<swipes {
            if element.exists, element.isHittable { return true }
            app.swipeUp(velocity: .slow)
        }
        return element.exists && element.isHittable
    }

    private func openTab(_ app: XCUIApplication, _ name: String) {
        if !app.buttons[name].exists { app.buttons["Home"].firstMatch.tap() }
        XCTAssertTrue(app.buttons[name].firstMatch.waitForExistence(timeout: 5), "No \(name) tab")
        app.buttons[name].firstMatch.tap()
    }

    /// One tap on the hero's info button opens the title's page.
    func testHeroInfo() {
        let app = launch()
        let info = app.buttons["hero.info"].firstMatch
        XCTAssertTrue(info.waitForExistence(timeout: 10), "No info button on the hero")
        // The slide on screen (the lazy row keeps its neighbours).
        let width = app.frame.width
        let shown = app.buttons.matching(identifier: "hero.info").allElementsBoundByIndex
            .first { $0.frame.minX >= 0 && $0.frame.maxX <= width } ?? info
        shown.tap()
        XCTAssertTrue(app.buttons["detail.play"].waitForExistence(timeout: 10), "The info button opened nothing")
        sleep(1)
        shot("01-hero-info")
    }

    /// A scrolled Home: the rows pass under a bar, not under the clock.
    func testHomeScrolled() {
        let app = launch()
        app.swipeUp(velocity: .slow)
        app.swipeUp(velocity: .slow)
        sleep(1)
        shot("02-home-scrolled")
        app.swipeUp(velocity: .slow)
        sleep(1)
        shot("02b-home-scrolled-more")
    }

    /// "See all" on Recently Added opens the library newest first.
    func testSeeAll() {
        let app = launch()
        let more = app.buttons["seeall.Recently Added in Movies"]
        XCTAssertTrue(scrollTo(app, more), "No See all on Recently Added in Movies")
        shot("03-see-all-heading")
        more.tap()
        XCTAssertTrue(app.navigationBars["Movies"].waitForExistence(timeout: 10), "See all did not open the library")
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 10), "The library has no card")
        sleep(2)
        shot("04-see-all-grid")
    }

    /// Home for a user with nothing started: Pick Something and the genres.
    func testNewUser() {
        XCTAssertEqual(MockServer.request("POST", "/mock/fresh").status, 204, "The mock did not take the fresh user")
        let app = launch()
        let picks = app.staticTexts["Pick Something"]
        XCTAssertTrue(scrollTo(app, picks, swipes: 2), "No Pick Something row on a new user's Home")
        sleep(2)
        shot("05-new-user")
        let genre = any(app, "genre.Drama")
        XCTAssertTrue(scrollTo(app, genre, swipes: 3), "No genre to browse")
        shot("05b-new-user-genres")
        genre.tap()
        XCTAssertTrue(app.navigationBars["Drama"].waitForExistence(timeout: 10), "The genre did not open its grid")
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 10), "The genre grid has no card")
        sleep(2)
        shot("06-genre-grid")
    }

    /// The Movies grid: a genre filter, the pill over the grid, a played
    /// film's tick and a started film's line.
    func testLibraryFilters() {
        let app = launch()
        openTab(app, "Library")
        XCTAssertTrue(app.cells.firstMatch.waitForExistence(timeout: 10), "The Library tab lists no library")
        app.cells.firstMatch.tap()
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 10), "The library page has no card")
        sleep(2)
        // A Quiet Machine is played, Paper Lanterns started (the mock).
        let played = cards(app).matching(NSPredicate(format: "label CONTAINS 'A Quiet Machine'")).firstMatch
        XCTAssertTrue(played.waitForExistence(timeout: 5), "No A Quiet Machine card")
        XCTAssertTrue((played.value as? String ?? "").contains("Watched"), "A played film has no watched state: \(played.value ?? "")")
        let started = cards(app).matching(NSPredicate(format: "label CONTAINS 'Paper Lanterns'")).firstMatch
        XCTAssertTrue((started.value as? String ?? "").contains("% watched"), "A started film has no progress: \(started.value ?? "")")
        shot("07-library-ticks")
        app.buttons["library.filter"].firstMatch.tap()
        sleep(1)
        shot("08-filter-menu")
        app.buttons["Genre"].firstMatch.tap()
        sleep(1)
        XCTAssertTrue(app.buttons["Drama"].firstMatch.waitForExistence(timeout: 5), "No Drama in the genre menu")
        sleep(1) // the submenu opens
        shot("08b-genre-menu")
        let drama = app.buttons.matching(identifier: "Drama").allElementsBoundByIndex.first(where: \.isHittable) ?? app.buttons["Drama"].firstMatch
        drama.tap()
        XCTAssertTrue(app.buttons["Drama, remove filter"].waitForExistence(timeout: 10), "No pill for the genre filter")
        sleep(2)
        shot("09-filtered")
        app.buttons["library.filter"].firstMatch.tap()
        sleep(1)
        XCTAssertTrue(app.buttons["In progress"].firstMatch.waitForExistence(timeout: 5), "No In progress filter")
        shot("09b-show-menu")
        app.buttons["In progress"].firstMatch.tap()
        sleep(2)
        shot("09c-in-progress-drama")
    }

    /// "bokma" finds the person; the page has the photo, the biography
    /// (opened with More) and the roles.
    func testPeople() {
        let app = launch()
        app.buttons["Search"].firstMatch.tap()
        XCTAssertTrue(app.searchFields.firstMatch.waitForExistence(timeout: 5), "No search field")
        sleep(1)
        app.typeText("bokma")
        let person = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'person.' AND label CONTAINS 'Mara Bokma'")).firstMatch
        XCTAssertTrue(person.waitForExistence(timeout: 15), "Search found no person for bokma")
        sleep(2)
        shot("10-search-people")
        person.tap()
        XCTAssertTrue(any(app, "person.bio").waitForExistence(timeout: 10), "The person's page has no biography")
        let role = cards(app).matching(NSPredicate(format: "label CONTAINS 'as Ilse Varga'")).firstMatch
        XCTAssertTrue(role.waitForExistence(timeout: 10), "The person's page has no role under a title")
        sleep(2)
        shot("11-person")
        app.buttons["person.bio.more"].firstMatch.tap()
        sleep(1)
        shot("12-person-bio-open")
    }

    /// The empty Search: genres to start from; a search done is a recent
    /// one, and Clear takes them away.
    func testSearchStart() {
        let app = launch()
        app.buttons["Search"].firstMatch.tap()
        XCTAssertTrue(any(app, "genre.Drama").waitForExistence(timeout: 10), "The empty Search has no genres")
        sleep(1)
        shot("13-search-empty")
        app.typeText("harbour")
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 15), "Search found nothing for harbour")
        sleep(3) // a search the user stops at is kept
        app.searchFields.firstMatch.buttons.firstMatch.tap() // the field's clear button
        let recent = app.buttons["search.recent.harbour"]
        XCTAssertTrue(recent.waitForExistence(timeout: 5), "The search is not in Recent")
        shot("14-search-recent")
        recent.tap()
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 15), "A recent search found nothing")
        app.searchFields.firstMatch.buttons.firstMatch.tap()
        XCTAssertTrue(app.buttons["search.recent.clear"].waitForExistence(timeout: 5), "No Clear for the recent searches")
        app.buttons["search.recent.clear"].tap()
        XCTAssertTrue(recent.waitForNonExistence(timeout: 5), "Clear left the recent search")
    }

    /// The person page on a real Jellyfin (SHOT_SYNCPLAY, read only): its
    /// people have photos and roles the mock lacks.
    func testRealPerson() throws {
        guard let server = env["SHOT_SYNCPLAY"], !server.isEmpty else { throw XCTSkip("needs SHOT_SYNCPLAY=<a test Jellyfin's address>") }
        let app = XCUIApplication()
        app.launchEnvironment = [
            "BLOOM_SERVER": server, "BLOOM_USER": "bloom-test", "BLOOM_PASSWORD": "bloom-test",
            "BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CHROMECAST_MOCK": "1",
        ]
        app.launch()
        XCTAssertTrue(any(app, "hero").waitForExistence(timeout: 60), "Home did not load")
        sleep(3)
        shot("20-real-home")
        app.swipeUp(velocity: .slow)
        app.swipeUp(velocity: .slow)
        sleep(1)
        shot("21-real-home-scrolled")
        openTab(app, "Library")
        XCTAssertTrue(app.cells.firstMatch.waitForExistence(timeout: 10), "No library")
        app.cells.firstMatch.tap()
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 15), "The library has no card")
        sleep(3)
        shot("22-real-library")
        app.buttons["Search"].firstMatch.tap()
        XCTAssertTrue(app.searchFields.firstMatch.waitForExistence(timeout: 5), "No search field")
        sleep(1)
        shot("23-real-search-empty")
        app.typeText("bokma")
        let person = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'person.'")).firstMatch
        XCTAssertTrue(person.waitForExistence(timeout: 15), "Search found no person for bokma")
        sleep(2)
        shot("24-real-search-people")
        person.tap()
        XCTAssertTrue(cards(app).firstMatch.waitForExistence(timeout: 15), "The person's page has no title")
        sleep(3)
        shot("25-real-person")
    }
}
