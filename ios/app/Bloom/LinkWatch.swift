import Foundation
import Network

/// The network path, for the link speed test of "Auto" quality (spec §16.8,
/// the Mac's `adaptive_socket` and its sleep check): a change of the path (a
/// new Wi-Fi, a return from cellular, a reconnect) makes the last
/// measurement stale, so the link is measured again as soon as nothing
/// plays. No test runs on an expensive or constrained path (cellular,
/// Low Data Mode); there the measurement is dropped and Auto plays the file
/// as it is, unless stalls step it down.
final class LinkWatch {
    static let shared = LinkWatch()

    /// The path's interfaces and flags, to tell a new path from the same.
    private var signature: String?
    private(set) var expensive = false
    private(set) var constrained = false
    /// The current path; nil until the monitor first reports. Downloads
    /// read it to tell a transfer that waits for a usable path from a stall.
    private(set) var path: NWPath?
    /// The phone streams over cellular (or a hotspot that the system marks
    /// as expensive): the cellular limit applies (spec Q11).
    var onCellular: Bool {
        guard let path else { return false }
        return path.usesInterfaceType(.cellular) || path.isExpensive
    }
    /// A measurement waits for a free link (nothing plays) or a known path.
    private var wanted = false

    private init() {
        // The shared watch lives as long as the app.
        Task { [weak self] in
            for await path in NWPathMonitor() { self?.update(path) }
        }
    }

    /// Whether a test download may run now; when not, one is noted and
    /// runs later (`measureIfWanted`) unless the path forbids it.
    func allows() -> Bool {
        guard signature != nil, LinkWatch.free else {
            wanted = true
            return false
        }
        return LinkWatch.mayMeasure(expensive: expensive, constrained: constrained)
    }

    /// No test on cellular or in Low Data Mode (spec §16.8).
    static func mayMeasure(expensive: Bool, constrained: Bool) -> Bool {
        !expensive && !constrained
    }

    /// The link is free when nothing plays: a test download beside a video
    /// measures the video too (the Mac waits the same way).
    private static var free: Bool {
        !(AppModel.current?.player?.isPlaying ?? false)
    }

    /// A noted measurement runs now if the link is free (the player paused
    /// or closed, or the path became known).
    func measureIfWanted() {
        guard wanted, signature != nil, LinkWatch.free else { return }
        wanted = false
        guard LinkWatch.mayMeasure(expensive: expensive, constrained: constrained) else { return }
        AppModel.current?.measureLink()
    }

    private func update(_ path: NWPath) {
        self.path = path
        expensive = path.isExpensive
        constrained = path.isConstrained
        guard path.status == .satisfied else { return }
        let new = path.availableInterfaces.map { "\($0.type)/\($0.name)" }.joined(separator: ",")
            + (path.isExpensive ? " expensive" : "") + (path.isConstrained ? " constrained" : "")
        let old = signature
        signature = new
        if let old, old != new {
            NSLog("LINK: the network path changed (%@ → %@)", old, new)
            // A measurement of another path says nothing of this one.
            AppModel.current?.linkBps = nil
            AppModel.current?.autoCap = nil
            // The cap of stalls belongs to the old path too.
            PlayerModel.stalls.forget()
            // A stream over the cellular limit loads again under it.
            AppModel.current?.player?.pathChanged()
            if LinkWatch.mayMeasure(expensive: path.isExpensive, constrained: path.isConstrained) {
                wanted = true
            } else {
                NSLog("LINK: no test on this path (cellular or Low Data Mode)")
                wanted = false
            }
        }
        measureIfWanted()
    }
}
