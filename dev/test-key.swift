// Makes a throwaway Ed25519 key pair for a test of dev/release that must
// not touch the Keychain: the private key (the 32 byte seed, base64, the
// format of `generate_appcast --ed-key-file`) goes into the file named by
// the first argument with mode 600, and the public key (base64, the form
// of SUPublicEDKey) is printed.
//   swift dev/test-key.swift <private key file>
import CryptoKit
import Foundation

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(Data("usage: test-key <private key file>\n".utf8))
    exit(2)
}
let key = Curve25519.Signing.PrivateKey()
let path = CommandLine.arguments[1]
guard FileManager.default.createFile(
    atPath: path, contents: Data((key.rawRepresentation.base64EncodedString() + "\n").utf8),
    attributes: [.posixPermissions: 0o600])
else {
    FileHandle.standardError.write(Data("test-key: cannot write \(path)\n".utf8))
    exit(1)
}
print(key.publicKey.rawRepresentation.base64EncodedString())
