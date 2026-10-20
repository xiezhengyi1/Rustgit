import Foundation
import Testing
import RgitClient
@testable import RustGitKit

@Suite("Diff parsing")
struct DiffParserTests {
    @Test func splitsFilesAndKeepsHunkContentThatLooksLikeHeaders() {
        let text = """
        --- a/a.txt
        +++ b/a.txt
        @@ -1,3 +1,3 @@
         keep
        --- not a header, a removed line starting with dashes
        +++ not a header either
         tail
        Binary files a/bin.dat and b/bin.dat differ
        --- /dev/null
        +++ b/new dir/n.txt
        @@ -0,0 +1,2 @@
        +one
        +two
        \\ No newline at end of file

        """
        let files = DiffParser.parse(text)
        #expect(files.map(\.path) == ["a.txt", "bin.dat", "new dir/n.txt"])
        #expect(files[1].isBinary)
        #expect(files[0].lines.filter { $0.kind == .removed }.count == 1)
        #expect(files[0].lines.filter { $0.kind == .added }.count == 1)
        #expect(files[2].lines.filter { $0.kind == .added }.count == 2)
    }

    @Test func deletedFileUsesTheOldPath() {
        let files = DiffParser.parse("--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n")
        #expect(files.map(\.path) == ["gone.txt"])
    }

    @Test func emptyAndGarbageInput() {
        #expect(DiffParser.parse("").isEmpty)
        #expect(DiffParser.parse("hello\nworld").isEmpty)
    }

    @Test func previewNumbersTrackEachSideAcrossHunks() throws {
        let text = "--- a/a.txt\n+++ b/a.txt\n@@ -10,3 +20,3 @@\n keep\n--- removed content\n+++ added content\n tail\n@@ -40 +50,2 @@\n last\n+extra\n\\ No newline at end of file\n"
        let file = try #require(DiffParser.parse(text).first)
        let rows = NumberedDiffLine.make(file.lines)
        #expect(rows.map(\.old) == [nil, 10, 11, nil, 12, nil, 40, nil, nil])
        #expect(rows.map(\.new) == [nil, 20, nil, 21, 22, nil, 50, 51, nil])
        #expect(rows[2].text == "-- removed content")
        #expect(rows[3].text == "++ added content")
        #expect(rows.last?.kind == .meta)
    }

    @Test func previewNumbersNewAndDeletedFilesWithoutZeroCoordinates() throws {
        let text = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n--- a/old.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-one\n-two\n"
        let files = DiffParser.parse(text)
        let added = NumberedDiffLine.make(files[0].lines)
        let removed = NumberedDiffLine.make(files[1].lines)
        #expect(added.map(\.old) == [nil, nil, nil])
        #expect(added.map(\.new) == [nil, 1, 2])
        #expect(removed.map(\.old) == [nil, 1, 2])
        #expect(removed.map(\.new) == [nil, nil, nil])
    }
}

@Suite("Commit graph layout")
struct GraphTests {
    private func commit(_ id: String, _ parents: [String]) throws -> CommitInfo {
        let json = #"{"id":"\#(id)","parents":[\#(parents.map { "\"\($0)\"" }.joined(separator: ","))],"message":"m"}"#
        return try JSONDecoder().decode(CommitInfo.self, from: Data(json.utf8))
    }

    @Test func linearHistoryStaysInOneLane() throws {
        let rows = GraphLayout.rows(for: [try commit("c", ["b"]), try commit("b", ["a"]), try commit("a", [])])
        #expect(rows.map(\.lane) == [0, 0, 0])
    }

    @Test func mergeOpensASecondLaneAndItCloses() throws {
        // m merges b2 into b1; both descend from a.
        let rows = GraphLayout.rows(for: [try commit("m", ["b1", "b2"]), try commit("b1", ["a"]),
                                          try commit("b2", ["a"]), try commit("a", [])])
        #expect(rows[0].parentLanes.count == 2)
        #expect(rows[1].lane != rows[2].lane)
        #expect(rows[3].lane == 0)
    }
}

@Suite("Localization")
struct LocalizationTests {
    static let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

    static func strings(_ lang: String) throws -> [String: String] {
        let url = root.appendingPathComponent("Resources/\(lang).lproj/Localizable.strings")
        let data = try Data(contentsOf: url)
        return try #require(try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: String])
    }

    /// Every `L("…")` / `LF("…")` key used in the sources.
    static func usedKeys() throws -> Set<String> {
        let dir = root.appendingPathComponent("Sources/RustGitKit")
        var keys = Set<String>()
        let regex = try NSRegularExpression(pattern: #"\bLF?\(\s*"((?:[^"\\]|\\.)*)""#)
        for f in try FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil) where f.pathExtension == "swift" {
            let text = try String(contentsOf: f, encoding: .utf8)
            for m in regex.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                keys.insert(String(text[Range(m.range(at: 1), in: text)!]).replacingOccurrences(of: "\\\"", with: "\""))
            }
        }
        return keys
    }

    @Test func englishAndChineseCoverEveryKeyUsedInCode() throws {
        let used = try Self.usedKeys()
        #expect(used.count > 100)
        let en = try Self.strings("en"), zh = try Self.strings("zh-Hans")
        #expect(used.subtracting(en.keys).isEmpty, "missing in en: \(used.subtracting(en.keys).sorted())")
        #expect(used.subtracting(zh.keys).isEmpty, "missing in zh-Hans: \(used.subtracting(zh.keys).sorted())")
        #expect(Set(en.keys) == Set(zh.keys))
    }

    @Test func translationsKeepTheSameFormatSpecifiers() throws {
        let en = try Self.strings("en"), zh = try Self.strings("zh-Hans")
        let spec = try NSRegularExpression(pattern: #"%(\d+\$)?[@d]"#)
        func specs(_ s: String) -> [String] {
            spec.matches(in: s, range: NSRange(s.startIndex..., in: s)).map { m in
                let t = String(s[Range(m.range, in: s)!])
                return t.replacingOccurrences(of: #"\d+\$"#, with: "", options: .regularExpression)
            }.sorted()
        }
        for (k, v) in zh { #expect(specs(k) == specs(v), "format mismatch for: \(k)") }
        for (k, v) in en { #expect(k == v) }
    }
}

@Suite("Error presentation")
struct ErrorPresenterTests {
    @Test func everyEngineCodeHasAnActionableMessage() {
        let codes: [RgitErrorCode] = [.notARepository, .dirtyWorkingTree, .nothingToCommit, .mergeInProgress, .noMerge,
                                      .unresolvedConflicts, .branchNotFound, .branchExists, .invalidName, .unportablePaths,
                                      .refConflict, .unknownRevision, .hostUnreachable, .permissionDenied, .quotaExceeded,
                                      .outOfSpace, .invalid, .other, .unknown("future_code")]
        for c in codes {
            let e = ErrorPresenter.present(RgitError.engine(code: c, message: "engine words", paths: ["p"]))
            #expect(!e.title.isEmpty && !e.suggestion.isEmpty)
            #expect(e.detail == "engine words" && e.paths == ["p"])
        }
    }

    @Test func processLevelErrors() {
        #expect(!ErrorPresenter.present(RgitError.cancelled).title.isEmpty)
        #expect(ErrorPresenter.present(RgitError.incompatibleInterface(found: 2, expected: 1)).detail.contains("2"))
        #expect(!ErrorPresenter.present(RgitError.executableUnavailable("x")).suggestion.isEmpty)
    }
}
