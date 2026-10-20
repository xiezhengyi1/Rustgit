import Foundation

/// Typed, async interface to the bundled `rgit --json` (docs/gui-interface.md).
///
/// Every call spawns one `rgit` process. Cancelling the calling `Task` terminates that process;
/// the engine resumes interrupted transfers on the next run. Safe to use from any actor.
public struct RgitClient: Sendable {
    /// The `json_interface` this adapter was written against. `checkCompatibility()` refuses others.
    public static let supportedJSONInterface = 1

    public var executable: URL
    public var directory: URL?
    public var environment: [String: String]?

    public init(executable: URL, directory: URL? = nil, environment: [String: String]? = nil) {
        self.executable = executable
        self.directory = directory
        self.environment = environment
    }

    public func inDirectory(_ dir: URL) -> RgitClient {
        var c = self
        c.directory = dir
        return c
    }

    // MARK: Core

    static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    func arguments(_ args: [String]) -> [String] {
        var a = ["--json"]
        if let directory { a += ["-C", directory.path] }
        return a + args
    }

    private struct ErrorBody: Decodable { var code: String; var message: String; var paths: [String]? }
    private struct Head: Decodable { var ok: Bool; var error: ErrorBody? }
    private struct Envelope<T: Decodable>: Decodable { var data: T }
    /// For commands whose `data` is `{}` or irrelevant.
    public struct Empty: Decodable, Sendable { public init() {} }

    /// Runs one command and decodes `data`. Throws `RgitError`.
    public func run<T: Decodable & Sendable>(_ args: [String], as type: T.Type = T.self,
                                              progress: ProgressHandler? = nil, input: Data? = nil) async throws -> T {
        let outcome = try await runToCompletion(executable: executable, arguments: arguments(args),
                                                environment: environment, onEvent: progress, input: input)
        if outcome.cancelled { throw RgitError.cancelled }
        guard let line = outcome.resultLine else {
            throw RgitError.processFailed(exitCode: outcome.exitCode, stderr: outcome.stderr)
        }
        let head: Head
        do { head = try Self.decoder.decode(Head.self, from: line) }
        catch { throw RgitError.malformedOutput("\(error)") }
        if !head.ok {
            if let obj = try? JSONSerialization.jsonObject(with: line) as? [String: Any],
               let error = obj["error"] as? [String: Any], var connection = error["connection"] as? [String: Any] {
                connection["event"] = "connection"
                if let data = try? JSONSerialization.data(withJSONObject: connection), case .event(let event) = WireLine.parse(data) { progress?(event) }
            }
            let e = head.error
            throw RgitError.engine(code: RgitErrorCode(wire: e?.code ?? "other"),
                                   message: e?.message ?? outcome.stderr, paths: e?.paths ?? [])
        }
        do { return try Self.decoder.decode(Envelope<T>.self, from: line).data }
        catch { throw RgitError.malformedOutput("\(args.first ?? "?"): \(error)") }
    }

    // MARK: Version

    public func version() async throws -> VersionInfo {
        try await inDirectory0().run(["version"])
    }

    private func inDirectory0() -> RgitClient { var c = self; c.directory = nil; return c }

    /// Throws `.incompatibleInterface` unless the binary speaks the interface this adapter knows.
    @discardableResult
    public func checkCompatibility() async throws -> VersionInfo {
        let v = try await version()
        guard v.jsonInterface == Self.supportedJSONInterface else {
            throw RgitError.incompatibleInterface(found: v.jsonInterface, expected: Self.supportedJSONInterface)
        }
        return v
    }

    // MARK: Project

    public func initProject(branch: String = "main") async throws -> InitResult {
        try await run(["init", "--branch", branch])
    }

    public func status() async throws -> RepoStatus { try await run(["status"]) }

    /// What `commit` should include.
    public enum CommitSelection: Sendable, Equatable {
        /// Every change.
        case all
        /// Only these paths (relative, `/`-separated). Sent through a file, never argv, so any count works.
        case only([String])
        /// Every change except these paths. Use when most of a large change set is selected.
        case allExcept([String])
    }

    /// Commits only `paths` (relative, `/`-separated) or everything when empty.
    public func commit(message: String, paths: [String] = []) async throws -> CommitResult {
        try await commit(message: message, selection: paths.isEmpty ? .all : .only(paths))
    }

