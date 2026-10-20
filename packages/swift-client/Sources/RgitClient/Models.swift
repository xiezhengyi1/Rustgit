import Foundation

// Decodable mirrors of the `data` objects in docs/gui-interface.md. The decoder uses
// `.convertFromSnakeCase`; fields the engine may omit are optional so that an additive change
// in the engine does not break the apps.

public struct VersionInfo: Codable, Sendable, Equatable {
    public var cli: String
    public var jsonInterface: Int
    public var format: Int
}

public struct InitResult: Codable, Sendable, Equatable {
    public var uuid: String
    public var root: String
}

public enum ChangeKind: Hashable, Sendable, Codable {
    case added, modified, deleted, renamed
    case unknown(String)

    public init(from decoder: Decoder) throws {
        let s = try decoder.singleValueContainer().decode(String.self)
        switch s {
        case "added": self = .added
        case "modified": self = .modified
        case "deleted": self = .deleted
        case "renamed": self = .renamed
        default: self = .unknown(s)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .added: try c.encode("added")
        case .modified: try c.encode("modified")
        case .deleted: try c.encode("deleted")
        case .renamed: try c.encode("renamed")
        case .unknown(let s): try c.encode(s)
        }
    }
}

public struct Change: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var path: String
    public var kind: ChangeKind
    public var from: String?
    public var id: String { path }
    public init(path: String, kind: ChangeKind, from: String? = nil) { self.path = path; self.kind = kind; self.from = from }
}

public struct SkippedPath: Codable, Sendable, Equatable, Hashable {
    public var path: String
    public var reason: String
}

public struct MergeState: Codable, Sendable, Equatable {
    public var kind: String
    public var label: String
}

public struct ConflictEntry: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var path: String
    public var kind: String
    public var resolved: Bool
    public var id: String { path }
}

public struct RepoStatus: Codable, Sendable, Equatable {
    public var branch: String?
    public var head: String?
    public var changes: [Change]
    public var skipped: [SkippedPath]
    public var merge: MergeState?
    public var conflicts: [ConflictEntry]

    public init(branch: String?, head: String?, changes: [Change] = [], skipped: [SkippedPath] = [],
                merge: MergeState? = nil, conflicts: [ConflictEntry] = []) {
        self.branch = branch; self.head = head; self.changes = changes; self.skipped = skipped
        self.merge = merge; self.conflicts = conflicts
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        branch = try c.decodeIfPresent(String.self, forKey: .branch)
        head = try c.decodeIfPresent(String.self, forKey: .head)
        changes = try c.decodeIfPresent([Change].self, forKey: .changes) ?? []
        skipped = try c.decodeIfPresent([SkippedPath].self, forKey: .skipped) ?? []
        merge = try c.decodeIfPresent(MergeState.self, forKey: .merge)
        conflicts = try c.decodeIfPresent([ConflictEntry].self, forKey: .conflicts) ?? []
    }
}

public struct CommitInfo: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var id: String
    public var short: String
    public var parents: [String]
    public var authorName: String
    public var authorEmail: String
    public var timestamp: Int64
    public var tzOffsetMin: Int
    public var message: String
    public var branches: [String]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        short = try c.decodeIfPresent(String.self, forKey: .short) ?? String(id.prefix(10))
        parents = try c.decodeIfPresent([String].self, forKey: .parents) ?? []
        authorName = try c.decodeIfPresent(String.self, forKey: .authorName) ?? ""
        authorEmail = try c.decodeIfPresent(String.self, forKey: .authorEmail) ?? ""
        timestamp = try c.decodeIfPresent(Int64.self, forKey: .timestamp) ?? 0
        tzOffsetMin = try c.decodeIfPresent(Int.self, forKey: .tzOffsetMin) ?? 0
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        branches = try c.decodeIfPresent([String].self, forKey: .branches) ?? []
    }

    public var date: Date { Date(timeIntervalSince1970: TimeInterval(timestamp)) }
    public var subject: String { message.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: true).first.map(String.init) ?? "" }
}

public struct LogResult: Codable, Sendable, Equatable { public var commits: [CommitInfo] }

public struct ShowResult: Codable, Sendable, Equatable {
    public var commit: CommitInfo
    public var changes: [Change]
    public var diff: String
}

