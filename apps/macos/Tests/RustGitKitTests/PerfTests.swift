import AppKit
import SwiftUI
import Foundation
import Testing
import RgitClient
@testable import RustGitKit

/// Large-project measurements. Off by default:
///   RGIT_PERF_PROJECT=/path/to/30k-file-project RGIT_BINARY=... swift test --filter Perf
@MainActor
@Suite("Perf", .serialized)
struct PerfTests {
    func ms(_ d: Duration) -> Double { Double(d.components.seconds) * 1000 + Double(d.components.attoseconds) / 1e15 }
    func report(_ s: String) {
        print("PERF " + s)
        if let f = ProcessInfo.processInfo.environment["RGIT_PERF_LOG"] {
            if let h = FileHandle(forWritingAtPath: f) { h.seekToEndOfFile(); h.write(Data((s + "\n").utf8)); try? h.close() }
            else { try? (s + "\n").write(toFile: f, atomically: true, encoding: .utf8) }
        }
    }

    func renderTime<V: View>(_ view: V, size: CGSize = CGSize(width: 1100, height: 700)) -> Double {
        let clock = ContinuousClock()
        let start = clock.now
        let host = NSHostingView(rootView: view)
        host.frame = CGRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds)!
        host.cacheDisplay(in: host.bounds, to: rep)
        return ms(start.duration(to: clock.now))
    }

    @Test func changesScreenOnLargeProject() async throws {
        guard let path = ProcessInfo.processInfo.environment["RGIT_PERF_PROJECT"] else { return }
        _ = NSApplication.shared
        let clock = ContinuousClock()
        let model = AppModel()
        await model.start()
        let t0 = clock.now
        #expect(await model.open(URL(fileURLWithPath: path)))
        let p = try #require(model.project)
        // open() returns after status succeeded; wait for the initial refresh to settle.
        for _ in 0..<600 where (p.status?.changes.isEmpty ?? true) || p.activity != nil { try await Task.sleep(for: .milliseconds(20)) }
        let loaded = ms(t0.duration(to: clock.now))
        report("open+first refresh until changes loaded: \(Int(loaded)) ms, changes=\(p.status?.changes.count ?? 0)")
        model.section = .changes
        let first = renderTime(RootView().environment(model).frame(width: 1100, height: 700))
        report("first render of Changes screen: \(Int(first)) ms")
        let second = renderTime(RootView().environment(model).frame(width: 1100, height: 700))
        report("second render: \(Int(second)) ms")
        let t1 = clock.now
        await p.refresh()
        report("refresh(): \(Int(ms(t1.duration(to: clock.now)))) ms")
    }

    /// A TCP listener that completes handshakes (kernel backlog) and never answers.
    final class SilentServer {
        let fd: Int32; let port: UInt16
        init() throws {
            let sock = socket(AF_INET, SOCK_STREAM, 0)
            var addr = sockaddr_in(); addr.sin_family = sa_family_t(AF_INET); addr.sin_addr.s_addr = inet_addr("127.0.0.1"); addr.sin_port = 0
            addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            _ = withUnsafePointer(to: &addr) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(sock, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
            listen(sock, 16)
            var out = sockaddr_in(); var len = socklen_t(MemoryLayout<sockaddr_in>.size)
            _ = withUnsafeMutablePointer(to: &out) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(sock, $0, &len) } }
            fd = sock; port = UInt16(bigEndian: out.sin_port)
        }
        deinit { close(fd) }
    }

    @Test func diffSectionLookupInAHugeDiff() {
        var text = ""
        for i in 0..<30_000 { text += "--- /dev/null\n+++ b/d\(i / 100)/f\(i).txt\n@@ -0,0 +1,3 @@\n+line a \(i)\n+line b\n+line c\n" }
        let clock = ContinuousClock(); let t = clock.now
        let d = DiffParser.section(of: text, path: "d299/f29999.txt")
        let took = ms(t.duration(to: clock.now))
        report("diff section lookup in a \(text.utf8.count / 1_000_000) MB diff of 30k files: \(Int(took)) ms")
        #expect(d?.path == "d299/f29999.txt" && d?.lines.filter { $0.kind == .added }.count == 3)
        let t2 = clock.now
        let all = DiffParser.parse(text)
        report("(full parse of the same diff, no longer used on refresh: \(Int(ms(t2.duration(to: clock.now)))) ms, \(all.count) files)")
    }

    @Test func commitCancelAndFocusOnTheLargeProject() async throws {
        guard let path = ProcessInfo.processInfo.environment["RGIT_PERF_PROJECT"] else { return }
        _ = NSApplication.shared
        let clock = ContinuousClock()
        // Work on an APFS clone so the original stays uncommitted.
        let copy = FileManager.default.temporaryDirectory.appendingPathComponent("perf-copy-\(UUID().uuidString)")
        let cp = Process(); cp.executableURL = URL(fileURLWithPath: "/bin/cp"); cp.arguments = ["-cR", path, copy.path]
        try cp.run(); cp.waitUntilExit()
        defer { try? FileManager.default.removeItem(at: copy) }

        let silent = try SilentServer()
        let model = AppModel(); await model.start()
        #expect(await model.open(copy))
        let p = try #require(model.project)
        for _ in 0..<1200 where p.changeCount == 0 { try await Task.sleep(for: .milliseconds(20)) }
        #expect(p.changeCount >= 30_000)

        // focus an added file
        let t0 = clock.now
        p.setFocus(p.status!.changes[12_345].path)
        for _ in 0..<500 where p.focusDiff == nil { try await Task.sleep(for: .milliseconds(5)) }
        report("focus an added file -> diff ready: \(Int(ms(t0.duration(to: clock.now)))) ms (lines=\(p.focusDiff?.lines.count ?? -1))")

        // a hung sync, then commit while it runs, then cancel
        try await p.client.addRemote(name: "origin", url: "tcp://127.0.0.1:\(silent.port)")
        await p.refresh()
        let fetch = Task { await p.fetch() }
        for _ in 0..<200 where p.activities.isEmpty { try await Task.sleep(for: .milliseconds(5)) }
        try await Task.sleep(for: .milliseconds(400))
        report("fetch running against a silent host: activities=\(p.activities.count), Commit blocked=\(p.blocksChanges)")
        #expect(!p.blocksChanges)

        for i in 0..<1000 { p.setSelected(p.status!.changes[i].path, false) }
        let tc = clock.now
        let ok = await p.commit(message: "29,000 of 30,000 while a fetch hangs")
        report("commit 29,000 of 30,000 files (+150 MB file) while a fetch is hanging: \(Int(ms(tc.duration(to: clock.now)))) ms, ok=\(ok), left=\(p.changeCount)")
        #expect(ok)
        let tx = clock.now
        p.cancelActivity()
        report("Cancel -> status bar idle: \(Int(ms(tx.duration(to: clock.now)))) ms (activities=\(p.activities.count))")
        #expect(p.activities.isEmpty)
        _ = await fetch.value
        report("fetch() call returned \(Int(ms(tx.duration(to: clock.now)))) ms after Cancel")
    }
}
