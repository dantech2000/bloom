import Foundation
import Observation

/// The names the user gave servers on this device, by the server's id. A
/// server names itself (often its machine: "2c7c1543f65e"); a name given
/// here shows instead, on this device only.
@Observable
final class ServerNames {
    static let shared = ServerNames()
    private static let key = "serverNames"

    private(set) var names: [String: String] = UserDefaults.standard.dictionary(forKey: key) as? [String: String] ?? [:]

    /// The name to show for a server: the one given here, else its own.
    func shown(id: String, reported: String) -> String {
        names[id] ?? reported
    }

    func shown(_ account: Account) -> String {
        shown(id: account.serverId, reported: account.serverName)
    }

    /// An empty name goes back to the server's own.
    func set(_ name: String, for id: String) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        names[id] = name.isEmpty ? nil : name
        UserDefaults.standard.set(names, forKey: Self.key)
    }
}
