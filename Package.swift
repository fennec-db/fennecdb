// swift-tools-version:5.9
//
// FennecDB for Swift: fennecdb embedded in a macOS or iOS app, a file on the
// device. The package lives in integrations/swift; this manifest is at the
// repository's root because SwiftPM fetches a package by its repository
// and tag -- `.package(url: "https://github.com/fennec-db/fennecdb", from: "X.Y.Z")`.
//
// The engine is the native library (crates/fennec-ffi) as an XCFramework.
// Built here by integrations/swift/build-xcframework.sh, it is taken from
// integrations/swift/build/; otherwise from the release the tag names, by
// its URL and checksum, which the release workflow writes below
// (integrations/swift/set-binary.sh) before the tag is made.
import Foundation
import PackageDescription

let local = "integrations/swift/build/FennecFFI.xcframework"
let here = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
let built = FileManager.default.fileExists(atPath: here.appendingPathComponent(local).path)

let release = "0.2.0"
let checksum = "bfdb8464a657c8ad9dafcfcb152d196ef90731abd22eadbf200c638b8d51b232"

let ffi: Target =
    built
    ? .binaryTarget(name: "FennecFFI", path: local)
    : .binaryTarget(
        name: "FennecFFI",
        url: "https://github.com/fennec-db/fennecdb/releases/download/v\(release)/FennecFFI.xcframework.zip",
        checksum: checksum)

let package = Package(
    name: "FennecDB",
    platforms: [.macOS(.v12), .iOS(.v15)],
    products: [
        .library(name: "FennecDB", targets: ["FennecDB"])
    ],
    targets: [
        ffi,
        .target(
            name: "FennecDB",
            dependencies: ["FennecFFI"],
            path: "integrations/swift/Sources/FennecDB"),
        .testTarget(
            name: "FennecDBTests",
            dependencies: ["FennecDB"],
            path: "integrations/swift/Tests/FennecDBTests"),
    ]
)
