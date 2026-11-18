// Spike S1 of specs/ios-native/plan.md: the bridge to bloom-core. A screen
// that exercises each rule of spec §5.1; BLOOM_SPIKE=1 shows it.
import BloomCore
import SwiftUI

@Observable
final class SpikeModel {
    var log: [String] = []
    var ticks = 0
    var busy = false
    let keychain = Keychain()
    var server: Server?

    var serverURL: String {
        ProcessInfo.processInfo.environment["BLOOM_SERVER"] ?? "http://127.0.0.1:8099"
    }

    func note(_ line: String) {
        log.append(line)
        print("SPIKE: \(line)")
    }

    func connectAndLoad() async {
        busy = true
        defer { busy = false }
        let server = Server(url: serverURL, deviceId: "spike-device", secrets: keychain)
        self.server = server
        do {
            let info = try await server.publicInfo()
            note("server: \(info.name) (\(info.id))")
            let me = try await server.signIn(username: "tester", password: "")
            note("signed in: \(me.userName); token kept: \(keychain.get(key: "token.\(me.userId)") != nil)")
            let home = try await server.home()
            note("home: hero \(home.hero.count), resume \(home.resume.count), next up \(home.nextUp.count), rows \(home.latest.map { "\($0.title) \($0.items.count)" })")
        } catch {
            note("error: \(error)")
        }
    }

    /// 60 calls that each hold a pool thread for 2 s, while a subscription
    /// ticks every 50 ms. With the pool of 8 the calls take about 16 s;
    /// the ticks must keep coming the whole time (Swift's pool not blocked).
    func stress() async {
        guard let server else { note("connect first"); return }
        busy = true
        defer { busy = false }
        ticks = 0
        let ticker = server.ticker(everyMs: 50, count: 400)
        let ticking = Task {
            while let _ = await ticker.next() { ticks += 1 }
        }
        let started = ContinuousClock.now
        await withTaskGroup(of: Void.self) { group in
            for _ in 0..<60 {
                group.addTask { _ = try? await server.slowEcho(ms: 2000) }
            }
        }
        let elapsed = ContinuousClock.now - started
        let ticksDuring = ticks
        ticker.close()
        _ = await ticking.value
        note("60 slow calls in \(elapsed); ticks during them: \(ticksDuring) (expected about \(Int(elapsed.components.seconds) * 20))")
    }

    func panics() async {
        guard let server else { note("connect first"); return }
        do {
            try await server.panicInACall()
            note("panic in a call: no error?")
        } catch {
            note("panic in a call → \(error)")
        }
        panicOnACoreThread()
        note("panic on a core thread → last_panic: \(lastPanic() ?? "none")")
        let echo = try? await server.slowEcho(ms: 10)
        note("pool still works after the panics: \(echo.map(String.init) ?? "no")")
    }
}

struct SpikeView: View {
    @State private var model = SpikeModel()

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Button("Connect, sign in, load Home") { Task { await model.connectAndLoad() } }
                    Button("Stress: 60 slow calls + a stream") { Task { await model.stress() } }
                    Button("Panics") { Task { await model.panics() } }
                }
                .disabled(model.busy)
                Section("Ticks") { Text("\(model.ticks)").monospacedDigit() }
                Section("Log") {
                    ForEach(Array(model.log.enumerated()), id: \.offset) { _, line in
                        Text(line).font(.footnote.monospaced())
                    }
                }
            }
            .navigationTitle("Bridge spike")
        }
        .task {
            // BLOOM_SPIKE_AUTORUN=1 runs the three checks at launch, for a
            // run with no taps (`simctl launch --console`).
            guard ProcessInfo.processInfo.environment["BLOOM_SPIKE_AUTORUN"] == "1" else { return }
            await model.connectAndLoad()
            await model.stress()
            await model.panics()
            model.note("SPIKE DONE")
        }
    }
}
