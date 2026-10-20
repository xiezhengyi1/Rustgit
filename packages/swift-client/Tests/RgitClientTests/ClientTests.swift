import Foundation
import Testing
@testable import RgitClient

@Suite("Local operations against real rgit")
struct LocalTests {
    @Test func versionAndCompatibility() async throws {
        let c = RgitClient(executable: TestEnv.rgit)
        let v = try await c.checkCompatibility()
        #expect(v.jsonInterface == RgitClient.supportedJSONInterface)
        #expect(!v.cli.isEmpty)
    }

    @Test func statusCommitLogShowDiff() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box)

        var st = try await c.status()
        #expect(st.branch == "main")
        #expect(st.changes.isEmpty)
        #expect(st.head != nil)

        try Sandbox.write("one\nTWO\nthree\n", to: "a.txt", in: dir)
        try Sandbox.write("new\n", to: "dir/b.txt", in: dir)
        st = try await c.status()
        #expect(Set(st.changes.map(\.path)) == ["a.txt", "dir/b.txt"])
        #expect(st.changes.first { $0.path == "a.txt" }?.kind == .modified)
        #expect(st.changes.first { $0.path == "dir/b.txt" }?.kind == .added)

        let d = try await c.diff()
        #expect(d.diff.contains("+TWO"))

        // Commit only one file; the other stays pending.
        let r = try await c.commit(message: "edit a — ünïcode ✓", paths: ["a.txt"])
        #expect(r.branch == "main")
        st = try await c.status()
        #expect(st.changes.map(\.path) == ["dir/b.txt"])

        let log = try await c.log(limit: 10)
        #expect(log.count == 2)
        #expect(log[0].message.contains("ünïcode ✓"))
        #expect(log[0].id == r.id)
        #expect(log[0].branches.contains("main"))
        #expect(log[0].authorName == "Test User")

        let shown = try await c.show(log[0].id)
        #expect(shown.commit.id == r.id)
        #expect(shown.diff.contains("+TWO"))
    }

    @Test func messageStartingWithDashIsNotAFlag() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box)
        try Sandbox.write("x\n", to: "x.txt", in: dir)
        _ = try await c.commit(message: "--amend this is just text")
        #expect(try await c.log(limit: 1)[0].message.hasPrefix("--amend"))
    }

    @Test func branchesSwitchDelete() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box)
        try await c.createBranch("feature")
        var bl = try await c.branches()
        #expect(bl.current == "main")
        #expect(Set(bl.branches.map(\.name)) == ["main", "feature"])

        _ = try await c.switchBranch("feature")
        try Sandbox.write("feature work\n", to: "f.txt", in: dir)
        _ = try await c.commit(message: "feature commit")
        _ = try await c.switchBranch("main")
        #expect(!FileManager.default.fileExists(atPath: dir.appendingPathComponent("f.txt").path))

        _ = try await c.switchBranch("topic", create: true)
        #expect(try await c.branches().current == "topic")
        _ = try await c.switchBranch("main")
        try await c.deleteBranch("topic")
        bl = try await c.branches()
        #expect(!bl.branches.contains { $0.name == "topic" })
    }

    @Test func restoreAndRevert() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box)
        try Sandbox.write("changed\n", to: "a.txt", in: dir)
        let second = try await c.commit(message: "change")
        try Sandbox.write("scratch\n", to: "a.txt", in: dir)

        // Uncommitted edits are protected unless forced.
        do {
            _ = try await c.restore(paths: ["a.txt"])
            Issue.record("restore over uncommitted work must fail")
        } catch let e as RgitError {
            #expect(e.code == .dirtyWorkingTree)
            #expect(e.paths == ["a.txt"])
        }
        let r = try await c.restore(paths: ["a.txt"], force: true)
        #expect(r.restored == ["a.txt"])
        #expect(try Sandbox.read("a.txt", in: dir) == "changed\n")

        let rev = try await c.revert(second.id)
        #expect(rev.commit != nil)
        #expect(try Sandbox.read("a.txt", in: dir) == "one\ntwo\nthree\n")
    }

    @Test func mergeConflictResolveOursTheirsAndEdit() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box, files: ["a.txt": "base\n", "b.txt": "base\n", "c.txt": "base\n"])
        _ = try await c.switchBranch("other", create: true)
        for f in ["a.txt", "b.txt", "c.txt"] { try Sandbox.write("theirs \(f)\n", to: f, in: dir) }
        _ = try await c.commit(message: "other side")
        _ = try await c.switchBranch("main")
        for f in ["a.txt", "b.txt", "c.txt"] { try Sandbox.write("ours \(f)\n", to: f, in: dir) }
        _ = try await c.commit(message: "main side")

        let m = try await c.merge("other")
        #expect(Set(m.conflicts) == ["a.txt", "b.txt", "c.txt"])
        var st = try await c.status()
        #expect(st.merge != nil)
        #expect(st.conflicts.count == 3)
        #expect(st.conflicts.allSatisfy { !$0.resolved })

        // Committing with unresolved conflicts is refused with a structured error.
        do { _ = try await c.commit(message: "too early"); Issue.record("must fail") }
        catch let e as RgitError { #expect(e.code == .unresolvedConflicts || e.code == .mergeInProgress) }

        _ = try await c.resolve("a.txt", with: .ours)
        _ = try await c.resolve("b.txt", with: .theirs)
        try Sandbox.write("hand merged\n", to: "c.txt", in: dir)
        _ = try await c.resolve("c.txt", with: .workingTree)
        #expect(try Sandbox.read("a.txt", in: dir) == "ours a.txt\n")
        #expect(try Sandbox.read("b.txt", in: dir) == "theirs b.txt\n")
        #expect(try Sandbox.read("c.txt", in: dir) == "hand merged\n")

        st = try await c.status()
        #expect(st.conflicts.allSatisfy { $0.resolved })
        _ = try await c.commit(message: "merge other")
        st = try await c.status()
        #expect(st.merge == nil)
        #expect(st.conflicts.isEmpty)
    }

    @Test func abortMerge() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box, files: ["a.txt": "base\n"])
        _ = try await c.switchBranch("other", create: true)
        try Sandbox.write("theirs\n", to: "a.txt", in: dir)
        _ = try await c.commit(message: "t")
        _ = try await c.switchBranch("main")
        try Sandbox.write("ours\n", to: "a.txt", in: dir)
        _ = try await c.commit(message: "o")
        _ = try await c.merge("other")
        _ = try await c.abortMerge()
        #expect(try await c.status().merge == nil)
        #expect(try Sandbox.read("a.txt", in: dir) == "ours\n")
    }

    @Test func storageStatsVerifyGcReindex() async throws {
        let box = try Sandbox()
        let big = String(repeating: "0123456789abcdef\n", count: 20_000)
        let (c, dir) = try await makeProject(box, files: ["big.txt": big])
        try Sandbox.write(big + "tail\n", to: "big2.txt", in: dir)
        _ = try await c.commit(message: "dup")

        let s = try await c.stats()
        #expect(s.commits == 2)
        #expect(s.storedBytes > 0 && s.storedBytes < s.logicalBytes)
        #expect((s.dedupRatio ?? 0) > 1)

        let v = try await c.verify()
        #expect(v.problems.isEmpty)
        #expect(v.commits == 2)

        let g = try await c.gc()
        #expect(g.repoBytesAfter > 0)
        let r = try await c.reindex()
        #expect(r.problems.isEmpty)
        #expect(try await c.verify().problems.isEmpty)
    }

    @Test func shelveRoundTrip() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box)
        try Sandbox.write("wip\n", to: "a.txt", in: dir)
        let name = try await c.shelve(message: "wip save")
        #expect(try Sandbox.read("a.txt", in: dir) == "one\ntwo\nthree\n")
        let list = try await c.shelves()
        #expect(list.shelves.count == 1)
        _ = try await c.unshelve(name)
        #expect(try Sandbox.read("a.txt", in: dir) == "wip\n")
    }
}

