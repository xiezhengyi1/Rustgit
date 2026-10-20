import Foundation
import Observation
import RgitClient

/// Kinds of foreground work. Only some of them keep the Commit button from working.
public enum ActivityKind: Sendable { case fetch, push, pull, verify, mutation, maintenance }

/// What the engine is doing right now, shown in the status bar with a Cancel button.
public struct ActivityState: Identifiable, Equatable {
    public let id = UUID()
    public var title: String
    public var phase: String?
    public var fraction: Double?
    public var cancellable: Bool
    public var kind: ActivityKind = .mutation

    /// Commit/switch/merge need the working folder to themselves; a fetch, push or verify does not.
    var blocksChanges: Bool { switch kind { case .mutation, .pull, .maintenance: true; case .fetch, .push, .verify: false } }
}

/// Resolves exactly once, from whichever side gets there first (the work finishing or the user cancelling).
final class Gate<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<T, Error>?
    private var waiter: CheckedContinuation<Result<T, Error>, Never>?
    func resolve(_ r: Result<T, Error>) {
        lock.lock()
        guard result == nil else { lock.unlock(); return }
        result = r
        let w = waiter; waiter = nil
        lock.unlock()
        w?.resume(returning: r)
    }
    func wait() async -> Result<T, Error> {
        await withCheckedContinuation { c in
            lock.lock()
            if let result { lock.unlock(); c.resume(returning: result) }
            else { waiter = c; lock.unlock() }
        }
    }
}

/// Three-step sharing indicator from plan section 10.
public enum ShareStage { case committedLocally, receivedByServer, fetchedByPeer }

public enum DiffState: Equatable { case idle, loading(slow: Bool), failed(String) }

@MainActor @Observable
public final class ProjectModel: Identifiable {
    public let url: URL
    public nonisolated var id: String { url.path }
    public var name: String { url.lastPathComponent }
    let client: RgitClient

    // Repository state
    public var status: RepoStatus?
    public var branches: BranchList?
    public var commits: [CommitInfo] = []
    public var graph: [String: GraphRow] = [:]
    public var remotes: [RemoteInfo] = []
    public var stats: RepoStats?
    public var shelves: ShelfList?
    public var skipped: [SkippedPath] { status?.skipped ?? [] }

    // Selection for the next commit: only the explicitly unticked paths are stored, so
    // "select all", the counts and the default for new files are O(1) however many files changed.
    public private(set) var unselected: Set<String> = []
    // Diff of the focused file, loaded on demand (never the whole tree's diff on refresh).
    public var focus: String?
    public private(set) var focusDiff: FileDiff?
    public private(set) var diffState: DiffState = .idle
    private var fullDiffText: String?
    private var diffTask: Task<Void, Never>?

    // Sync state
    public var syncState: SyncState?
    public var lastFetch: Date?
    public var lastPushedHead: String?       // head the host acknowledged in this session
    public var pushedAt: Date?

    // UI state
    public private(set) var connection: ConnectionInfo?
    public private(set) var activities: [ActivityState] = []
    public private(set) var isRefreshing = false
    public var error: UserFacingError?
    public var errorAction: (title: String, run: @MainActor () async -> Void)?
    public var lastMessage: String?
    public var commitMessageDraft = ""
    public private(set) var commitAndShareInProgress = false
    private var pendingShareRemote: String?
    public var hasPendingShare: Bool { pendingShareRemote != nil }
    public var verifyResult: VerifyResult?
    public var gcResult: GcResult?
    public var reindexResult: ReindexResult?

    private var connectionActivity: UUID?
    private var cancelHooks: [UUID: () -> Void] = [:]
    private var logHead: String??

    public init(url: URL, client: RgitClient) {
        self.url = url
        self.client = client.inDirectory(url)
    }

    // MARK: Derived

    /// The activity shown in the status bar (the oldest one running).
    public var activity: ActivityState? { activities.first }
    /// True while something is running that needs the working folder: Commit waits for that, not for fetches.
    public var blocksChanges: Bool { activities.contains { $0.blocksChanges } }
    public func isRunning(_ kind: ActivityKind) -> Bool { activities.contains { $0.kind == kind } }
    public var branchName: String { status?.branch ?? branches?.current ?? "—" }
    public var hasRemote: Bool { !remotes.isEmpty }
    public var isMerging: Bool { status?.merge != nil }
    public var hasConflicts: Bool { !(status?.conflicts.isEmpty ?? true) }
    public var changeCount: Int { status?.changes.count ?? 0 }
    public var selectedCount: Int { changeCount - unselected.count }
    public var allSelected: Bool { unselected.isEmpty }

