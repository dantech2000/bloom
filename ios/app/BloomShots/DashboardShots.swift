import XCTest

/// The dashboard's backups, the follow of a log and the filters of the
/// activity log: walks against dev/mock-jellyfin with the user made an
/// administrator (POST /mock/admin?on=1), with screenshots in SHOT_DIR.
/// A create and a restore run on the mock only (it restarts nothing). On
/// the container (SHOT_SYNCPLAY) the walk reads and changes nothing.
@MainActor
final class DashboardShots: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }
    private var dir: String { env["SHOT_DIR"] ?? "/tmp/bloom-shots" }

    override func setUp() async throws {
        continueAfterFailure = true
        MockServer.reset()
        MockServer.request("POST", "/mock/admin?on=1")
        XCUIDevice.shared.orientation = .portrait
    }

    override func tearDown() async throws {
        MockServer.reset()
    }

    private func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? png.write(to: URL(fileURLWithPath: "\(dir)/\(name).png"))
    }

    private func text(_ app: XCUIApplication, _ format: String, _ args: CVarArg...) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: format, argumentArray: args)).firstMatch
    }

    /// The app on the mock, at a section of the dashboard.
    private func open(_ section: String, environment: [String: String] = MockServer.environment()) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment = environment
        app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["hero"].waitForExistence(timeout: 60), "Home did not load")
        sleep(2)
        app.buttons["account"].firstMatch.tap()
        let dashboard = app.buttons["account.dashboard"]
        if !dashboard.waitForExistence(timeout: 5) { app.swipeUp() }
        XCTAssertTrue(dashboard.waitForExistence(timeout: 10), "No Dashboard for an administrator")
        dashboard.tap()
        let row = app.buttons[section].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10), "No \(section) in the dashboard")
        for _ in 0..<3 where !row.isHittable { app.swipeUp(); sleep(1) }
        row.tap()
        return app
    }

    /// A submenu of the filter menu: its label starts with its name (the
    /// value on show follows).
    private func submenu(_ app: XCUIApplication, _ name: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", name)).firstMatch
    }

    /// Taps the entry of the open menu with this label: the one on top
    /// (a submenu leaves its parent's entries in the tree, out of reach).
    private func choose(_ app: XCUIApplication, _ label: String) {
        let query = app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", label))
        XCTAssertTrue(query.firstMatch.waitForExistence(timeout: 5), "No menu entry \(label)")
        sleep(1)
        guard let entry = query.allElementsBoundByIndex.last(where: { $0.isHittable }) else {
            return XCTFail("No menu entry \(label) in reach")
        }
        entry.tap()
    }

    private func dashboardState() -> [String: Any] {
        (MockServer.json("/mock/dashboard") as? [String: Any]) ?? [:]
    }

    /// The list, a new backup with Metadata, the warning of a restore, and
    /// the restore (on the mock).
    func testBackups() {
        let app = open("Backups")
        let create = app.buttons["backups.create"].firstMatch
        XCTAssertTrue(create.waitForExistence(timeout: 15), "The Backups page did not load")
        XCTAssertTrue(text(app, "label CONTAINS 'Database, Metadata, Subtitles'").waitForExistence(timeout: 5), "No backup of the mock listed")
        sleep(1)
        shot("d01-backups")
        let metadata = app.switches.matching(NSPredicate(format: "label BEGINSWITH 'Metadata'")).firstMatch
        XCTAssertTrue(metadata.exists, "No Metadata switch")
        metadata.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        create.tap()
        let made = NSPredicate { _, _ in (self.dashboardState()["Created"] as? Int) == 1 }
        wait(for: [expectation(for: made, evaluatedWith: nil)], timeout: 15)
        XCTAssertTrue(text(app, "label CONTAINS 'Database, Metadata'").waitForExistence(timeout: 10))
        sleep(1)
        shot("d02-backup-created")
        let restore = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'backups.restore.'")).firstMatch
        XCTAssertTrue(restore.waitForExistence(timeout: 5), "No Restore button")
        restore.tap()
        let alert = app.alerts["Restore this backup?"]
        XCTAssertTrue(alert.waitForExistence(timeout: 10), "No warning before a restore")
        XCTAssertTrue(alert.staticTexts.matching(NSPredicate(format: "label CONTAINS 'You cannot undo this'")).firstMatch.exists)
        shot("d03-restore-warning")
        alert.buttons["Restore and Restart"].tap()
        let restored = NSPredicate { _, _ in ((self.dashboardState()["Restored"] as? [String]) ?? []).count == 1 }
        wait(for: [expectation(for: restored, evaluatedWith: nil)], timeout: 15)
        sleep(1)
        shot("d04-restore-sent")
    }

    /// A library scan runs: a backup asks first.
    func testBackupDuringScan() {
        MockServer.request("POST", "/mock/admin?scan=1")
        let app = open("Backups")
        let create = app.buttons["backups.create"].firstMatch
        XCTAssertTrue(create.waitForExistence(timeout: 15), "The Backups page did not load")
        XCTAssertTrue(text(app, "label BEGINSWITH 'A library scan is running'").waitForExistence(timeout: 5), "No line about the scan")
        create.tap()
        let alert = app.alerts["Back up during a library scan?"]
        XCTAssertTrue(alert.waitForExistence(timeout: 5), "No question during a scan")
        shot("d05-backup-scan-warning")
        alert.buttons["Cancel"].tap()
        XCTAssertEqual(dashboardState()["Created"] as? Int, 0, "A cancelled backup was made")
    }

    /// The server log grows by a line a second; Follow shows the new lines.
    func testLogFollow() {
        let app = open("Logs")
        let file = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'log_'")).firstMatch
        XCTAssertTrue(file.waitForExistence(timeout: 15), "No server log listed")
        file.tap()
        let log = app.descendants(matching: .any)["log.text"]
        XCTAssertTrue(log.waitForExistence(timeout: 15), "The log did not load")
        sleep(1)
        let before = (log.label.components(separatedBy: "\n").last { $0.contains("tick") }) ?? ""
        shot("d06-log")
        app.buttons["log.follow"].firstMatch.tap()
        sleep(7)
        let after = (log.label.components(separatedBy: "\n").last { $0.contains("tick") }) ?? ""
        XCTAssertNotEqual(before, after, "No new lines came while following")
        shot("d07-log-following")
    }

    /// The filters go to the server: Sessions, then the server's own
    /// errors, then the next page.
    func testActivityFilters() {
        let app = open("Activity")
        let more = app.buttons["activity.more"]
        XCTAssertTrue(text(app, "label CONTAINS 'is online from Mock TV'").waitForExistence(timeout: 15), "The activity did not load")
        sleep(1)
        shot("d08-activity")
        app.buttons["activity.filter"].firstMatch.tap()
        let kind = submenu(app, "Kind")
        XCTAssertTrue(kind.waitForExistence(timeout: 5), "No Kind filter")
        kind.tap()
        choose(app, "Sessions")
        sleep(1)
        shot("d09-activity-filter-sheet")
        app.buttons["Done"].firstMatch.tap()
        let summary = app.descendants(matching: .any)["activity.summary"]
        XCTAssertTrue(summary.waitForExistence(timeout: 10), "No summary of the filters")
        XCTAssertTrue(text(app, "label == 'Sessions · 75 entries'").waitForExistence(timeout: 10), "Sessions did not filter on the server")
        sleep(1)
        shot("d10-activity-sessions")
        app.buttons["activity.filter"].firstMatch.tap()
        submenu(app, "Kind").tap()
        choose(app, "Every kind")
        submenu(app, "Level").tap()
        choose(app, "Errors")
        app.buttons["Done"].firstMatch.tap()
        XCTAssertTrue(text(app, "label == 'Errors · 37 entries'").waitForExistence(timeout: 10), "Errors did not filter on the server")
        shot("d11-activity-errors")
        // Clear, then the second page.
        app.buttons["activity.filter"].firstMatch.tap()
        choose(app, "Clear Filters")
        app.buttons["Done"].firstMatch.tap()
        for _ in 0..<12 where !more.exists { app.swipeUp() }
        XCTAssertTrue(more.waitForExistence(timeout: 5), "No Load More under a full page")
        XCTAssertTrue(more.label.contains("100 of 300"), "Load More says \(more.label)")
        more.tap()
        sleep(3)
        // The row moved down a page: the list is lazy, so scroll to it.
        for _ in 0..<30 where !(more.exists && more.label.contains("200 of 300")) { app.swipeUp() }
        XCTAssertTrue(more.exists && more.label.contains("200 of 300"), "Load More did not add a page")
        shot("d12-activity-more")
    }

    /// The container (dev/jellyfin-local, SHOT_SYNCPLAY): the backups and
    /// the activity of a real 12.x server. Read only: nothing is created
    /// or restored.
    func testContainerReadOnly() throws {
        guard let server = env["SHOT_SYNCPLAY"], !server.isEmpty else { throw XCTSkip("needs SHOT_SYNCPLAY=<dev/jellyfin-local address>") }
        let environment = [
            "BLOOM_SERVER": server, "BLOOM_USER": "bloom-test", "BLOOM_PASSWORD": "bloom-test",
            "BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CHROMECAST_MOCK": "1",
        ]
        let app = open("Backups", environment: environment)
        XCTAssertTrue(app.buttons["backups.create"].firstMatch.waitForExistence(timeout: 20), "The Backups page did not load")
        sleep(2)
        shot("d13-container-backups")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let activity = app.buttons["Activity"].firstMatch
        for _ in 0..<3 where !activity.isHittable { app.swipeDown(); sleep(1) }
        activity.tap()
        app.buttons["activity.filter"].firstMatch.tap()
        submenu(app, "Kind").tap()
        choose(app, "Sign-ins")
        app.buttons["Done"].firstMatch.tap()
        XCTAssertTrue(text(app, "label BEGINSWITH 'Sign-ins · '").waitForExistence(timeout: 15), "Sign-ins did not filter")
        sleep(1)
        shot("d14-container-activity-signins")
    }
}
