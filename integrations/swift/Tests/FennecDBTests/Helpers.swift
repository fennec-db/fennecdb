// What the tests need of Foundation, in a file of its own: a file that
// imports both Foundation and Testing imports their overlay too
// (_Testing_Foundation), which the Command Line Tools ship without its
// module -- `swift test` without Xcode stopped at it.
import Foundation

@testable import FennecDB

/// A file of its own in a directory of its own, as an app's documents
/// directory holds one.
func scratch(_ name: String) throws -> String {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("fennecdb-swift-\(ProcessInfo.processInfo.processIdentifier)-\(name)")
    try? FileManager.default.removeItem(at: dir)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir.appendingPathComponent("app.fennec").path
}

/// `Fennec.open(_ url:)` over a path.
func open(url path: String, options: Fennec.Options) async throws -> Fennec {
    try await Fennec.open(URL(fileURLWithPath: path), options: options)
}

/// The text of integrations/builder-golden.json: `FENNEC_GOLDEN`, or the
/// repository's, found from this file.
func goldenText(_ file: String = #filePath) -> String {
    let path = ProcessInfo.processInfo.environment["FENNEC_GOLDEN"]
        ?? URL(fileURLWithPath: file)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().appendingPathComponent("builder-golden.json").path
    return (try? String(contentsOfFile: path, encoding: .utf8)) ?? "[]"
}

/// A `$date` of the golden file as a caller hands it over: a `Date`.
func date(_ text: String) -> Value {
    let f = ISO8601DateFormatter()
    f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    return try! f.date(from: text)!.fennecValue()
}

/// The ISO text `Date`s since the epoch are written as.
func isoText(seconds: Double) -> String { iso(Date(timeIntervalSince1970: seconds)) }

/// Berlin's 03:00 on 1 January, 02:00 UTC -- this year's, or next -- as
/// `iso` writes it.
func newYearsText() -> String {
    var utc = Calendar(identifier: .gregorian)
    utc.timeZone = TimeZone(identifier: "UTC")!
    let now = Date()
    let first = utc.date(from: DateComponents(year: utc.component(.year, from: now), month: 1, day: 1, hour: 2))!
    return iso(now < first ? first : utc.date(byAdding: .year, value: 1, to: first)!)
}