    /// Commit with an include list or an exclude list of any size (tens of thousands of paths are fine:
    /// the list is handed to `rgit` through a temporary file with `--paths-from` / `--except-from`).
    public func commit(message: String, selection: CommitSelection) async throws -> CommitResult {
        let base = ["commit", "--message=\(message)"]
        switch selection {
        case .all:
            return try await run(base)
        case .only(let paths):
            return try await withPathFile(paths) { try await run(base + ["--paths-from", $0.path]) }
        case .allExcept(let paths):
            return try await withPathFile(paths) { try await run(base + ["--except-from", $0.path]) }
        }
    }

    private func withPathFile<T>(_ paths: [String], _ body: (URL) async throws -> T) async throws -> T {
        if paths.contains(where: { $0.contains("\n") }) {
            throw RgitError.engine(code: .invalid, message: "a path contains a line break and cannot be listed", paths: paths.filter { $0.contains("\n") })
        }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("rgit-paths-\(UUID().uuidString).txt")
        try (paths.joined(separator: "\n") + "\n").write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }
        return try await body(url)
    }

    public func log(limit: Int = 50, revision: String? = nil) async throws -> [CommitInfo] {
        let r: LogResult = try await run(["log", "-n", String(limit)] + (revision.map { [$0] } ?? []))
        return r.commits
    }

    public func show(_ revision: String) async throws -> ShowResult { try await run(["show", revision]) }

    /// Working tree vs HEAD, or `a` vs working tree / `a` vs `b`.
    public func diff(_ a: String? = nil, _ b: String? = nil) async throws -> DiffResult {
        try await run(["diff"] + [a, b].compactMap { $0 })
    }

    public func config(_ key: String) async throws -> String {
        struct R: Decodable, Sendable { var value: String }
        return try await run(["config", key], as: R.self).value
    }

    public func setConfig(_ key: String, _ value: String) async throws {
        _ = try await run(["config", key, value], as: Empty.self)
    }

    // MARK: Branches

    public func branches() async throws -> BranchList { try await run(["branch"]) }

    @discardableResult
    public func createBranch(_ name: String, from revision: String? = nil) async throws -> BranchRef {
        try await run(["branch"] + (revision.map { ["--from", $0] } ?? []) + ["--", name])
    }

    @discardableResult
    public func deleteBranch(_ name: String, force: Bool = false) async throws -> BranchRef {
        try await run(["branch", "-d"] + (force ? ["--force"] : []) + ["--", name])
    }

    @discardableResult
    public func switchBranch(_ name: String, create: Bool = false) async throws -> SwitchResult {
        try await run(["switch"] + (create ? ["-c"] : []) + ["--", name])
    }

    // MARK: History operations

    /// Restores files from `revision` (HEAD when nil). `force` overwrites edits (a recovery snapshot is kept).
    public func restore(paths: [String], from revision: String? = nil, force: Bool = false) async throws -> MergeResult {
        try await run(["restore"] + (revision.map { ["--from", $0] } ?? []) + (force ? ["--force"] : []) + ["--"] + paths)
    }

    public func revert(_ revision: String) async throws -> MergeResult { try await run(["revert", revision]) }

    /// A result with non-empty `conflicts` is not an error: resolve them, then `commit`.
    public func merge(_ branch: String) async throws -> MergeResult { try await run(["merge", "--", branch]) }

    public func abortMerge() async throws -> MergeResult { try await run(["merge", "--abort"]) }

    public enum Resolution: Sendable { case ours, theirs, workingTree }

    /// `.workingTree` marks the file resolved with whatever the user left in it ("edit").
    public func resolve(_ path: String, with resolution: Resolution) async throws -> ResolveResult {
        let flag: [String] = switch resolution {
        case .ours: ["--ours"]
        case .theirs: ["--theirs"]
        case .workingTree: []
        }
        return try await run(["resolve"] + flag + ["--", path])
    }

    public func shelve(message: String = "") async throws -> String {
        struct R: Decodable, Sendable { var name: String }
        return try await run(["shelve", "--message=\(message)"], as: R.self).name
    }

    public func shelves() async throws -> ShelfList { try await run(["shelves"]) }
    /// `id` may be the name returned by `shelve` ("shelf/1") or the bare number ("1").
    public func unshelve(_ id: String) async throws -> MergeResult {
        try await run(["unshelve", id.hasPrefix("shelf/") ? String(id.dropFirst("shelf/".count)) : id])
    }

    // MARK: Storage

    public func stats() async throws -> RepoStats { try await run(["stats"]) }

    /// `problems` non-empty is a successful run that found damage (the CLI exits 2).
    public func verify(progress: ProgressHandler? = nil) async throws -> VerifyResult {
        try await run(["verify"], progress: progress)
    }

    public func gc(aggressive: Bool = false, progress: ProgressHandler? = nil) async throws -> GcResult {
        try await run(["gc"] + (aggressive ? ["--aggressive"] : []), progress: progress)
    }

    public func reindex() async throws -> ReindexResult { try await run(["reindex"]) }

    // MARK: Remotes and sync

    public func remotes() async throws -> [RemoteInfo] {
        let r: RemoteList = try await run(["remote"])
        return r.remotes
    }

    public func addRemote(name: String, url: String, hostKey: String? = nil) async throws {
        _ = try await run(["remote", "add", name, url] + (hostKey.map { ["--host-key", $0] } ?? []), as: Empty.self)
    }

    public func removeRemote(name: String) async throws {
        _ = try await run(["remote", "remove", name], as: Empty.self)
    }

    /// Clone `url` (e.g. `tcp://host:port`) into `destination`. The client's directory is ignored.
    public func clone(url: String, into destination: URL, hostKey: String? = nil, progress: ProgressHandler? = nil) async throws -> CloneResult {
        try await inDirectory0().run(["clone", url, destination.path] + (hostKey.map { ["--host-key", $0] } ?? []), progress: progress)
    }

    public func fetch(remote: String = "origin", progress: ProgressHandler? = nil) async throws -> SyncResult {
        try await run(["fetch", remote], progress: progress)
    }

    public func push(remote: String = "origin", branch: String? = nil, progress: ProgressHandler? = nil) async throws -> SyncResult {
        try await run(["push", remote] + (branch.map { [$0] } ?? []), progress: progress)
    }

    public func pull(remote: String = "origin", progress: ProgressHandler? = nil) async throws -> PullResult {
        try await run(["pull", remote], progress: progress)
    }
}