public struct DiffResult: Codable, Sendable, Equatable {
    public var changes: [Change]
    public var diff: String
}

public struct BranchInfo: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var name: String
    public var id: String
    public var short: String?
    public var shortId: String { short ?? String(id.prefix(10)) }
}

public struct BranchList: Codable, Sendable, Equatable {
    public var current: String
    public var branches: [BranchInfo]
}

public struct BranchRef: Codable, Sendable, Equatable { public var name: String; public var id: String }
public struct SwitchResult: Codable, Sendable, Equatable { public var branch: String }
public struct ResolveResult: Codable, Sendable, Equatable { public var path: String }
public struct CommitResult: Codable, Sendable, Equatable { public var id: String; public var short: String; public var branch: String }

/// Result of merge / revert / restore / unshelve. `conflicts` non-empty means the merge stopped
/// and needs resolution (the CLI exits 1 but `ok` is true).
public struct MergeResult: Codable, Sendable, Equatable {
    public var commit: String?
    public var fastForward: Bool
    public var alreadyUpToDate: Bool
    public var conflicts: [String]
    public var restored: [String]?
    public var aborted: Bool?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        commit = try c.decodeIfPresent(String.self, forKey: .commit)
        fastForward = try c.decodeIfPresent(Bool.self, forKey: .fastForward) ?? false
        alreadyUpToDate = try c.decodeIfPresent(Bool.self, forKey: .alreadyUpToDate) ?? false
        conflicts = try c.decodeIfPresent([String].self, forKey: .conflicts) ?? []
        restored = try c.decodeIfPresent([String].self, forKey: .restored)
        aborted = try c.decodeIfPresent(Bool.self, forKey: .aborted)
    }
}

public struct RepoStats: Codable, Sendable, Equatable {
    public var commits: Int
    public var branches: Int
    public var objects: Int
    public var logicalBytes: Int64
    public var storedBytes: Int64
    public var deltaObjects: Int
    public var packFiles: Int
    public var packBytes: Int64
    public var indexBytes: Int64
    public var repoBytes: Int64

    /// logical / stored; nil for an empty store.
    public var dedupRatio: Double? { storedBytes > 0 ? Double(logicalBytes) / Double(storedBytes) : nil }
}

public struct VerifyResult: Codable, Sendable, Equatable {
    public var objectsChecked: Int
    public var commits: Int
    public var problems: [String]
}

public struct GcResult: Codable, Sendable, Equatable {
    public var moved: Int
    public var dropped: Int
    public var freedBytes: Int64
    public var repoBytesBefore: Int64
    public var repoBytesAfter: Int64
}

public struct ReindexResult: Codable, Sendable, Equatable {
    public var objects: Int
    public var packs: Int
    public var problems: [String]
}

public struct RemoteInfo: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var name: String
    public var url: String
    public var secure: Bool?
    public var hostKey: String?
    public var id: String { name }
}
public struct RemoteList: Codable, Sendable, Equatable { public var remotes: [RemoteInfo] }

public struct Shelf: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var ref: String
    public var id: String
    public var name: String?
    public var message: String?
}
public struct ShelfList: Codable, Sendable, Equatable {
    public var shelves: [Shelf]
    public var recovery: [Shelf]
}

public struct RefUpdate: Codable, Sendable, Equatable {
    public var ref: String
    public var old: String?
    public var new: String?
    public var generation: UInt64?
}

public struct TransferStats: Codable, Sendable, Equatable {
    public var objectsTransferred: Int?
    public var objectsAlreadyComplete: Int?
    public var objectsResumed: Int?
    public var logicalBytes: Int64?
    public var wireBytesSent: Int64?
    public var wireBytesReceived: Int64?
}

/// Relation of the local branch to the host's branch after a fetch.
public enum SyncState: Hashable, Sendable, Codable {
    case upToDate, behind, ahead, diverged, noLocalCommits
    case unknown(String)

