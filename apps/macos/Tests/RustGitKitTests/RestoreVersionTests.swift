import Foundation
import Testing
import RgitClient
@testable import RustGitKit

@MainActor
@Suite("Restore the selected version")
struct RestoreVersionTests {
    @Test func rootVersionRestoreUsesRestoreAndProtectsUnsavedFiles() async throws {
        guard let path = ProcessInfo.processInfo.environment["RGIT_BINARY"] else { return }
        let box = FileManager.default.temporaryDirectory.appendingPathComponent("restore-version-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: box) }
        let root = box.appendingPathComponent("project")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let client = RgitClient(executable: URL(fileURLWithPath: path), directory: root,
            environment: ["RGIT_INSECURE_FILE_KEYS": "1", "RGIT_KEY_DIR": box.appendingPathComponent("keys").path])
        _ = try await client.initProject()
        try await client.setConfig("user.name", "Restore tester")
        try "original".write(to: root.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        _ = try await client.commit(message: "first")
        let first = try #require(try await client.log().first)
        #expect(first.parents.isEmpty)
        try "newer".write(to: root.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        _ = try await client.commit(message: "second")
        let head = try await client.status().head
        try "keep local".write(to: root.appendingPathComponent("local.txt"), atomically: true, encoding: .utf8)
        let model = ProjectModel(url: root, client: client)
        await model.refresh()
        await model.restoreVersion(first)
        #expect(model.error == nil)
        #expect(try String(contentsOf: root.appendingPathComponent("a.txt"), encoding: .utf8) == "original")
        #expect(try String(contentsOf: root.appendingPathComponent("local.txt"), encoding: .utf8) == "keep local")
        #expect(try await client.status().head == head)
        try "unsaved".write(to: root.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        await model.restoreVersion(first)
        #expect(model.error?.code == .dirtyWorkingTree)
        #expect(model.errorAction != nil)
        #expect(try String(contentsOf: root.appendingPathComponent("a.txt"), encoding: .utf8) == "unsaved")
        await model.errorAction?.run()
        #expect(model.error == nil)
        #expect(try await client.shelves().recovery.isEmpty == false)
    }
}
