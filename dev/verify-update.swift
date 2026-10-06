// Checks an Ed25519 signature of a Sparkle update archive against a public
// key, with CryptoKit only (no Keychain, no private key):
//   swift dev/verify-update.swift <public key, base64> <file> <signature, base64>
// Exit status 0: the signature is valid for the file and the key.
import CryptoKit
import Foundation

let args = CommandLine.arguments
guard args.count == 4 else {
    FileHandle.standardError.write(Data("usage: verify-update <public key> <file> <signature>\n".utf8))
    exit(2)
}
guard let key = Data(base64Encoded: args[1]), key.count == 32,
      let signature = Data(base64Encoded: args[3]), signature.count == 64,
      let file = FileManager.default.contents(atPath: args[2]),
      let publicKey = try? Curve25519.Signing.PublicKey(rawRepresentation: key)
else {
    FileHandle.standardError.write(Data("verify-update: bad key, signature or file\n".utf8))
    exit(2)
}
exit(publicKey.isValidSignature(signature, for: file) ? 0 : 1)
