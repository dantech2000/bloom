import XCTest

/// Sign-in, the account sheet, playlists and the dashboard's Overview:
/// walks against dev/mock-jellyfin (and the container Jellyfin of
/// dev/jellyfin-local where named, SHOT_SYNCPLAY), with screenshots in
/// SHOT_DIR. Read only on the container: nothing is saved, run or deleted.
@MainActor
final class AccountShots: XCTestCase {
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
        try? png.write(to: URL(fileURLWithPath: "\(dir)/\(name).png"))
    }

    private func launch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = MockServer.environment()
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "Home did not load")
        sleep(2)
        return app
    }

    /// The connect screen with no saved profile.
    private func launchConnect(_ extra: [String: String] = [:]) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = ["BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CHROMECAST_MOCK": "1"].merging(extra) { $1 }
        app.launchArguments = ["-accounts", "", "-activeUser", "", "-serverNames", ""]
        app.launch()
        XCTAssertTrue(app.textFields["connect.address"].waitForExistence(timeout: 15), "No connect screen")
        return app
    }

    private func text(_ app: XCUIApplication, _ format: String, _ args: CVarArg...) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: format, argumentArray: args)).firstMatch
    }

    /// The mock answers the discovery question (asked directly at its
    /// address, BLOOM_DISCOVERY_TARGETS); its row connects, and its user
    /// shows as a profile with a picture. A user with no password signs in
    /// at the tap.
    func testNearbyAndProfiles() {
        let app = launchConnect(["BLOOM_DISCOVERY_TARGETS": MockServer.host])
        let row = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'connect.nearby.'")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10), "The mock was not found on the network")
        shot("a01-connect-nearby")
        row.tap()
        let profile = app.buttons["connect.profile.tester"]
        XCTAssertTrue(profile.waitForExistence(timeout: 15), "No profile for the mock's user")
        XCTAssertTrue(app.navigationBars["Who is watching?"].exists, "The title is not Who is watching?")
        sleep(2) // the picture
        shot("a02-profiles")
        profile.tap()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 30), "The tap on a profile did not sign in")
    }

    /// A server that lists no user names itself in the title; a wrong
    /// password shows its error by the fields, and typing clears it.
    func testSignInError() throws {
        guard let server = env["SHOT_SYNCPLAY"], !server.isEmpty else { throw XCTSkip("needs SHOT_SYNCPLAY=<dev/jellyfin-local address>") }
        let app = launchConnect()
        let address = app.textFields["connect.address"]
        address.tap()
        address.typeText(server.replacingOccurrences(of: "http://", with: "") + "\n")
        let username = app.textFields["connect.username"]
        XCTAssertTrue(username.waitForExistence(timeout: 15), "No sign-in fields")
        XCTAssertTrue(app.navigationBars["Sign in"].exists, "The title is not Sign in")
        username.tap()
        username.typeText("bloom-test")
        let password = app.secureTextFields["connect.password"]
        password.tap()
        password.typeText("not-the-password\n")
        let error = app.descendants(matching: .any)["connect.error"].firstMatch
        XCTAssertTrue(error.waitForExistence(timeout: 15), "No error after a wrong password")
        sleep(1)
        shot("a03-sign-in-error")
        // Under the password field, above Sign In.
        let signIn = app.buttons["Sign In"].firstMatch
        XCTAssertLessThan(error.frame.minY, signIn.frame.minY, "The error is not above Sign In")
        XCTAssertGreaterThan(error.frame.minY, password.frame.maxY, "The error is not under the password")
        password.tap()
        password.typeText("x")
        XCTAssertTrue(error.waitForNonExistence(timeout: 5), "Typing did not clear the error")
        shot("a04-sign-in-typed")
    }

    /// A local name for the server, from the account sheet: the sheet and
    /// Settings › Account show it; an empty name brings the server's back.
    func testRenameServer() {
        let app = launch()
        app.buttons["account"].firstMatch.tap()
        // The mock's: a test on the container may have left its profile.
        let rename = app.buttons["Rename Mock Jellyfin"].firstMatch
        XCTAssertTrue(rename.waitForExistence(timeout: 5), "No Rename on the account sheet")
        rename.tap()
        let field = app.alerts.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5), "No name field")
        field.typeText("Home server")
        shot("a05-rename-server")
        app.buttons["Save"].firstMatch.tap()
        XCTAssertTrue(text(app, "label ==[c] 'Home server'").waitForExistence(timeout: 5), "The new name does not show")
        sleep(1)
        shot("a06-renamed-server")
        app.buttons["Settings"].firstMatch.tap()
        app.collectionViews.buttons["Account"].firstMatch.tap()
        XCTAssertTrue(text(app, "label CONTAINS 'Home server'").waitForExistence(timeout: 5), "Settings › Account shows the old name")
        shot("a07-account-renamed")
        app.navigationBars.buttons.firstMatch.tap()
        app.buttons["Done"].firstMatch.tap()
        sleep(1)
        // Back to the server's own name.
        app.buttons["account"].firstMatch.tap()
        let renameAgain = app.buttons["Rename Home server"].firstMatch
        XCTAssertTrue(renameAgain.waitForExistence(timeout: 5))
        renameAgain.tap()
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: "Home server".count))
        app.buttons["Save"].firstMatch.tap()
        XCTAssertTrue(text(app, "label ==[c] 'Mock Jellyfin'").waitForExistence(timeout: 5), "The server's own name did not come back")
    }

    /// No playlist yet: the page says so and makes one.
    func testEmptyPlaylists() {
        let app = launch()
        if !app.buttons["Library"].exists { app.buttons["Home"].firstMatch.tap() }
        app.buttons["Library"].firstMatch.tap()
        app.buttons["Playlists"].firstMatch.tap()
        let new = app.buttons["playlists.new"]
        XCTAssertTrue(new.waitForExistence(timeout: 10), "No empty state with New Playlist")
        shot("a08-playlists-empty")
        new.tap()
        let field = app.textFields["Name"].firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5), "No name prompt")
        field.typeText("Weekend")
        app.buttons["Create"].firstMatch.tap()
        let card = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Weekend'")).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 10), "The new playlist is not listed")
        sleep(1)
        shot("a09-playlists-one")
    }

    /// Settings › Home: the rows Bloom shows first, the web-only ones in
    /// their own quiet group under them.
    func testHomeRows() {
        let app = launch()
        app.buttons["account"].firstMatch.tap()
        app.buttons["Settings"].firstMatch.tap()
        app.collectionViews.buttons["Home"].firstMatch.tap()
        XCTAssertTrue(app.descendants(matching: .any)["home.row.nextup"].waitForExistence(timeout: 10), "No Next Up row")
        let web = app.descendants(matching: .any)["home.row.livetv"]
        for _ in 0..<2 where !web.isHittable { app.swipeUp() }
        XCTAssertTrue(text(app, "label ==[c] 'Only in Jellyfin Web'").exists, "No group for the web-only rows")
        shot("a10-settings-home")
    }

    /// The Overview on the container: added this week, the disks, and the
    /// list behind the count. Nothing is run or saved.
    func testOverview() throws {
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
        let overview = app.buttons["Overview"].firstMatch
        XCTAssertTrue(overview.waitForExistence(timeout: 10), "No Overview")
        sleep(1)
        overview.tap()
        XCTAssertTrue(text(app, "label ==[c] 'Version'").waitForExistence(timeout: 15), "The Overview did not load")
        sleep(3)
        shot("a11-overview")
        // The alerts above can push the rest down: scroll until each shows.
        let added = app.buttons["overview.added"].firstMatch
        for _ in 0..<2 where !added.isHittable { app.swipeUp(); sleep(1) }
        XCTAssertTrue(added.waitForExistence(timeout: 30), "No Added this week")
        shot("a12-overview-middle")
        let storage = text(app, "label ==[c] 'Storage'")
        for _ in 0..<4 where !storage.exists { app.swipeUp(); sleep(1) }
        XCTAssertTrue(storage.exists, "No Storage section")
        shot("a13-overview-end")
        for _ in 0..<4 where !added.isHittable { app.swipeDown(); sleep(1) }
        if added.isHittable {
            added.tap()
            sleep(2)
            shot("a14-added-this-week")
        }
    }
}
