import Foundation

/// Error codes of `rgit --json` (docs/gui-interface.md). Unknown codes are kept verbatim so
/// a newer engine never makes the app crash or lose the message.
public enum RgitErrorCode: Hashable, Sendable {
    case notARepository, dirtyWorkingTree, nothingToCommit, mergeInProgress, noMerge
    case unresolvedConflicts, branchNotFound, branchExists, invalidName, unportablePaths
    case refConflict, unknownRevision, hostUnreachable, permissionDenied, quotaExceeded
    case outOfSpace, invalid, other
    case unknown(String)

    public init(wire: String) {
        switch wire {
        case "not_a_repository": self = .notARepository
        case "dirty_working_tree": self = .dirtyWorkingTree
        case "nothing_to_commit": self = .nothingToCommit
        case "merge_in_progress": self = .mergeInProgress
        case "no_merge": self = .noMerge
        case "unresolved_conflicts": self = .unresolvedConflicts
        case "branch_not_found": self = .branchNotFound
        case "branch_exists": self = .branchExists
        case "invalid_name": self = .invalidName
        case "unportable_paths": self = .unportablePaths
        case "ref_conflict": self = .refConflict
        case "unknown_revision": self = .unknownRevision
        case "host_unreachable": self = .hostUnreachable
        case "permission_denied": self = .permissionDenied
        case "quota_exceeded": self = .quotaExceeded
        case "out_of_space": self = .outOfSpace
        case "invalid": self = .invalid
        case "other": self = .other
        default: self = .unknown(wire)
        }
    }

    public var wire: String {
        switch self {
        case .notARepository: "not_a_repository"
        case .dirtyWorkingTree: "dirty_working_tree"
        case .nothingToCommit: "nothing_to_commit"
        case .mergeInProgress: "merge_in_progress"
        case .noMerge: "no_merge"
        case .unresolvedConflicts: "unresolved_conflicts"
        case .branchNotFound: "branch_not_found"
        case .branchExists: "branch_exists"
        case .invalidName: "invalid_name"
        case .unportablePaths: "unportable_paths"
        case .refConflict: "ref_conflict"
        case .unknownRevision: "unknown_revision"
        case .hostUnreachable: "host_unreachable"
        case .permissionDenied: "permission_denied"
        case .quotaExceeded: "quota_exceeded"
        case .outOfSpace: "out_of_space"
        case .invalid: "invalid"
        case .other: "other"
        case .unknown(let s): s
        }
    }
}

/// Everything that can go wrong when driving `rgit`.
public enum RgitError: Error, Sendable, Equatable, LocalizedError {
    /// The engine reported a structured failure (`{"ok":false,...}`).
    case engine(code: RgitErrorCode, message: String, paths: [String])
    /// The `rgit` executable could not be found or started.
    case executableUnavailable(String)
    /// The engine speaks a different `json_interface` than this adapter.
    case incompatibleInterface(found: Int, expected: Int)
    /// The process ended without a parseable result object (crash, killed, old binary without --json).
    case processFailed(exitCode: Int32, stderr: String)
    /// The result line was not valid for the requested type.
    case malformedOutput(String)
    /// The caller cancelled; the engine was terminated and resumes on the next run.
    case cancelled

    public var code: RgitErrorCode? {
        if case .engine(let code, _, _) = self { return code }
        return nil
    }

    public var paths: [String] {
        if case .engine(_, _, let paths) = self { return paths }
        return []
    }

    /// Engine message or a description; UI code should prefer mapping `code` to localized text.
    public var errorDescription: String? {
        switch self {
        case .engine(_, let message, _): message
        case .executableUnavailable(let why): "rgit is unavailable: \(why)"
        case .incompatibleInterface(let found, let expected):
            "rgit speaks JSON interface \(found); this app needs \(expected)"
        case .processFailed(let code, let stderr):
            "rgit exited with status \(code)" + (stderr.isEmpty ? "" : ": \(stderr)")
        case .malformedOutput(let why): "unexpected output from rgit: \(why)"
        case .cancelled: "cancelled"
        }
    }
}
