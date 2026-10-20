# macOS app and Swift adapter

Layout:

| Path | What |
|---|---|
| `packages/swift-client/` | `RgitClient`: Swift package that drives the bundled `rgit --json` (docs/gui-interface.md) |
| `apps/macos/` | SwiftUI app (`RustGitKit` library + `RustGit` executable), localization, Info.plist template |
| `scripts/macos/build-app.sh` | cargo build + swift build + `.app` assembly + ad-hoc codesign + `.zip`/`.dmg` |

## Toolchain (what was actually used)

* macOS 27 (arm64), **Command Line Tools only** (Swift 6.4). No Xcode IDE, no `xcodebuild`, no XCTest.
* Rust 1.99 (rustup) for `rgit`. Deployment target macOS 14. The app is built for the host architecture
  (arm64). `--universal` exists in the script (needs `rustup target add x86_64-apple-darwin`) but **has not been run**.
* **SDK workaround:** the macOS 27 SDK implements SwiftUI's `@State` etc. as macros whose compiler plug-in
  (`SwiftUIMacros`) is part of Xcode, not of the Command Line Tools, so `swift build` fails with
  "plugin for module 'SwiftUIMacros' not found". The script therefore builds against the newest older SDK in
  `/Library/Developer/CommandLineTools/SDKs` (`MacOSX26.5.sdk` here) when only the CLT are installed.
  With full Xcode installed the default SDK is used. Override with `RGIT_SWIFT_SDK=/path/to.sdk`.
  The same flag works manually: `swift build -c release --sdk <path>` inside `apps/macos`.
* SwiftPM's macro-plugin lookup is sometimes flaky with only the CLT (a build fails with "plugin for module
  … not found" and succeeds on retry); the script retries once.

## Build, test, run

```bash
scripts/macos/build-app.sh                  # -> dist/macos/RustGit.app, RustGit-<ver>-macos-arm64.{zip,dmg}, BUILD-INFO.txt
scripts/macos/build-app.sh --rgit PATH      # bundle a prebuilt rgit instead of running cargo

# adapter tests (real rgit, temp repositories); Swift Testing, because XCTest is not in the CLT
cd packages/swift-client && RGIT_BINARY=../../target/release/rgit swift test
# app tests (diff parser, graph layout, localization key parity, error texts)
cd apps/macos && swift test --sdk /Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
# offscreen renders of every screen from a real repository (needs a built rgit):
RGIT_SNAPSHOT_DIR=/tmp/snaps RGIT_BINARY=... swift test --filter Snapshot
```

If a test build fails with "plugin for module 'TestingMacros' not found", pass the CLT plugin explicitly:
`-Xswiftc -load-plugin-library -Xswiftc /Library/Developer/CommandLineTools/usr/lib/swift/host/plugins/testing/libTestingMacros.dylib`.
Automated sharing tests opt into file keys in isolated temporary directories; they do not touch the real Keychain.

`RgitLocator` finds `rgit` in this order: `$RGIT_BINARY`, `Contents/Resources/rgit`, next to the app
executable (`Contents/MacOS/rgit`, where the script puts it), `PATH`, Homebrew paths.

## Adapter (`RgitClient`)

* One process per command: `rgit --json -C <dir> <command>`; stdout is read line by line, `{"event":…}`
  lines go to a progress callback, the `{"ok":…}` line is the result. Async/await, `Sendable` models.
* Errors: `RgitError` = `.engine(code, message, paths)` (codes from the contract, unknown codes preserved),
  `.executableUnavailable`, `.incompatibleInterface`, `.processFailed` (no result line, e.g. crash),
  `.malformedOutput`, `.cancelled`.
* Cancellation: cancelling the calling `Task` closes the child's stdin and sends SIGTERM (SIGKILL after 3 s);
  the call throws `.cancelled`. Interrupted transfers resume on the next run (engine behaviour).
