import Foundation
import Testing
import RgitClient
@testable import RustGitKit

@MainActor
@Suite("Activities, cancellation and the Commit button")
struct ActivityTests {
    func fakeProject() -> ProjectModel {
        ProjectModel(url: URL(fileURLWithPath: "/nonexistent-project"), client: RgitClient(executable: URL(fileURLWithPath: "/usr/bin/false")))
    }
    func ms(_ d: Duration) -> Double { Double(d.components.seconds) * 1000 + Double(d.components.attoseconds) / 1e15 }

    struct Boom: Error {}

    @Test func activityIsClearedAfterSuccessAndFailure() async {
        let p = fakeProject()
        let v: Int? = await p.run("ok", kind: .fetch, refreshAfter: false) { _, _ in 42 }
        #expect(v == 42 && p.activities.isEmpty)
        let w: Int? = await p.run("boom", kind: .fetch, refreshAfter: false) { _, _ in throw Boom() }
        #expect(w == nil && p.activities.isEmpty && p.error != nil)
    }

    @Test func cancelReturnsToIdleImmediatelyEvenIfTheWorkIgnoresIt() async throws {
        let p = fakeProject()
        // Work that never finishes and ignores cancellation, like a child process slow to die.
        let runner = Task { await p.run("hang", kind: .fetch, cancellable: true, refreshAfter: false) { _, _ in
            while true { try? await Task.sleep(for: .seconds(60)) }   // swallows cancellation
            return 1
        } }
        for _ in 0..<100 where p.activities.isEmpty { try await Task.sleep(for: .milliseconds(5)) }
        #expect(p.activities.count == 1)
        let clock = ContinuousClock(), t = clock.now
        p.cancelActivity()
        _ = await runner.value
        let took = ms(t.duration(to: clock.now))
        print("PERF app cancel -> idle with stubborn work: \(Int(took)) ms")
        #expect(p.activities.isEmpty)
        // ~2-3 ms alone; the bound is loose because other suites render on the same main actor in parallel.
        #expect(took < 1000)
    }

    @Test func cancellingTheCallingTaskAlsoClears() async throws {
        let p = fakeProject()
        let runner = Task { await p.run("hang", kind: .push, cancellable: true, refreshAfter: false) { _, _ in
            try await Task.sleep(for: .seconds(60)); return 1
        } }
        for _ in 0..<100 where p.activities.isEmpty { try await Task.sleep(for: .milliseconds(5)) }
        runner.cancel()
        _ = await runner.value
        #expect(p.activities.isEmpty)
    }

    @Test func backgroundSyncDoesNotBlockCommitButAMutationDoes() async throws {
        let p = fakeProject()
        let fetch = Task { await p.run("fetch", kind: .fetch, cancellable: true, refreshAfter: false) { _, _ in
            try await Task.sleep(for: .seconds(60)); return 1
        } }
        let verify = Task { await p.run("verify", kind: .verify, cancellable: true, refreshAfter: false) { _, _ in
            try await Task.sleep(for: .seconds(60)); return 1
        } }
        for _ in 0..<100 where p.activities.count < 2 { try await Task.sleep(for: .milliseconds(5)) }
        #expect(p.activities.count == 2)
        #expect(!p.blocksChanges)                       // Commit stays enabled during fetch + verify
        // A mutation can start alongside them…
        let slow = Task { await p.run("commit", kind: .mutation, refreshAfter: false) { _, _ in
            try await Task.sleep(for: .milliseconds(300)); return 7
        } }
        for _ in 0..<100 where !p.blocksChanges { try await Task.sleep(for: .milliseconds(5)) }
        #expect(p.blocksChanges)
        // …and a second one is refused rather than queued behind it.
        let second: Int? = await p.run("switch", kind: .mutation, refreshAfter: false) { _, _ in 9 }
        #expect(second == nil)
        #expect(await slow.value == 7)
        #expect(!p.blocksChanges)
        p.cancelActivity(); p.cancelActivity()
        #expect(p.activities.isEmpty)   // synchronous: both gone before any task unwinds
        _ = await fetch.value; _ = await verify.value
        #expect(p.activities.isEmpty)
    }

    @Test func refreshCallsAreCoalesced() async throws {
        // No engine behind this project: reload() fails fast. Many concurrent refreshes must all return
        // and leave no refresh running.
        let p = fakeProject()
        await withTaskGroup(of: Void.self) { g in for _ in 0..<50 { g.addTask { await p.refresh() } } }
        #expect(!p.isRefreshing)
    }

    @Test func selectionIsConstantTimeBookkeeping() {
        let p = fakeProject()
        let changes = (0..<30_000).map { Change(path: "d\($0 / 100)/f\($0).txt", kind: .added, from: nil) }
        p.setStatusForTesting(RepoStatus(branch: "main", head: nil, changes: changes))
        #expect(p.selectedCount == 30_000 && p.allSelected)
        p.setSelected("d0/f0.txt", false)
        #expect(p.selectedCount == 29_999 && !p.allSelected)
        if case .allExcept(let ex) = p.commitSelection() { #expect(ex == ["d0/f0.txt"]) } else { Issue.record("expected allExcept") }
        p.selectNone()
        #expect(p.selectedCount == 0)
        p.setSelected("d0/f1.txt", true)
        if case .only(let on) = p.commitSelection() { #expect(on == ["d0/f1.txt"]) } else { Issue.record("expected only") }
        p.selectAll()
        #expect(p.commitSelection() == .all)
    }
}
