import AppKit
import SwiftUI
import Foundation
import Testing
import RgitClient
@testable import RustGitKit

/// Renders the real views, backed by a real `rgit` and real repositories, into PNG files.
/// Off by default; run with RGIT_SNAPSHOT_DIR=/some/dir RGIT_BINARY=/path/to/rgit swift test --filter Snapshot
@MainActor
@Suite("Snapshots", .serialized)
struct SnapshotTests {
    func render<V: View>(_ view: V, name: String, dir: URL, size: CGSize = CGSize(width: 1100, height: 700)) throws {
        let host = NSHostingView(rootView: view.background(Color(nsColor: .windowBackgroundColor)).environment(\.colorScheme, .light))
        host.frame = CGRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .aqua)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        RunLoop.current.run(until: Date().addingTimeInterval(0.6))
        host.layoutSubtreeIfNeeded()
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        let png = try #require(rep.representation(using: .png, properties: [:]))
        try png.write(to: dir.appendingPathComponent("\(name).png"))
    }

    func run(_ exe: URL, _ dir: URL, _ args: [String]) throws {
        let p = Process(); p.executableURL = exe; p.arguments = ["-C", dir.path] + args
        p.standardOutput = FileHandle.nullDevice; p.standardError = FileHandle.nullDevice
        try p.run(); p.waitUntilExit()
    }

    @Test func renderChangesReview() async throws {
        guard let out = ProcessInfo.processInfo.environment["RGIT_SNAPSHOT_DIR"],
              let exePath = ProcessInfo.processInfo.environment["RGIT_BINARY"] else { return }
        let dir = URL(fileURLWithPath: out)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        _ = NSApplication.shared
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("changes-review-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let client = RgitClient(executable: URL(fileURLWithPath: exePath), directory: tmp)
        _ = try await client.initProject()
        try await client.setConfig("user.name", "Demo")
        try await client.setConfig("user.email", "demo@example.com")
        let paths = ["artifacts/1.0.0/SHA256SUMS.txt", "macos/Sources/PocketCore/ScreenSessions.swift",
                     "macos/README.md", "docs/releases/1.0.0/macos-swift-reliability-install-2026102002.json"]
        let before = (1...18).map { String(repeating: String($0 % 10), count: 64) + "  macOS/SwiftCandidate/CodePocket-macos-arm64-1.0.\($0).dmg" }.joined(separator: "\n") + "\n"
        for path in paths {
            let url = tmp.appendingPathComponent(path)
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try before.write(to: url, atomically: true, encoding: .utf8)
        }
        _ = try await client.commit(message: "Initial version")
        for path in paths {
            try before.replacingOccurrences(of: String(repeating: "5", count: 64), with: String(repeating: "a", count: 64))
                .write(to: tmp.appendingPathComponent(path), atomically: true, encoding: .utf8)
        }
        try "# Next steps\nReview changes\n".write(to: tmp.appendingPathComponent("TODO.md"), atomically: true, encoding: .utf8)
        let model = AppModel(settings: AppSettings(defaults: UserDefaults(suiteName: "changes-review-\(UUID().uuidString)")!), hostsDirectory: tmp.appendingPathComponent("hosts"))
        await model.start()
        #expect(await model.open(tmp))
        let p = try #require(model.project)
        p.setFocus(paths[0])
        for _ in 0..<200 where p.focusDiff == nil { try await Task.sleep(for: .milliseconds(20)) }
        #expect(p.focusDiff != nil)
        model.section = .changes
        try render(RootView().environment(model), name: "changes-review", dir: dir, size: CGSize(width: 1240, height: 820))
        try render(RootView().environment(model), name: "changes-review-narrow", dir: dir, size: CGSize(width: 960, height: 650))
    }

    @Test func renderAllScreens() async throws {
        guard let out = ProcessInfo.processInfo.environment["RGIT_SNAPSHOT_DIR"], let exePath = ProcessInfo.processInfo.environment["RGIT_BINARY"] else { return }
        let outDir = URL(fileURLWithPath: out)
        try FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
        _ = NSApplication.shared
        let exe = URL(fileURLWithPath: exePath)
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("snap-\(UUID().uuidString)")
        let proj = tmp.appendingPathComponent("Demo Project")
        try FileManager.default.createDirectory(at: proj, withIntermediateDirectories: true)

        let c = RgitClient(executable: exe, directory: proj)
        _ = try await c.initProject()
        try await c.setConfig("user.name", "Ann Lee"); try await c.setConfig("user.email", "ann@example.com")
        try "# Demo\nhello\n".write(to: proj.appendingPathComponent("README.md"), atomically: true, encoding: .utf8)
        try "line1\nline2\nline3\n".write(to: proj.appendingPathComponent("notes.txt"), atomically: true, encoding: .utf8)
        _ = try await c.commit(message: "Add README and notes")
        _ = try await c.switchBranch("feature", create: true)
        try "line1\nline2 (feature)\nline3\n".write(to: proj.appendingPathComponent("notes.txt"), atomically: true, encoding: .utf8)
        _ = try await c.commit(message: "Feature edit of notes\n\nLonger body.")
        _ = try await c.switchBranch("main")
        try "line1\nline2 (main)\nline3\nline4\n".write(to: proj.appendingPathComponent("notes.txt"), atomically: true, encoding: .utf8)
        _ = try await c.commit(message: "Main edit of notes")
        // host + remote
        let host = tmp.appendingPathComponent("h.rgit")
        let serve = try await ServeSession.start(executable: exe, directory: host, options: .init(listen: "127.0.0.1:0", initialize: true, insecureOpenAccess: true))
        try await c.addRemote(name: "origin", url: "tcp://\(serve.address)")
        _ = try await c.push()
        // pending changes
        try "# Demo\nhello\nworld\n".write(to: proj.appendingPathComponent("README.md"), atomically: true, encoding: .utf8)
        try "new file\n".write(to: proj.appendingPathComponent("todo.txt"), atomically: true, encoding: .utf8)

        let model = AppModel(settings: AppSettings(defaults: UserDefaults(suiteName: "snap-\(UUID().uuidString)")!), hostsDirectory: tmp.appendingPathComponent("hosts"))
        await model.start()   // finds rgit via RGIT_BINARY
        #expect(await model.open(proj))
        let p = try #require(model.project)
        func idle() async { for _ in 0..<200 where p.activity != nil { try? await Task.sleep(for: .milliseconds(50)) } }
        await idle()
        await p.fetch()
        await p.refreshStorage()
        model.section = .changes

        func shot(_ name: String, _ v: some View) throws {
            try render(v.environment(model).frame(width: 1100, height: 700), name: name, dir: outDir)
        }
        try shot("root-changes", RootView())
        model.section = .history; try shot("root-history", RootView())
        model.section = .sharing; try shot("root-sharing", RootView())
        await model.hostProject(p, startPort: 19_900 + Int.random(in: 0..<90), allowLAN: false)
        if let e = model.hosts.entry(forProject: proj.path) { await model.hosts.setAllowLAN(e.id, true) }
        try shot("root-sharing-hosted-lan", RootView())
        try render(SharingView(project: p).environment(model), name: "sharing-full", dir: outDir, size: CGSize(width: 1000, height: 2000))
        await model.hosts.stopAll()
        model.section = .storage; await idle(); await p.verify(); try shot("root-storage", RootView())
        model.section = .projects; try shot("root-projects", RootView())
        // conflicts
        await idle()
        // Committed work is needed for a merge: set the pending edits aside first.
        _ = await p.commit(message: "pending")
        await idle()
        #expect(await p.merge("feature"))
        #expect(p.status?.conflicts.count == 1)
        model.section = .changes; try shot("root-conflict", RootView())
        try render(ConflictsSheet(project: p).environment(model), name: "conflicts-sheet", dir: outDir, size: CGSize(width: 700, height: 420))
        await p.abortMerge()
        await serve.stop()
        try? FileManager.default.removeItem(at: tmp)
    }
}
