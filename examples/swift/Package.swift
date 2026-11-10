// swift-tools-version:5.9
// Notes: a SwiftUI app over fennecdb embedded in it, a file on the device.
// NotesKit holds the model and the views (macOS and iOS alike), NotesApp is
// the macOS app, notes-cli the same model from a terminal.
import Foundation
import PackageDescription

// FENNEC_LOCAL=1 builds against this repository's FennecDB (CI does), its
// XCFramework from integrations/swift/build-xcframework.sh; otherwise the
// release. A path dependency is named by its directory.
let local = ProcessInfo.processInfo.environment["FENNEC_LOCAL"] != nil
let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../..").standardized
let fennec: Package.Dependency =
    local
    ? .package(path: root.path)
    : .package(url: "https://github.com/fennec-db/fennecdb", from: "0.2.0")
let fennecPackage = local ? root.lastPathComponent : "fennecdb"

let package = Package(
    name: "Notes",
    platforms: [.macOS(.v12), .iOS(.v15)],
    products: [
        .library(name: "NotesKit", targets: ["NotesKit"]),
        .executable(name: "NotesApp", targets: ["NotesApp"]),
        .executable(name: "notes-cli", targets: ["notes-cli"]),
    ],
    dependencies: [fennec],
    targets: [
        .target(
            name: "NotesKit",
            dependencies: [.product(name: "FennecDB", package: fennecPackage)],
            resources: [.copy("schema.fennecql")]),
        .executableTarget(name: "NotesApp", dependencies: ["NotesKit"]),
        .executableTarget(name: "notes-cli", dependencies: ["NotesKit"]),
    ]
)