    public init(from decoder: Decoder) throws {
        let s = try decoder.singleValueContainer().decode(String.self)
        self = SyncState(wire: s)
    }
    public init(wire s: String) {
        switch s {
        case "up_to_date": self = .upToDate
        case "behind": self = .behind
        case "ahead": self = .ahead
        case "diverged": self = .diverged
        case "no_local_commits": self = .noLocalCommits
        default: self = .unknown(s)
        }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .upToDate: try c.encode("up_to_date")
        case .behind: try c.encode("behind")
        case .ahead: try c.encode("ahead")
        case .diverged: try c.encode("diverged")
        case .noLocalCommits: try c.encode("no_local_commits")
        case .unknown(let s): try c.encode(s)
        }
    }
}

/// `fetch` / `push` result.
public struct SyncResult: Codable, Sendable, Equatable {
    public var upToDate: Bool
    public var updated: [RefUpdate]
    public var stats: TransferStats?
    public var state: SyncState?
    public var alreadyApplied: Bool?
    public var connection: ConnectionInfo?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        upToDate = try c.decodeIfPresent(Bool.self, forKey: .upToDate) ?? false
        updated = try c.decodeIfPresent([RefUpdate].self, forKey: .updated) ?? []
        stats = try c.decodeIfPresent(TransferStats.self, forKey: .stats)
        state = try c.decodeIfPresent(SyncState.self, forKey: .state)
        alreadyApplied = try c.decodeIfPresent(Bool.self, forKey: .alreadyApplied)
        connection = try c.decodeIfPresent(ConnectionInfo.self, forKey: .connection)
    }
}

/// `pull` result: a fetch followed by an optional merge.
public struct PullResult: Codable, Sendable, Equatable {
    public var fetch: SyncResult
    public var merge: MergeResult?
}

public struct CloneResult: Codable, Sendable, Equatable {
    public var connection: ConnectionInfo?
    public var dest: String
    public var branch: String?
    public var head: String?
}

/// Progress or other NDJSON event printed before the final result.
public struct ProgressEvent: Sendable, Equatable {
    public var event: String
    public var phase: String?
    public var done: Int64?
    public var total: Int64?
    public var raw: [String: String]
    public var connection: ConnectionInfo? = nil
    public var host: DiscoveredHost? = nil

    /// 0...1 when the total is known.
    public var fraction: Double? {
        guard let done, let total, total > 0 else { return nil }
        return min(1, Double(done) / Double(total))
    }
}

/// Events of a running `rgit serve`.
public enum ServeEvent: Sendable, Equatable {
    case listening(addr: String, project: String)
    case connection(String)
    case push(String)
    case error(String)
    case other(event: String, line: String)
    case exited(status: Int32)
}

public struct ConnectionInfo: Codable, Sendable, Equatable {
    public var state: String
    public var path: String?
    public var error: String?
    public var usedRelay: Bool?
    public var usedDirect: Bool?
    public var pathSwitches: Int?
}
public struct DeviceIdentity: Codable, Sendable, Equatable {
    public var fingerprint: String
    public var keyStorage: String
    public var keyStorageProtected: Bool
}
public struct HostIdentity: Codable, Sendable, Equatable {
    public var fingerprint: String
    public var project: String
}
public struct CreatedInvitation: Decodable, Sendable {
    public var uri: String
    public var id: String
    public var hostFingerprint: String
}
public struct Invitation: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var role: String
    public var status: String
    public var label: String
    public var expires: Int64?
    public var uses: Int
    public var maxUses: Int?
}
public struct InvitationList: Codable, Sendable { public var invitations: [Invitation] }
public struct EnrolledDevice: Codable, Sendable, Equatable, Identifiable {
    public var fingerprint: String
    public var name: String
    public var role: String
    public var status: String
    public var lastSeen: Int64?
    public var id: String { fingerprint }
}
public struct DeviceList: Codable, Sendable { public var devices: [EnrolledDevice] }
public struct JoinResult: Codable, Sendable {
    public var dest: String?
    public var hostFingerprint: String
    public var deviceFingerprint: String
    public var role: String
    public var connection: ConnectionInfo?
}
public struct DiscoveredHost: Codable, Sendable, Equatable, Identifiable {
    public var instance: String
    public var urls: [String]
    public var reachable: Bool?
    public var verified: Bool
    public var id: String { instance }
}
public struct DiscoveryResult: Codable, Sendable { public var hosts: [DiscoveredHost] }