* `checkCompatibility()` refuses a binary whose `json_interface` is not 1.
* `ServeSession` runs `rgit serve` and waits for the `listening` event; `stop()` ends it.
* Implementation notes that cost time and are encoded in tests: `FileHandle.bytes` was **not** used because
  its blocking reads share one serial queue, so a long-lived `serve` reader starves every other reader;
  `Process.waitUntilExit()` was observed never returning for a finished child, so exit is observed through
  `terminationHandler`; pipes are created close-on-exec under a lock so a long-lived child never inherits another
  command's pipes (which would keep that command from ever seeing EOF).

## App behaviour

* **Projects:** create (init) in a chosen folder, open a folder (offers to initialize it if it is not a project),
  join/clone from a `tcp://host:port` address, recent list, hosted projects list. The sidebar retains up to
  30 recent projects across launches and switches directly between them. Draft commit messages and file
  selections survive switching during the current session; failed opens leave the current project intact.
  Right-click a sidebar project to remove its recent entry without touching its files.
* **Changes:** searchable file list with names above their directories, per-file checkboxes, a text preview
  with old/new line numbers, horizontal scrolling for long lines, addition/deletion counts and a separate
  commit composer. Searching only filters the list; the selection count and Select all apply to the whole
  project. Shared projects use **Commit and share** to commit and then send in one action; projects
  without a remote retain a local commit button. A failed/cancelled send keeps the local commit,
  clears its submitted draft and offers **Retry sharing** without creating another commit.
  Commit of the selected
  files only (`commit -m … -- paths`). During a merge the commit takes everything and is blocked until every
  conflict is resolved.