    public func stage(_ s: ShareStage) -> Bool {
        switch s {
        case .committedLocally: return status?.head != nil
        case .receivedByServer:
            guard let head = status?.head else { return false }
            return syncState == .upToDate || lastPushedHead == head
        case .fetchedByPeer: return false      // the engine does not report peer fetches yet
        }
    }

    // MARK: Plumbing

    /// Runs `work` as foreground activity of the given kind.
    ///
    /// * The activity is always removed again: the single exit path below runs on success, failure,
    ///   cancellation by the user, and cancellation of the calling task.
    /// * Cancelling returns immediately. The child process is terminated on a background task; nothing
    ///   waits for it, and no refresh is awaited either.
    /// * Returns nil when it did not run (busy, failed or cancelled); the error banner explains failures.
    @discardableResult
    func run<T>(_ title: String, kind: ActivityKind = .mutation, cancellable: Bool = false, refreshAfter: Bool = true,
                _ work: @escaping (RgitClient, @escaping ProgressHandler) async throws -> T) async -> T? {
        let probe = ActivityState(title: title, phase: nil, fraction: nil, cancellable: cancellable, kind: kind)
        if activities.contains(where: { $0.kind == kind }) || (probe.blocksChanges && blocksChanges) {
            lastMessage = L("Another action is still running.")
            return nil
        }
        error = nil; errorAction = nil
        let id = probe.id
        activities.append(probe)
        if [.fetch, .push, .pull].contains(kind) { connectionActivity = id; connection = nil }
        let handler: ProgressHandler = { [weak self] e in
            Task { @MainActor in
                guard let self else { return }
                if let connection = e.connection, self.connectionActivity == id { self.connection = connection }
                guard let i = self.activities.firstIndex(where: { $0.id == id }) else { return }
                guard e.event == "progress" else { return }
                self.activities[i].phase = e.phase
                self.activities[i].fraction = e.fraction
            }
        }
        let client = self.client
        let gate = Gate<T>()
        let t = Task {
            do { gate.resolve(.success(try await work(client, handler))) } catch { gate.resolve(.failure(error)) }
        }
        let cancel = { gate.resolve(.failure(RgitError.cancelled)); t.cancel() }
        cancelHooks[id] = cancel
        let outcome = await withTaskCancellationHandler { await gate.wait() } onCancel: { cancel() }
        // Single exit path.
        cancelHooks[id] = nil
        activities.removeAll { $0.id == id }

        switch outcome {
        case .success(let v):
            if refreshAfter { await refresh() }
            return v
        case .failure(let e):
            if let r = e as? RgitError, r == .cancelled {
                lastMessage = L("Cancelled. Nothing was lost; interrupted transfers resume next time.")
                if kind != .fetch && kind != .push { Task { await refresh() } }
            } else {
                error = ErrorPresenter.present(e)
                await refresh()
            }
            return nil
        }
    }

    /// Stops an activity now: the status bar clears at once and the engine process dies in the background.
    public func cancelActivity(_ id: UUID? = nil) {
        guard let target = id ?? activities.first(where: { $0.cancellable })?.id else { return }
        cancelHooks[target]?()
        activities.removeAll { $0.id == target }       // idle right now, not when the work task finally unwinds
    }

    func fail(_ e: Error, action: (title: String, run: @MainActor () async -> Void)? = nil) {
        error = ErrorPresenter.present(e)
        errorAction = action
    }

    public func dismissError() { error = nil; errorAction = nil }

    // MARK: Refresh

    private var refreshTask: Task<Void, Never>?
    private var refreshPending = false

    /// Reloads status, branches and connections (and the history when HEAD moved). Concurrent callers
    /// are coalesced: at most one refresh runs and at most one more is queued behind it.
    public func refresh() async {
        if let t = refreshTask {
            refreshPending = true
            await t.value
            return
        }
        let t = Task { [weak self] in
            guard let self else { return }
            repeat {
                self.refreshPending = false
                self.isRefreshing = true
                await self.reload()
            } while self.refreshPending
            self.isRefreshing = false
            self.refreshTask = nil
        }
        refreshTask = t
        await t.value
    }

