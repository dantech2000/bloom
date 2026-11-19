import Darwin
import Foundation

/// Jellyfin servers on the local network. A server answers the UDP
/// broadcast "who is JellyfinServer?" on port 7359 with its address, id
/// and name (as Jellyfin Web's and the other apps' discovery do).
///
/// iOS lets an app send a broadcast only with the multicast entitlement
/// (com.apple.developer.networking.multicast); without it the send fails
/// and the search finds nothing, which the connect screen shows as such.
/// The simulator sends it either way. `BLOOM_DISCOVERY_TARGETS`
/// ("127.0.0.1:8133,…") adds addresses to ask directly, for a test against
/// dev/mock-jellyfin.
enum ServerDiscovery {
    nonisolated struct Found: Hashable, Sendable, Decodable {
        let address: String
        let id: String
        let name: String

        enum CodingKeys: String, CodingKey {
            case address = "Address", id = "Id", name = "Name"
        }
    }

    /// Asks once and collects the answers for `seconds`.
    static func search(seconds: Double = 2) async -> [Found] {
        let extra = ProcessInfo.processInfo.environment["BLOOM_DISCOVERY_TARGETS"] ?? ""
        return await Task.detached(priority: .utility) { ask(seconds: seconds, extra: extra) }.value
    }

    private nonisolated static func ask(seconds: Double, extra: String) -> [Found] {
        let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
        guard fd >= 0 else { return [] }
        defer { close(fd) }
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_BROADCAST, &on, socklen_t(MemoryLayout<Int32>.size))
        // A short wait per read, so the loop sees its deadline.
        var wait = timeval(tv_sec: 0, tv_usec: 250_000)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &wait, socklen_t(MemoryLayout<timeval>.size))

        var targets: [(String, UInt16)] = [("255.255.255.255", 7359)]
        for entry in extra.split(separator: ",") {
            let parts = entry.split(separator: ":")
            if parts.count == 2, let port = UInt16(parts[1]) { targets.append((String(parts[0]), port)) }
        }
        let message = Array("who is JellyfinServer?".utf8)
        var sent = 0
        for (host, port) in targets {
            var address = sockaddr_in()
            address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            address.sin_family = sa_family_t(AF_INET)
            address.sin_port = port.bigEndian
            guard inet_pton(AF_INET, host, &address.sin_addr) == 1 else { continue }
            let result = withUnsafePointer(to: &address) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    sendto(fd, message, message.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            if result >= 0 { sent += 1 }
        }
        guard sent > 0 else { return [] }

        var found: [Found] = []
        let deadline = Date.now.addingTimeInterval(seconds)
        var buffer = [UInt8](repeating: 0, count: 4096)
        while Date.now < deadline {
            let count = recv(fd, &buffer, buffer.count, 0)
            guard count > 0 else { continue }
            if let server = try? JSONDecoder().decode(Found.self, from: Data(buffer[0..<count])),
               !found.contains(where: { $0.id == server.id }) {
                found.append(server)
            }
        }
        return found.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }
}