* **History and branches:** commit list with a lane graph, details and per-file diff, create/switch/delete
  branch, revert, restore a file from a commit (offers a forced restore that keeps the engine's recovery copy
  when uncommitted edits are in the way), merge with a conflict sheet: keep ours / take theirs / edit
  (the working file contains the engine's conflict markers; saving marks it resolved), abort merge.
* **Sharing:** "Where is your work?" three-step indicator (committed locally / received by the host / fetched by
  other devices), fetch/pull/push with progress and Cancel, connections (remotes), host this project (creates a
  hosted copy under `~/Library/Application Support/RustGit/Hosts`, starts `rgit serve` on 127.0.0.1 from port
  7878, enrolls this Mac with a single-use owner invitation, pins the host identity, and pushes).
  The Sharing screen creates invitations (role, expiry, single-use, label), lists/revokes invitations,
  lists devices with role changes/revocation and last seen, and shows device key storage and host fingerprints.
  Invitation secrets are held only in view state and are passed to `join -` through stdin.
  Join accepts `projectapp://` links, registered in the bundle, or `tcp://`, `ssh://`, `iroh://` addresses
  with an optional trusted host fingerprint. Discovery uses `discover --watch`; results are hints only.
  Iroh events show direct/relayed/unreachable connection state. A per-host relay field restarts serving
  with `--iroh --iroh-relay`; the relay must be supplied by the user, and direct internet access is not guaranteed.
  "Fetched by other devices" remains unknown because the host does not report it.
* **Storage and recovery:** size and dedup statistics, verify, gc (optionally thorough), reindex, written
  recovery guidance, list of shelves/recovery copies.
* Always visible (bottom bar): current branch, merge state, sync state relative to the host (the engine reports
  `up_to_date/behind/ahead/diverged` after fetch, not counts), server state, running activity with progress.
  The project fetches every 60 s while it has a connection (fetch never touches working files).
* **Menu bar:** a menu-bar item lists hosted projects with start/stop toggles. With "Keep hosting when the
  window is closed" on (default) and a host running, closing the window keeps the app, and hosting, alive.
  Quitting the app stops hosting (Cmd-Q, and also SIGTERM/SIGINT/SIGHUP, which are routed through a normal quit; a SIGKILL or crash would leave `rgit serve` running, because `serve` does not exit when its stdin closes - an engine behaviour). Hosting is unavailable while the Mac sleeps or after shutdown; the UI says so.
* **Login item:** Settings has a "Start RustGit at login" switch using `SMAppService.mainApp`. It compiles and is
  wired, but it has **not been exercised** (registering changes the user's Login Items; it also needs a
  properly signed app in /Applications). Outside a `.app` bundle the switch is disabled with an explanation.
* **Localization:** English and Simplified Chinese in `apps/macos/Resources/{en,zh-Hans}.lproj/Localizable.strings`
  (key = English text). `RustGitKitTests` checks every `L("…")` key exists in both files and format specifiers match.
  Plurals are not handled (English shows "1 files").
* Accessibility: labels/hints on icon buttons, checkboxes, branch rows (with custom actions), status items.
  Not tested with VoiceOver.

## Large projects, cancellation and the Commit button

Measured on a generated project (`bench materialize --workload tiny --scale 1.5` = 30,000 files, plus a 150 MB random
file; arm64, warm engine caches; offscreen render, not a live window). Reproduce with
`RGIT_PERF_PROJECT=<dir> RGIT_BINARY=... swift test --filter Perf` in `apps/macos`.

| Measurement | Before | After |
|---|---|---|
| `refresh()` of the 30,001-change project (everything the UI reloads after any action) | 36.8 s | 0.6–1.2 s |
| Steady-state render of the Changes screen with 30,001 rows | 339 ms | 137 ms |
| First render in a fresh process (includes SwiftUI start-up; a 50-row list costs about the same warm) | 833 ms | 811 ms (unchanged: not row-bound) |
| Select a file, added: diff visible | n/a (whole-tree diff on every refresh) | 7–19 ms (read from the file, no engine call) |
| Find one file in a whole-tree diff of 30k files (2 MB text) | full parse 220–310 ms | 1–3 ms (byte search) |
| Cancel -> status bar idle (hung fetch/push, unroutable host, stubborn work) | up to the post-cancel refresh (tens of seconds here) | 0–3 ms (UI), adapter returns in <1 ms |
| Commit 29,000 of 30,000 files + 150 MB file, while a fetch hangs | blocked (button disabled while any activity ran) | 5.2 s (engine time: hashing and storing), button enabled during the fetch |
| Engine alone: `rgit --json status` on that tree | 0.3 s warm, 4.5–4.8 s cold | unchanged (engine) |

Root causes: (1) every refresh also ran `rgit diff` for the whole tree (12 MB of JSON for 30k added files) and
parsed it on the main actor; the diff is now loaded only for the file you select. (2) Cancel awaited a full refresh
after the process had already died (the engine itself stops within 2 ms of SIGTERM in all three cases tried). Cancel now
clears the activity synchronously and does not wait for the child or for a refresh. (3) Commit was disabled by *any*
activity, including background fetches; now only work that needs the working folder (commit, switch, merge, pull,
cleanup) disables it, and every activity is removed on a single exit path (tests cover success, failure, user cancel,
cancelled caller, a work task that ignores cancellation). (4) Selection used `[String]` bookkeeping recomputed per body;
it now stores only the unticked paths, so the header, select-all and counts are O(1). (5) A commit of N selected paths
no longer passes them on the command line: the adapter writes a temporary file and uses `--paths-from` (few selected) or
`--except-from` (most selected); 30k-file commits are tested.

Remaining limits: selecting a *modified or deleted* file in a huge change set still needs the engine's whole-tree diff
(`rgit diff` has no path filter), 4–5 s here, shown with a spinner and cancelled when you pick another file. A
`diff -- PATH` option in the engine would remove that. Selection ticks and list rendering were measured offscreen only.

## Hosting for other computers

Hosting uses a separate bare repository under `~/Library/Application Support/RustGit/Hosts` and
requires encrypted, authenticated connections. LAN access remains off by default. Enabling it restarts
serving on `0.0.0.0:<port>` with `--advertise`, preserving authentication. The UI shows loopback and LAN
addresses; an address alone does not enroll a new device. Share an invitation from the People and devices card.
The owner connection is enrolled automatically when hosting is created. On upgrade, only an existing unpinned
loopback remote to that hosted copy is enrolled; an existing host pin is never replaced automatically.

`HostFlowTests` uses the real release CLI and separate temporary identities: host a working project in a
separate bare repo, enroll the owner, generate an invitation, join a second folder via loopback and LAN,
commit/push/fetch/pull, revoke the guest, and confirm `device_revoked`. It also verifies that the LAN address
refuses connections when LAN access is off, and that upgrading an old owner connection preserves existing pins.
These tests run on one physical Mac, not two computers. Firewall dialogs, VPN/guest-network isolation,
interactive Keychain prompts, real SSH servers and two-NAT Iroh paths remain unverified.

## Signing and notarization

`build-app.sh` signs ad-hoc (`codesign -s -`). That is enough to run on the build machine, **not** to distribute:
other Macs show a Gatekeeper warning and the app is **not notarized**. To distribute with a Developer ID
(requires an Apple Developer Program membership and a "Developer ID Application" certificate in the keychain):

```bash
scripts/macos/build-app.sh --identity "Developer ID Application: Your Name (TEAMID)" --bundle-id com.yourco.rustgit
# the script signs rgit then the app with --options runtime --timestamp, and signs the .dmg

# notarize (once): store credentials in the keychain
xcrun notarytool store-credentials rustgit-notary --apple-id you@example.com --team-id TEAMID --password <app-specific-password>
# submit the dmg (or the zip), wait for "Accepted", then staple
xcrun notarytool submit dist/macos/RustGit-*.dmg --keychain-profile rustgit-notary --wait
xcrun stapler staple dist/macos/RustGit-*.dmg
spctl --assess --type open --context context:primary-signature -v dist/macos/RustGit-*.dmg
```

`notarytool` and `stapler` ship with Xcode / the Command Line Tools; none of these steps was run here (no
Developer ID certificate on this machine). The hardened runtime needs no entitlement exceptions for this app
(`apps/macos/Support/RustGit.entitlements` is intentionally empty; the app is not sandboxed because it edits
user-chosen folders and spawns the bundled engine). `SMAppService` login items also require the app to be signed
with a stable identity.

## Not verified

No screen could be captured here (Screen Recording and Accessibility are not granted to the tools in this
environment), so the running window was not looked at or clicked through. What was done instead: the packaged
`.app` was launched and stayed alive with its window opened at 1100×720, and every screen was rendered offscreen
through `NSHostingView` against a real repository (a few AppKit-backed controls such as list selection render
differently offscreen, so layout fidelity of the live window is unconfirmed). Intel, macOS older than 27,
a clean-machine install, Gatekeeper behaviour on another Mac, VoiceOver, dark mode and the login item are untested.

## Continuation validation (2026-10-20)

The Swift adapter passes 32 tests, including real invitation/join, read-only push rejection, and device and
invitation revocation. The app passes 27 tests, including the secure hosting and migration flow above.
Screen snapshots were rendered offscreen from real repositories. This is not a live click-through of the
updated app, and URL dispatch by Finder has not been exercised. The earlier candidate package remains in `dist/macos-updated/`. The final package with the RustGit icon
is in `dist/macos/` and is installed to `/Applications/RustGit.app`. Signing remains ad-hoc, not notarized.

The icon master and generation prompt are under `assets/branding/`. macOS uses
`Support/RustGit.icns` via `CFBundleIconFile`; the packaging script copies it into Resources.
Windows icon formats are generated from the same master under `apps/windows/src-tauri/icons/`.

Installed on 2026-10-20 to `/Applications/RustGit.app` (build 202610201627).
Code-signature verification and disk-image verification passed. The installed application was launched
and its native About panel was visually checked: the new crab/branch icon and version/build were visible.
This verifies installation/startup and the About panel; it does not replace a full live workflow test.

## Sharing and version restoration UX (2026-10-20)

Sharing now presents the next step (commit first, set up hosting, or resume), a prominent invitation
button, readable send/get actions and the collaborators list. Technical connection details are collapsed.
An invitation sheet explains network choice, permissions and copying/sharing the link. LAN invitations
exclude loopback/wildcard addresses, preventing a guest from trying to connect to its own computer.
Discovery runs only while advanced settings are open. Defaults: write access, 24 hours, single-use.

History's primary action is **Restore to this version**, using `restore --from REV .` and leaving history
intact. **Undo this commit** remains a separate context-menu action, disabled for root commits. Unsaved
edits require a further explicit choice and are saved to a recovery snapshot before forced restoration.
Preview loading can be cancelled and no longer spins forever after a decoding/process failure.