    private func reload() async {
        let client = self.client
        do {
            async let st = client.status()
            async let br = client.branches()
            async let rm = client.remotes()
            let (s, b, r) = try await (st, br, rm)
            apply(status: s)
            branches = b; remotes = r
            if logHead == nil || logHead! != s.head {
                if s.head != nil {
                    let log = try await client.log(limit: 300)
                    commits = log
                    graph = Dictionary(uniqueKeysWithValues: GraphLayout.rows(for: log).map { ($0.commit, $0) })
                } else { commits = []; graph = [:] }
                logHead = .some(s.head)
            }
        } catch {
            // A failed refresh must not hide a more useful earlier error.
            if self.error == nil { fail(error) }
        }
    }

    private func apply(status s: RepoStatus) {
        let old = status
        status = s
        fullDiffText = nil
        if !unselected.isEmpty {
            let present = Set(s.changes.map(\.path))
            unselected.formIntersection(present)
        }
        if let f = focus, !s.changes.contains(where: { $0.path == f }) { focus = nil; focusDiff = nil; diffState = .idle }
        else if focus != nil, old?.changes != s.changes { loadDiff() }
    }

    public func refreshStorage() async {
        if let s = try? await client.stats() { stats = s }
        if let sh = try? await client.shelves() { shelves = sh }
    }

    // MARK: Selection

    /// Test hook: installs a status without an engine.
    func setStatusForTesting(_ s: RepoStatus) { apply(status: s) }

    public func isSelected(_ path: String) -> Bool { !unselected.contains(path) }

    public func setSelected(_ path: String, _ on: Bool) {
        if on { unselected.remove(path) } else { unselected.insert(path) }
    }

    public func selectAll() { unselected = [] }

    public func selectNone() { unselected = Set(status?.changes.map(\.path) ?? []) }

    /// Include list, exclude list or everything: whichever is shorter, never a long argv.
    func commitSelection() -> RgitClient.CommitSelection {
        guard let changes = status?.changes, !unselected.isEmpty, !isMerging else { return .all }
        // A rename removes the old path as well: both ends follow the new path's choice.
        if unselected.count <= changes.count - unselected.count {
            var out: [String] = []; out.reserveCapacity(unselected.count)
            for c in changes where unselected.contains(c.path) { out.append(c.path); if let f = c.from { out.append(f) } }
            return .allExcept(out)
        }
        var out: [String] = []; out.reserveCapacity(changes.count - unselected.count)
        for c in changes where !unselected.contains(c.path) { out.append(c.path); if let f = c.from { out.append(f) } }
        return .only(out)
    }

    // MARK: Lazy diff of the focused file

    public func setFocus(_ path: String?) {
        guard path != focus else { return }
        focus = path
        loadDiff()
    }

    private static let slowDiffThreshold = 2000

    func loadDiff() {
        diffTask?.cancel()
        guard let path = focus, let change = status?.changes.first(where: { $0.path == path }) else {
            focusDiff = nil; diffState = .idle; return
        }
        let root = url, client = self.client
        let cached = fullDiffText
        let big = changeCount >= Self.slowDiffThreshold
        if change.kind == .added {
            // Nothing to compare against: the file itself is the diff. No engine call needed.
            diffState = .loading(slow: false)
            diffTask = Task { [weak self] in
                let d = await Task.detached(priority: .userInitiated) { DiffParser.synthesizeAdded(path: path, root: root) }.value
                guard !Task.isCancelled, let self, self.focus == path else { return }
                self.focusDiff = d; self.diffState = .idle
            }
            return
        }
        diffState = .loading(slow: big && cached == nil)
        diffTask = Task { [weak self] in
            do {
                let text: String
                if let cached { text = cached } else { text = try await client.diff().diff }
                guard !Task.isCancelled, let self else { return }
                self.fullDiffText = text
                let d = await Task.detached(priority: .userInitiated) { DiffParser.section(of: text, path: path) }.value
                guard !Task.isCancelled, self.focus == path else { return }
                self.focusDiff = d; self.diffState = .idle
            } catch {
                guard !Task.isCancelled, let self, self.focus == path else { return }
                if let r = error as? RgitError, r == .cancelled { return }
                self.diffState = .failed(ErrorPresenter.present(error).detail)
            }
        }
    }

    // MARK: Changes

    public func commit(message: String) async -> Bool {
        let msg = message.trimmingCharacters(in: .whitespacesAndNewlines)
        let selection = commitSelection()
        let r = await run(L("Committing…")) { c, _ in try await c.commit(message: msg, selection: selection) }
        if let r { lastMessage = LF("Committed %@ on %@.", r.short, r.branch) }
        return r != nil
    }

