import XCTest

/// Spike S2: the clip plays through the sample-buffer output, PiP starts
/// from the button, and stays when the app goes to the background.
@MainActor
final class PiPSpikeTests: XCTestCase {
    private var dir: String { ProcessInfo.processInfo.environment["SHOT_DIR"] ?? "/tmp/bloom-shots" }

    private func shot(_ name: String) {
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? XCUIScreen.main.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: "\(dir)/\(name).png"))
    }

    func testPictureInPicture() {
        let app = XCUIApplication()
        app.launchEnvironment = ["SPIKE_URL": ProcessInfo.processInfo.environment["SPIKE_URL"] ?? ""]
        app.launch()
        sleep(6)
        shot("pip-1-playing")
        let state = app.staticTexts["spike.pipstate"].label
        print("SPIKE-TEST: \(state) | \(app.staticTexts["spike.state"].label)")
        let pip = app.buttons["spike.pip"]
        XCTAssertTrue(pip.waitForExistence(timeout: 10))
        pip.tap()
        sleep(3)
        print("SPIKE-TEST: after tap \(app.staticTexts["spike.pipstate"].label)")
        XCUIDevice.shared.press(.home)
        sleep(3)
        shot("pip-2-home-screen")
    }
}
