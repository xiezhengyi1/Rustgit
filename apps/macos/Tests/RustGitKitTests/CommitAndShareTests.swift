import Foundation
import Testing
import RgitClient
@testable import RustGitKit

@MainActor
@Suite("Commit and share")
struct CommitAndShareTests {
    @Test func publishesSelectionAndRetriesFailedSendWithoutAnotherCommit() async throws {
        guard let binary = ProcessInfo.processInfo.environment["RGIT_BINARY"] else { return }
        let box = FileManager.default.temporaryDirectory.appendingPathComponent("commit-share-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: box) }
        let folder = box.appendingPathComponent("project"), host = box.appendingPathComponent("host")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let exe = URL(fileURLWithPath: binary)
        let c = RgitClient(executable: exe, directory: folder, environment: ["RGIT_INSECURE_FILE_KEYS": "1", "RGIT_KEY_DIR": box.appendingPathComponent("keys").path])
        _ = try await c.initProject()
        try await c.setConfig("user.name", "Test")
        try "one".write(to: folder.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        try "leave uncommitted".write(to: folder.appendingPathComponent("b.txt"), atomically: true, encoding: .utf8)
        var server = try await ServeSession.start(executable: exe, directory: host, options: .init(listen: "127.0.0.1:0", initialize: true, insecureOpenAccess: true))
        do {
            let address = server.address
            // A single remote need not be named origin.
            try await c.addRemote(name: "team", url: "tcp://\(address)")
            let p = ProjectModel(url: folder, client: c)
            await p.refresh()
            p.setSelected("b.txt", false)
            p.commitMessageDraft = "first"
            #expect(await p.commitAndShare(message: "first"))
            #expect(p.error == nil)
            #expect(!p.hasPendingShare && !p.commitAndShareInProgress)
            #expect(p.commitMessageDraft.isEmpty)
            let shared = RgitClient(executable: exe, directory: host)
            #expect(try await shared.log().first?.id == p.status?.head)
            #expect(p.status?.changes.map(\.path) == ["b.txt"])
            #expect(p.lastPushedHead == p.status?.head)

            await server.stop()
            try "two".write(to: folder.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
            await p.refresh()
            p.commitMessageDraft = "second"
            #expect(await p.commitAndShare(message: "second"))
            #expect(p.hasPendingShare)
            #expect(p.commitMessageDraft.isEmpty)
            #expect(p.error?.title == L("Committed locally, but not shared"))
            #expect(try await c.log().count == 2)
            #expect(try await shared.log().count == 1)

            server = try await ServeSession.start(executable: exe, directory: host, options: .init(listen: address, insecureOpenAccess: true))
            let retry = try #require(p.errorAction)
            await retry.run()
            #expect(p.error == nil && !p.hasPendingShare)
            #expect(try await c.log().count == 2)
            #expect(try await shared.log().first?.id == p.status?.head)
            #expect(await !p.commitAndShare(message: "nothing changed"))
            #expect(p.error?.code == .nothingToCommit)
            #expect(try await shared.log().count == 2)
        } catch { await server.stop(); throw error }
        await server.stop()
    }

    @Test func projectWithoutRemoteStillCommitsLocally() async throws {
        guard let binary = ProcessInfo.processInfo.environment["RGIT_BINARY"] else { return }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("local-commit-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let c = RgitClient(executable: URL(fileURLWithPath: binary), directory: folder)
        _ = try await c.initProject()
        try "local".write(to: folder.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        let p = ProjectModel(url: folder, client: c)
        await p.refresh()
        #expect(await p.commitAndShare(message: "local only"))
        #expect(p.error == nil && !p.hasPendingShare)
        #expect(p.status?.head != nil)
        #expect(p.lastPushedHead == nil)
    }
}