    /// Keep project-changing actions locked until both steps finish. A failed send leaves the
    /// successful commit intact, clears its draft, and offers a send-only retry.
    @discardableResult
    public func commitAndShare(message: String) async -> Bool {
        guard !commitAndShareInProgress, !blocksChanges, !isRunning(.push) else { return false }
        guard isMerging || selectedCount > 0 else {
            fail(RgitError.engine(code: .nothingToCommit, message: L("Pick at least one file."), paths: []))
            return false
        }
        guard let remote = remotes.first(where: { $0.name == "origin" })?.name ?? remotes.first?.name else {
            return await commit(message: message)
        }
        commitAndShareInProgress = true
        defer { commitAndShareInProgress = false }
        let msg = message.trimmingCharacters(in: .whitespacesAndNewlines)
        let selection = commitSelection()
        var saved = false
        let done: Bool? = await run(L("Committing and sharing…"), cancellable: true) { [weak self] c, progress in
            let committed = try await c.commit(message: msg, selection: selection)
            await MainActor.run {
                saved = true
                self?.pendingShareRemote = remote
                if self?.commitMessageDraft.trimmingCharacters(in: .whitespacesAndNewlines) == msg {
                    self?.commitMessageDraft = ""
                }
                self?.lastMessage = L("Committed locally. Sending to the host…")
            }
            _ = try await c.push(remote: remote, progress: progress)
            await MainActor.run {
                self?.lastPushedHead = committed.id
                self?.pushedAt = Date()
                self?.syncState = .upToDate
                self?.pendingShareRemote = nil
            }
            return true
        }
        if done == true {
            lastMessage = L("Committed and shared. Other devices can now pull this version.")
        } else if saved {
            var problem = error ?? ErrorPresenter.present(RgitError.cancelled)
            problem.title = L("Committed locally, but not shared")
            problem.suggestion = L("Your commit is safe. Retry sharing; you do not need to commit again.")
            error = problem
            lastMessage = nil
            if problem.code == .refConflict {
                errorAction = (L("Fetch and merge now"), { [weak self] in await self?.pull() })
            } else {
                errorAction = (L("Retry sharing"), { [weak self] in await self?.push() })
            }
        }
        return saved
    }

    // MARK: Branches and history

    public func createBranch(_ name: String, from revision: String?, switchTo: Bool) async {
        _ = await run(L("Creating branch…")) { c, _ in
            if switchTo && revision == nil { _ = try await c.switchBranch(name, create: true) }
            else {
                try await c.createBranch(name, from: revision)
                if switchTo { _ = try await c.switchBranch(name) }
            }
        }
    }

    public func switchBranch(_ name: String) async {
        _ = await run(LF("Switching to %@…", name)) { c, _ in try await c.switchBranch(name) }
    }

    public func deleteBranch(_ name: String, force: Bool = false) async {
        let ok: Bool? = await run(LF("Deleting %@…", name)) { c, _ in try await c.deleteBranch(name, force: force); return true }
        if ok == nil, error != nil, !force {
            errorAction = (L("Delete anyway"), { [weak self] in await self?.deleteBranch(name, force: true) })
        }
    }

    /// Returns true when the merge stopped on conflicts.
    @discardableResult
    public func merge(_ branch: String) async -> Bool {
        let r = await run(LF("Merging %@…", branch)) { c, _ in try await c.merge(branch) }
        guard let r else { return false }
        if !r.conflicts.isEmpty { lastMessage = LF("Merge stopped: %d files need your decision.", r.conflicts.count) }
        else if r.alreadyUpToDate { lastMessage = L("Already up to date.") }
        else { lastMessage = L("Merged.") }
        return !r.conflicts.isEmpty
    }

    public func abortMerge() async { _ = await run(L("Aborting merge…")) { c, _ in try await c.abortMerge() } }

    public func resolve(_ path: String, _ how: RgitClient.Resolution) async {
        _ = await run(LF("Resolving %@…", path)) { c, _ in try await c.resolve(path, with: how) }
    }

    public func revert(_ commit: CommitInfo) async {
        let r = await run(LF("Reverting %@…", commit.short)) { c, _ in try await c.revert(commit.id) }
        if let r, !r.conflicts.isEmpty { lastMessage = LF("Revert stopped: %d files need your decision.", r.conflicts.count) }
        else if r != nil { lastMessage = LF("Reverted %@.", commit.short) }
    }

