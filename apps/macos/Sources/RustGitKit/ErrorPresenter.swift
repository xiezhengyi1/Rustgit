import Foundation
import RgitClient

/// Error as shown to the user: what happened, what to do, and the engine's own words for support.
public struct UserFacingError: Identifiable, Equatable {
    public let id = UUID()
    public var title: String
    public var suggestion: String
    public var detail: String
    public var paths: [String]
    public var code: RgitErrorCode?
}

public enum ErrorPresenter {
    public static func present(_ error: Error) -> UserFacingError {
        guard let e = error as? RgitError else {
            return UserFacingError(title: L("Something went wrong"), suggestion: L("Try the action again."),
                                   detail: error.localizedDescription, paths: [], code: nil)
        }
        switch e {
        case .cancelled:
            return UserFacingError(title: L("Cancelled"), suggestion: L("Nothing was lost. Interrupted transfers continue where they stopped next time."),
                                   detail: "", paths: [], code: nil)
        case .executableUnavailable(let why):
            return UserFacingError(title: L("The RustGit engine is missing"), suggestion: L("Reinstall the app. The engine (rgit) is bundled inside it."),
                                   detail: why, paths: [], code: nil)
        case .incompatibleInterface(let found, let expected):
            return UserFacingError(title: L("The bundled engine does not match this app"),
                                   suggestion: L("Reinstall the app from a single download so both parts are the same version."),
                                   detail: LF("Engine interface %d, app expects %d.", found, expected), paths: [], code: nil)
        case .processFailed(let code, let stderr):
            return UserFacingError(title: L("The engine stopped unexpectedly"), suggestion: L("Try again. If it keeps happening, run Verify in Storage and Recovery."),
                                   detail: LF("Exit status %d. %@", Int(code), stderr), paths: [], code: nil)
        case .malformedOutput(let why):
            return UserFacingError(title: L("The engine returned something unexpected"), suggestion: L("Reinstall the app so the engine and app match."),
                                   detail: why, paths: [], code: nil)
        case .engine(let code, let message, let paths):
            let (title, suggestion) = text(for: code)
            return UserFacingError(title: title, suggestion: suggestion, detail: message, paths: paths, code: code)
        }
    }

    static func text(for code: RgitErrorCode) -> (String, String) {
        if let security = sharingText(code.wire) { return security }
        return switch code {
        case .notARepository: (L("This folder is not a RustGit project"), L("Choose a project folder, or create a new project here."))
        case .dirtyWorkingTree: (L("You have uncommitted changes in the way"), L("Commit them, or set them aside, then try again."))
        case .nothingToCommit: (L("Nothing to commit"), L("Change a file first."))
        case .mergeInProgress: (L("A merge is still in progress"), L("Resolve the conflicts and commit, or abort the merge."))
        case .noMerge: (L("There is no merge to finish"), L("Nothing to do."))
        case .unresolvedConflicts: (L("Some conflicts are not resolved yet"), L("Pick ours, theirs or edit the file for each conflict, then commit."))
        case .branchNotFound: (L("That branch does not exist"), L("Pick a branch from the list."))
        case .branchExists: (L("A branch with that name already exists"), L("Choose a different name."))
        case .invalidName: (L("That name is not allowed"), L("Use letters, numbers, dashes and slashes, without spaces."))
        case .unportablePaths: (L("Some file names cannot be used on every system"), L("Rename the files listed below, then try again."))
        case .refConflict: (L("Someone else changed the shared branch first"), L("Fetch their work, merge it, then share again. Your commits are safe."))
        case .unknownRevision: (L("That version could not be found"), L("Refresh the history and pick another commit."))
        case .hostUnreachable: (L("The host cannot be reached"), L("Check the address, that the host is running and awake, and that you are on the same network."))
        case .permissionDenied: (L("The host refused this action"), L("Ask the host owner to allow it."))
        case .quotaExceeded: (L("The host's storage limit was reached"), L("Ask the host owner to raise the limit or free space."))
        case .outOfSpace: (L("This disk is full"), L("Free some space and try again. Committed history is not rewritten."))
        case .invalid: (L("That request was not valid"), L("Check what you entered."))
        case .other, .unknown: (L("The operation failed"), L("Read the details below, fix the cause and try again."))
        }
    }

    private static func sharingText(_ code: String) -> (String, String)? {
        switch code {
        case "host_key_mismatch": (L("The host identity does not match"), L("Confirm the fingerprint with the host owner before changing the pinned identity."))
        case "secure_channel_failed": (L("The encrypted connection failed"), L("Check the host address and try again. Do not disable encryption."))
        case "security_required": (L("This host requires an encrypted connection"), L("Use a current engine and an invitation from the host owner."))
        case "not_enrolled": (L("This device needs an invitation"), L("Ask the host owner for an invitation and join the project."))
        case "device_revoked": (L("Access for this device was revoked"), L("Contact the host owner. Downloaded copies remain on this device."))
        case "invite_malformed": (L("The invitation link is malformed"), L("Paste the complete invitation link."))
        case "invite_invalid": (L("The invitation is not valid"), L("Ask the host owner for a new invitation."))
        case "invite_expired": (L("The invitation has expired"), L("Ask the host owner for a new invitation."))
        case "invite_used": (L("The invitation has already been used"), L("Ask the host owner for a new invitation."))
        case "invite_revoked": (L("The invitation was revoked"), L("Ask the host owner for a new invitation."))
        case "key_storage_unavailable": (L("The system key store is unavailable"), L("Unlock the system key store and try again. Private keys are not saved to a file automatically."))
        case "host_key_unavailable": (L("The host identity key is unavailable"), L("Check the key store on the host computer."))
        case "not_hosted": (L("This is not a hosted repository"), L("Manage invitations and devices on the computer hosting the project."))
        case "host_serving": (L("The host must be stopped first"), L("Stop hosting, complete this action, then start hosting again."))
        case "no_such_device": (L("That device could not be found"), L("Refresh the device list."))
        case "no_such_invitation": (L("That invitation could not be found"), L("Refresh the invitation list."))
        case "ambiguous": (L("More than one item matches"), L("Use the complete device fingerprint or invitation ID."))
        case "ssh_host_key_failed": (L("The SSH host key could not be verified"), L("Verify the SSH host fingerprint with its owner and configure known_hosts yourself."))
        case "ssh_auth_failed": (L("SSH authentication failed"), L("Check your SSH key, agent and server account."))
        case "ssh_unavailable": (L("The SSH client is missing"), L("Install the system SSH client before using this address."))
        case "remote_rgit_missing": (L("The remote computer has no RustGit engine"), L("Install rgit on the remote computer and make it available over SSH."))
        default: nil
        }
    }
}
