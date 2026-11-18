import Foundation
import XCTest

/// The tests' hands on dev/mock-jellyfin: its address, a request as
/// another client makes it, and the mock's own controls (`/mock/...`, see
/// the mock's docstring). dev/ios-next starts the mock on BLOOM_MOCK_PORT
/// and gives its address as BLOOM_SERVER.
nonisolated enum MockServer {
    /// The mock's address when dev/ios-next names none.
    static let fallback = "http://127.0.0.1:8099"
    /// The mock's session token: a strict mock (BLOOM_MOCK_STRICT) answers
    /// 401 to a request without it.
    static let token = "mock-token"

    static var base: String { ProcessInfo.processInfo.environment["BLOOM_SERVER"] ?? fallback }
    static var host: String { base.replacingOccurrences(of: "http://", with: "") }
    static var port: UInt16 { UInt16(host.split(separator: ":").last.flatMap { Int($0) } ?? 8099) }

    /// The environment of an app under test: the mock, muted, no reports,
    /// the core's fake Chromecast (never a real one, CLAUDE.md).
    static func environment(controlsStay: Bool = true) -> [String: String] {
        var env = ["BLOOM_SERVER": base, "BLOOM_MUTED": "1", "BLOOM_NO_REPORT": "1", "BLOOM_CHROMECAST_MOCK": "1"]
        if controlsStay { env["BLOOM_CONTROLS_STAY"] = "1" }
        return env
    }

    /// A request with the session's token; the status (0 when the mock did
    /// not answer) and the body.
    @discardableResult
    static func request(_ method: String, _ path: String, body: Data? = nil) -> (status: Int, body: Data?) {
        guard let url = URL(string: base + path) else { return (0, nil) }
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.httpMethod = method
        request.httpBody = body
        request.setValue(token, forHTTPHeaderField: "X-Emby-Token")
        let done = DispatchSemaphore(value: 0)
        nonisolated(unsafe) var answer: (Int, Data?) = (0, nil)
        URLSession.shared.dataTask(with: request) { data, response, _ in
            answer = ((response as? HTTPURLResponse)?.statusCode ?? 0, data)
            done.signal()
        }.resume()
        done.wait()
        return answer
    }

    /// The JSON of a GET, or nil.
    static func json(_ path: String) -> Any? {
        guard let data = request("GET", path).body else { return nil }
        return try? JSONSerialization.jsonObject(with: data)
    }

    /// Back to the mock's state at its start: the library, the user's
    /// settings, playlists, favourites, hidden titles, bookmarks. Each test
    /// starts from there, whatever the test before it changed.
    static func reset() {
        let status = request("POST", "/mock/reset").status
        // A test on another server (SHOT_SYNCPLAY) may run without the mock.
        if status != 204 { print("SHOTS: the mock did not reset (status \(status))") }
    }

    /// The play commands the mock took for other sessions (Play on).
    static func remoteCommands() -> [[String: Any]] {
        json("/mock/remote") as? [[String: Any]] ?? []
    }

    /// A count of the mock: "LibraryPages", "SearchesAsked", "SearchesAnswered".
    static func count(_ name: String) -> Int {
        (json("/mock/counts") as? [String: Int])?[name] ?? -1
    }

    /// Holds every search answer of the mock (true) or lets them go.
    static func holdSearches(_ hold: Bool) {
        request("POST", "/mock/search?hold=\(hold ? 1 : 0)")
    }

    /// Waits until `condition` holds, polling; false at the deadline. A
    /// paused runner stretches the wait and the work alike, so the deadline
    /// is generous (CLAUDE.md, "Timing in tests").
    static func wait(timeout: TimeInterval, _ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            Thread.sleep(forTimeInterval: 0.25)
        }
        return condition()
    }
}
