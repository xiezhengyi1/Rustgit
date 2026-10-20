import Foundation
import Testing
import RgitClient
@testable import RustGitKit

@MainActor
@Suite("Recent project switching")
struct ProjectSwitchTests {
    @Test func remembersProjectsAndPreservesEachDraftAndSelection() async throws {
        guard let path = ProcessInfo.processInfo.environment["RGIT_BINARY"] else { return }
        let box = FileManager.default.temporaryDirectory.appendingPathComponent("switch-projects-\(UUID().uuidString)")
        let suite = "switch-projects-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer {
            try? FileManager.default.removeItem(at: box)
            defaults.removePersistentDomain(forName: suite)
        }
        // Same names in different locations must remain distinct projects.
        let first = box.appendingPathComponent("one/Project"), second = box.appendingPathComponent("two/Project")
        for url in [first, second] {
            try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
            _ = try await RgitClient(executable: URL(fileURLWithPath: path), directory: url).initProject()
            try "pending".write(to: url.appendingPathComponent("note.txt"), atomically: true, encoding: .utf8)
        }
        let app = AppModel(settings: AppSettings(defaults: defaults), hostsDirectory: box.appendingPathComponent("hosts"))
        await app.start()
        #expect(await app.open(first))
        let a = try #require(app.project)
        await a.refresh()
        a.commitMessageDraft = "Draft for the first project"
        a.setSelected("note.txt", false)
        #expect(await app.open(second))
        let b = try #require(app.project)
        b.commitMessageDraft = "Draft for the second project"
        #expect(await app.open(first))
        #expect(app.project === a)
        #expect(a.commitMessageDraft == "Draft for the first project")
        #expect(!a.isSelected("note.txt"))
        #expect(b.commitMessageDraft == "Draft for the second project")
        let saved = AppSettings(defaults: defaults)
        #expect(saved.projects.map(\.path) == [first.path, second.path])
        #expect(saved.lastProject == first.path)
        // Failed switches must leave the current project and its draft in place.
        try FileManager.default.removeItem(at: second)
        #expect(await !app.open(second))
        #expect(app.project === a)
        #expect(app.banner != nil)
        #expect(app.openingProjectPath == nil)
        #expect(app.settings.lastProject == first.path)
        app.close()
        #expect(app.settings.projects.count == 2)
        #expect(app.settings.lastProject == nil)
    }
}