    public func restore(paths: [String], from commit: CommitInfo, force: Bool = false) async {
        let r = await run(L("Restoring…")) { c, _ in try await c.restore(paths: paths, from: commit.id, force: force) }
        if let r { lastMessage = LF("Restored %d files from %@.", r.restored?.count ?? paths.count, commit.short) }
        else if error?.code == .dirtyWorkingTree, !force {
            errorAction = (L("Restore anyway (keeps a recovery copy)"), { [weak self] in
                await self?.restore(paths: paths, from: commit, force: true)
            })
        }
    }

    public func restoreVersion(_ commit: CommitInfo, force: Bool = false) async {
        let result = await run(L("Restoring…"), cancellable: true) { client, _ in
            try await client.restore(paths: ["."], from: commit.id, force: force)
        }
        if let result {
            lastMessage = result.restored?.isEmpty == true ? L("Your tracked files already match this version.") : L("Files restored. Review them in Changes, then commit when ready.")
        } else if error?.code == .dirtyWorkingTree, !force {
            errorAction = (L("Restore anyway (keeps a recovery copy)"), { [weak self] in
                await self?.restoreVersion(commit, force: true)
            })
        }
    }

    public func showCommit(_ id: String) async -> ShowResult? { try? await client.show(id) }

    // MARK: Sync

    public func fetch() async {
        let r = await run(L("Checking for updates…"), kind: .fetch, cancellable: true, refreshAfter: false) { c, p in try await c.fetch(progress: p) }
        if let r { syncState = r.state ?? syncState; lastFetch = Date() }
    }

    public func push() async {
        guard let remote = pendingShareRemote ?? remotes.first(where: { $0.name == "origin" })?.name ?? remotes.first?.name else { return }
        let before = status?.head
        let r = await run(L("Sharing your commits…"), kind: .push, cancellable: true, refreshAfter: false) { c, p in try await c.push(remote: remote, progress: p) }
        if let r {
            if pendingShareRemote == remote { pendingShareRemote = nil }
            lastPushedHead = before
            pushedAt = Date()
            syncState = .upToDate
            lastMessage = r.upToDate ? L("Everything was already shared.") : L("Shared with the host.")
        } else if error?.code == .refConflict {
            errorAction = (L("Fetch and merge now"), { [weak self] in await self?.pull() })
        }
    }

    public func pull() async {
        let r = await run(L("Getting updates…"), kind: .pull, cancellable: true) { c, p in try await c.pull(progress: p) }
        if let r {
            syncState = r.fetch.state ?? syncState
            lastFetch = Date()
            if let m = r.merge, !m.conflicts.isEmpty { lastMessage = LF("Merge stopped: %d files need your decision.", m.conflicts.count) }
            else { lastMessage = L("You have the latest work.") }
        }
    }

    public func addRemote(name: String, url: String, hostKey: String? = nil) async {
        _ = await run(L("Adding connection…")) { c, _ in try await c.addRemote(name: name, url: url, hostKey: hostKey) }
    }
    public func removeRemote(_ name: String) async {
        _ = await run(L("Removing connection…")) { c, _ in try await c.removeRemote(name: name) }
    }

    // MARK: Storage

    public func verify() async {
        verifyResult = nil
        if let r = await run(L("Verifying every object…"), kind: .verify, cancellable: true, refreshAfter: false, { c, p in try await c.verify(progress: p) }) {
            verifyResult = r
        }
        await refreshStorage()
    }

    public func gc(aggressive: Bool) async {
        gcResult = nil
        if let r = await run(L("Cleaning up storage…"), kind: .maintenance, cancellable: true, refreshAfter: false, { c, p in try await c.gc(aggressive: aggressive, progress: p) }) {
            gcResult = r
        }
        await refreshStorage()
    }

    public func reindex() async {
        reindexResult = nil
        if let r = await run(L("Rebuilding the index…"), kind: .maintenance, refreshAfter: false, { c, _ in try await c.reindex() }) { reindexResult = r }
        await refreshStorage()
    }

    // MARK: Auto-update

    private var pollTask: Task<Void, Never>?
    /// Fetches periodically while a connection exists. Fetch never touches working files.
    public func startPolling(every seconds: Double = 60) {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(seconds))
                guard let self, !Task.isCancelled else { return }
                if self.hasRemote, !self.activities.contains(where: { $0.kind == .fetch || $0.kind == .pull }), !self.isMerging {
                    if let r = try? await self.client.fetch() { self.syncState = r.state ?? self.syncState; self.lastFetch = Date() }
                }
            }
        }
    }
    public func stopPolling() { pollTask?.cancel(); pollTask = nil }
}