@Suite("Error mapping")
struct ErrorTests {
    @Test func notARepository() async throws {
        let box = try Sandbox()
        let c = makeClient(in: try box.dir("empty"))
        do { _ = try await c.status(); Issue.record("must fail") }
        catch let e as RgitError {
            #expect(e.code == .notARepository)
            #expect(!(e.errorDescription ?? "").isEmpty)
        }
    }

    @Test func nothingToCommitAndBranchErrors() async throws {
        let box = try Sandbox()
        let (c, _) = try await makeProject(box)
        do { _ = try await c.commit(message: "nothing"); Issue.record("must fail") }
        catch let e as RgitError { #expect(e.code == .nothingToCommit) }
        do { _ = try await c.switchBranch("nope"); Issue.record("must fail") }
        catch let e as RgitError { #expect(e.code == .branchNotFound) }
        try await c.createBranch("dup")
        do { try await c.createBranch("dup"); Issue.record("must fail") }
        catch let e as RgitError { #expect(e.code == .branchExists) }
        do { try await c.createBranch("bad name with spaces"); Issue.record("must fail") }
        catch let e as RgitError { #expect(e.code == .invalidName) }
    }

    @Test func dirtyWorkingTreeCarriesPaths() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box)
        try await c.createBranch("other")
        _ = try await c.switchBranch("other")
        try Sandbox.write("x\n", to: "a.txt", in: dir)
        _ = try await c.commit(message: "diverge")
        _ = try await c.switchBranch("main")
        try Sandbox.write("local edit\n", to: "a.txt", in: dir)
        do { _ = try await c.switchBranch("other"); Issue.record("must fail") }
        catch let e as RgitError {
            #expect(e.code == .dirtyWorkingTree)
            #expect(e.paths.contains("a.txt"))
        }
    }

    @Test func missingExecutable() async throws {
        let c = RgitClient(executable: URL(fileURLWithPath: "/nonexistent/rgit"))
        do { _ = try await c.version(); Issue.record("must fail") }
        catch let e as RgitError {
            guard case .executableUnavailable = e else { Issue.record("wrong case \(e)"); return }
        }
    }

    @Test func nonJSONBinaryIsProcessFailed() async throws {
        // /usr/bin/false prints nothing and exits 1: no result object.
        let c = RgitClient(executable: URL(fileURLWithPath: "/usr/bin/false"))
        do { _ = try await c.version(); Issue.record("must fail") }
        catch let e as RgitError {
            guard case .processFailed(let code, _) = e else { Issue.record("wrong case \(e)"); return }
            #expect(code == 1)
        }
    }

    @Test func incompatibleInterfaceIsRefused() async throws {
        let box = try Sandbox()
        let fake = box.url.appendingPathComponent("fake-rgit")
        try "#!/bin/sh\necho '{\"ok\":true,\"data\":{\"cli\":\"9.9.9\",\"json_interface\":2,\"format\":1}}'\n"
            .write(to: fake, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: fake.path)
        let c = RgitClient(executable: fake)
        do { try await c.checkCompatibility(); Issue.record("must fail") }
        catch let e as RgitError { #expect(e == .incompatibleInterface(found: 2, expected: 1)) }
    }

    @Test func unknownErrorCodeIsPreserved() async throws {
        let box = try Sandbox()
        let fake = box.url.appendingPathComponent("fake-rgit")
        try "#!/bin/sh\necho '{\"ok\":false,\"error\":{\"code\":\"brand_new_code\",\"message\":\"hello\",\"paths\":[\"x\"]}}'\nexit 1\n"
            .write(to: fake, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: fake.path)
        do { _ = try await RgitClient(executable: fake).status(); Issue.record("must fail") }
        catch let e as RgitError {
            #expect(e == .engine(code: .unknown("brand_new_code"), message: "hello", paths: ["x"]))
        }
    }

    @Test func malformedResultIsReported() async throws {
        let box = try Sandbox()
        let fake = box.url.appendingPathComponent("fake-rgit")
        try "#!/bin/sh\necho '{\"ok\":true,\"data\":{\"unexpected\":1}}'\n"
            .write(to: fake, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: fake.path)
        do { _ = try await RgitClient(executable: fake).stats(); Issue.record("must fail") }
        catch let e as RgitError {
            guard case .malformedOutput = e else { Issue.record("wrong case \(e)"); return }
        }
    }
}

@Suite("Progress events and cancellation")
struct ProgressTests {
    final class Collector: @unchecked Sendable {
        private let lock = NSLock(); private var items: [ProgressEvent] = []
        func add(_ e: ProgressEvent) { lock.lock(); items.append(e); lock.unlock() }
        var all: [ProgressEvent] { lock.lock(); defer { lock.unlock() }; return items }
    }

    @Test func progressLinesAreSeparatedFromTheResult() async throws {
        let box = try Sandbox()
        let fake = box.url.appendingPathComponent("fake-rgit")
        try """
        #!/bin/sh
        echo '{"event":"progress","phase":"upload","done":1,"total":4,"bytes":10}'
        echo '{"event":"progress","phase":"upload","done":4,"total":4,"bytes":40}'
        echo '{"ok":true,"data":{"objects":3,"packs":1,"problems":[]}}'
        """.write(to: fake, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: fake.path)
        let got = Collector()
        let r = try await RgitClient(executable: fake).run(["reindex"], as: ReindexResult.self, progress: { got.add($0) })
        #expect(r.objects == 3)
        #expect(got.all.count == 2)
        #expect(got.all.last?.fraction == 1.0)
        #expect(got.all.first?.phase == "upload")
    }

    @Test func realCloneEmitsProgressAndSyncsBothWays() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box, files: ["a.txt": String(repeating: "data\n", count: 40_000)])
        let hostDir = try box.dir("host.rgit")
        let serve = try await ServeSession.start(executable: TestEnv.rgit, directory: hostDir,
                                                 options: .init(listen: "127.0.0.1:0", initialize: true, insecureOpenAccess: true))
        defer { Task { await serve.stop() } }
        #expect(serve.address.hasPrefix("127.0.0.1:"))
        let url = "tcp://\(serve.address)"

        try await c.addRemote(name: "origin", url: url)
        #expect(try await c.remotes().map(\.name) == ["origin"])
        let pushed = try await c.push(remote: "origin")
        #expect(!pushed.upToDate)

        let cloneDir = box.url.appendingPathComponent("clone")
        let got = Collector()
        let cr = try await RgitClient(executable: TestEnv.rgit).clone(url: url, into: cloneDir, progress: { got.add($0) })
        #expect(cr.branch == "main")
        #expect(try Sandbox.read("a.txt", in: cloneDir).count == 200_000)
        #expect(got.all.allSatisfy { $0.event == "progress" })

        // Second commit in the clone, pushed; the original fetches and sees "behind".
        let cc = makeClient(in: cloneDir)
        try await cc.setConfig("user.name", "Clone User")
        try await cc.setConfig("user.email", "c@example.com")
        try Sandbox.write("from clone\n", to: "n.txt", in: cloneDir)
        _ = try await cc.commit(message: "clone work")
        _ = try await cc.push()
        let f = try await c.fetch()
        #expect(f.state == .behind)
        let p = try await c.pull()
        #expect(p.merge?.fastForward == true)
        #expect(try Sandbox.read("n.txt", in: dir) == "from clone\n")
        #expect(try await c.fetch().state == .upToDate)
    }

    @Test func rejectedPushIsAStructuredRefConflict() async throws {
        let box = try Sandbox()
        let (a, _) = try await makeProject(box, name: "a")
        let serve = try await ServeSession.start(executable: TestEnv.rgit, directory: try box.dir("h.rgit"),
                                                 options: .init(listen: "127.0.0.1:0", initialize: true, insecureOpenAccess: true))
        defer { Task { await serve.stop() } }
        let url = "tcp://\(serve.address)"
        try await a.addRemote(name: "origin", url: url)
        _ = try await a.push()
        let bDir = box.url.appendingPathComponent("b")
        _ = try await RgitClient(executable: TestEnv.rgit).clone(url: url, into: bDir)
        let b = makeClient(in: bDir)
        try await b.setConfig("user.name", "B"); try await b.setConfig("user.email", "b@e")
        try Sandbox.write("b\n", to: "b.txt", in: bDir)
        _ = try await b.commit(message: "b work")
        try Sandbox.write("a\n", to: "a2.txt", in: a.directory!)
        _ = try await a.commit(message: "a work")
        _ = try await a.push()
        do { _ = try await b.push(); Issue.record("diverged push must be rejected") }
        catch let e as RgitError { #expect(e.code == .refConflict) }
        // Local commits survive the rejection.
        #expect(try await b.log(limit: 5).first?.message == "b work")
    }

    @Test func cancellationTerminatesTheProcess() async throws {
        let box = try Sandbox()
        let pidFile = box.url.appendingPathComponent("pid")
        let fake = box.url.appendingPathComponent("fake-rgit")
        try """
        #!/bin/sh
        echo $$ > "\(pidFile.path)"
        echo '{"event":"progress","phase":"upload","done":1,"total":100,"bytes":1}'
        exec sleep 600
        """.write(to: fake, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: fake.path)

        let got = Collector()
        let task = Task { try await RgitClient(executable: fake).fetch(progress: { got.add($0) }) }
        try await waitUntil { got.all.count == 1 }
        let pid = try #require(Int32(String(contentsOf: pidFile, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)))
        #expect(kill(pid, 0) == 0)             // alive

        let started = Date()
        task.cancel()
        do { _ = try await task.value; Issue.record("must be cancelled") }
        catch let e as RgitError { #expect(e == .cancelled) }
        #expect(Date().timeIntervalSince(started) < 5)
        try await waitUntil { kill(pid, 0) != 0 }   // process is gone
    }

    @Test func cancelledBeforeStartNeverLeavesAProcess() async throws {
        let box = try Sandbox()
        let marker = box.url.appendingPathComponent("ran")
        let fake = box.url.appendingPathComponent("fake-rgit")
        try "#!/bin/sh\nsleep 1; touch \"\(marker.path)\"\n".write(to: fake, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: fake.path)
        let task = Task { try await RgitClient(executable: fake).status() }
        task.cancel()
        _ = try? await task.value
        try await Task.sleep(for: .milliseconds(1500))
        #expect(!FileManager.default.fileExists(atPath: marker.path))
    }

    @Test func cancelledRealOperationLeavesRepositoryHealthy() async throws {
        let box = try Sandbox()
        var files: [String: String] = [:]
        for i in 0..<30 { files["f\(i).txt"] = String(repeating: "line \(i)\n", count: 20_000) }
        let (c, _) = try await makeProject(box, files: files)
        let task = Task { try await c.verify() }
        task.cancel()
        _ = try? await task.value
        // Whatever happened, the repository still verifies cleanly afterwards.
        #expect(try await c.verify().problems.isEmpty)
    }
}

@Suite("Serve session")
struct ServeTests {
    @Test func startStopAndPortConflict() async throws {
        let box = try Sandbox()
        let host = try box.dir("h.rgit")
        let s = try await ServeSession.start(executable: TestEnv.rgit, directory: host,
                                             options: .init(listen: "127.0.0.1:0", initialize: true, insecureOpenAccess: true))
        #expect(s.isRunning)
        #expect(!s.projectID.isEmpty)

        // The same directory cannot be served twice: structured error, no hang.
        do {
            _ = try await ServeSession.start(executable: TestEnv.rgit, directory: host,
                                             options: .init(listen: "127.0.0.1:0", insecureOpenAccess: true))
            Issue.record("second serve must fail")
        } catch let e as RgitError {
            #expect(e.code != nil)
        }
        await s.stop()
        #expect(!s.isRunning)
    }

    @Test func refusesNonLoopbackWithoutOptIn() async throws {
        let box = try Sandbox()
        do {
            _ = try await ServeSession.start(executable: TestEnv.rgit, directory: try box.dir("h.rgit"),
                                             options: .init(listen: "0.0.0.0:0", initialize: true, insecureOpenAccess: true))
            Issue.record("must be refused")
        } catch let e as RgitError {
            #expect(e.errorDescription?.contains("insecure") == true || e.errorDescription?.contains("refusing") == true)
        }
    }
}

func waitUntil(timeout: TimeInterval = 10, _ cond: @Sendable () -> Bool) async throws {
    let end = Date().addingTimeInterval(timeout)
    while !cond() {
        if Date() > end { throw RgitError.malformedOutput("timeout waiting for condition") }
        try await Task.sleep(for: .milliseconds(25))
    }
}

@Suite("Large commits and cancellation latency")
struct LargeTests {
    /// Writes `count` small files across directories.
    static func generate(_ count: Int, in dir: URL) throws {
        let fm = FileManager.default
        for d in 0..<(count / 100 + 1) { try fm.createDirectory(at: dir.appendingPathComponent("d\(d)"), withIntermediateDirectories: true) }
        for i in 0..<count { try "file \(i)\n".write(to: dir.appendingPathComponent("d\(i / 100)/f\(i).txt"), atomically: false, encoding: .utf8) }
    }

    @Test func commitSelectionAllOnlyAndExcept() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box, files: [:])
        for n in ["a", "b", "c", "d"] { try Sandbox.write("\(n)\n", to: "\(n).txt", in: dir) }
        _ = try await c.commit(message: "only a,b", selection: .only(["a.txt", "b.txt"]))
        #expect(Set(try await c.status().changes.map(\.path)) == ["c.txt", "d.txt"])
        _ = try await c.commit(message: "all but d", selection: .allExcept(["d.txt"]))
        #expect(try await c.status().changes.map(\.path) == ["d.txt"])
        _ = try await c.commit(message: "rest", selection: .all)
        #expect(try await c.status().changes.isEmpty)
        // A path with a line break cannot be listed one-per-line: refused before anything runs.
        try Sandbox.write("x\n", to: "e.txt", in: dir)
        do { _ = try await c.commit(message: "bad", selection: .only(["e\n.txt"])); Issue.record("must fail") }
        catch let e as RgitError { #expect(e.code == .invalid) }
    }

    @Test func thirtyThousandFilesCommitThroughPathFiles() async throws {
        let box = try Sandbox()
        let (c, dir) = try await makeProject(box, files: [:])
        try Self.generate(30_000, in: dir)
        let clock = ContinuousClock()
        let t0 = clock.now
        let st = try await c.status()
        let statusTime = t0.duration(to: clock.now)
        #expect(st.changes.count == 30_000)

        // 29,000 selected -> exclude the 1,000 unselected; argv would exceed ARG_MAX with these paths.
        let all = st.changes.map(\.path)
        let unselected = Array(all.prefix(1000))
        let t1 = clock.now
        _ = try await c.commit(message: "most", selection: .allExcept(unselected))
        let exceptTime = t1.duration(to: clock.now)
        let left = try await c.status()
        #expect(Set(left.changes.map(\.path)) == Set(unselected))

        // Few selected -> include list; 20,000 paths this time on a fresh set.
        try Self.generate(30_000, in: dir.appendingPathComponent("second"))
        let st2 = try await c.status()
        let pick = st2.changes.map(\.path).filter { $0.hasPrefix("second/") }.prefix(20_000)
        let t2 = clock.now
        _ = try await c.commit(message: "only many", selection: .only(Array(pick)))
        let onlyTime = t2.duration(to: clock.now)
        #expect(try await c.status().changes.count == 1000 + 10_000)
        print("PERF 30k status \(statusTime), commit allExcept(1000) \(exceptTime), commit only(20000) \(onlyTime)")
        #expect(exceptTime < .seconds(20) && onlyTime < .seconds(20))
    }

    /// A TCP server that completes the handshake (kernel backlog) and then never answers.
    final class SilentServer {
        let fd: Int32
        let port: UInt16
        init() throws {
            let sock = socket(AF_INET, SOCK_STREAM, 0)
            var addr = sockaddr_in(); addr.sin_family = sa_family_t(AF_INET); addr.sin_addr.s_addr = inet_addr("127.0.0.1"); addr.sin_port = 0
            addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            let r = withUnsafePointer(to: &addr) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(sock, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
            guard r == 0, listen(sock, 16) == 0 else { throw RgitError.malformedOutput("socket setup failed") }
            var out = sockaddr_in(); var len = socklen_t(MemoryLayout<sockaddr_in>.size)
            _ = withUnsafeMutablePointer(to: &out) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(sock, $0, &len) } }
            fd = sock
            port = UInt16(bigEndian: out.sin_port)
        }
        deinit { close(fd) }
    }

    func cancelLatency(url: String, op: @Sendable @escaping (RgitClient) async throws -> Void) async throws -> Duration {
        let box = try Sandbox()
        let (c, _) = try await makeProject(box)
        try await c.addRemote(name: "origin", url: url)
        let task = Task { try await op(c) }
        try await Task.sleep(for: .milliseconds(600))      // let the engine connect and block
        let clock = ContinuousClock()
        let t = clock.now
        task.cancel()
        do { try await task.value; Issue.record("must not succeed") } catch let e as RgitError { #expect(e == .cancelled) }
        return t.duration(to: clock.now)
    }

    @Test func cancelFetchAgainstServerThatNeverAnswers() async throws {
        let s = try SilentServer()
        let d = try await cancelLatency(url: "tcp://127.0.0.1:\(s.port)") { _ = try await $0.fetch() }
        print("PERF cancel fetch (silent host): \(d)")
        #expect(d < .milliseconds(500))
    }

    @Test func cancelPushAgainstServerThatNeverAnswers() async throws {
        let s = try SilentServer()
        let d = try await cancelLatency(url: "tcp://127.0.0.1:\(s.port)") { _ = try await $0.push() }
        print("PERF cancel push (silent host): \(d)")
        #expect(d < .milliseconds(500))
    }

    @Test func cancelFetchAgainstUnroutableHost() async throws {
        let d = try await cancelLatency(url: "tcp://10.255.255.1:7878") { _ = try await $0.fetch() }
        print("PERF cancel fetch (unroutable host): \(d)")
        #expect(d < .milliseconds(500))
    }
}
