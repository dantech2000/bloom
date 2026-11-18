// swift-tools-version: 6.0
// bloom-core's Swift face for the native iOS app: the generated bindings
// (Sources/BloomCore/bloom_ffi.swift) over BloomFFI.xcframework. Both are
// made by dev/ios-core; do not edit them by hand.
import PackageDescription

let package = Package(
    name: "BloomCore",
    // The package asks for less than the app (27.0), so a spike build for
    // an iOS 26 simulator can lower the app alone.
    platforms: [.iOS("26.0")],
    products: [
        .library(name: "BloomCore", targets: ["BloomCore"]),
    ],
    targets: [
        .binaryTarget(name: "bloom_ffiFFI", path: "BloomFFI.xcframework"),
        .target(
            name: "BloomCore",
            dependencies: ["bloom_ffiFFI"],
            // UniFFI's async glue is not Sendable yet (uniffi-rs #2448):
            // the bindings compile in Swift 5 mode; the app is Swift 6.
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