// MARK: Authenticated sharing
extension RgitClient {
    public func deviceIdentity() async throws -> DeviceIdentity { try await run(["device", "id"]) }
    public func hostIdentity() async throws -> HostIdentity { try await run(["host", "id"]) }
    public func createInvitation(role: String = "read", expires: String = "24h", singleUse: Bool = true,
                                 label: String = "", hints: [String]) async throws -> CreatedInvitation {
        var args = ["invite", "create", "--role", role, "--expires", expires, "--single-use=\(singleUse)"]
        if !label.isEmpty { args += ["--label", label] }
        for hint in hints { args += ["--hint", hint] }
        return try await run(args)
    }
    public func invitations() async throws -> InvitationList { try await run(["invite", "list"]) }
    public func revokeInvitation(_ id: String) async throws {
        _ = try await run(["invite", "revoke", id], as: Empty.self)
    }
    public func devices() async throws -> DeviceList { try await run(["devices", "list"]) }
    public func revokeDevice(_ fingerprint: String) async throws {
        _ = try await run(["devices", "revoke", fingerprint], as: Empty.self)
    }
    public func setDeviceRole(_ fingerprint: String, role: String) async throws {
        _ = try await run(["devices", "role", fingerprint, role], as: Empty.self)
    }
    /// Invitation secrets travel through stdin, never argv or logs.
    public func join(invitation: String, into destination: URL, progress: ProgressHandler? = nil) async throws -> JoinResult {
        try await inDirectory0().run(["join", "-", destination.path], progress: progress,
                                    input: Data((invitation.trimmingCharacters(in: .whitespacesAndNewlines) + "\n").utf8))
    }
    public func enroll(invitation: String, remote: String = "origin") async throws -> JoinResult {
        try await run(["join", "-", "--add-remote", "--remote", remote],
                      input: Data((invitation + "\n").utf8))
    }
    public func watchDiscovery(onEvent: @escaping ProgressHandler) async throws {
        let _: Empty = try await inDirectory0().run(["discover", "--watch"], progress: onEvent)
    }
    public func discover(timeout: Int = 3) async throws -> DiscoveryResult {
        try await inDirectory0().run(["discover", "--timeout", String(timeout)])
    }
}
