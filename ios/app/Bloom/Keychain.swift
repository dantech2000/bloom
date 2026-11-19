import BloomCore
import Foundation
import Security

/// The Keychain as core's `SecretStore`. Called on Rust threads, so it is
/// not isolated to the main actor and waits for nothing.
nonisolated final class Keychain: SecretStore, Sendable {
    private let service = "dev.bloom.ios.next"

    private func base(_ key: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
    }

    func get(key: String) -> String? {
        var query = base(key)
        query[kSecReturnData as String] = true
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    func set(key: String, value: String) {
        remove(key: key)
        var add = base(key)
        add[kSecValueData as String] = Data(value.utf8)
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(add as CFDictionary, nil)
    }

    /// Deletes the item (a sign-out, a forgotten profile).
    func remove(key: String) {
        SecItemDelete(base(key) as CFDictionary)
    }
}
