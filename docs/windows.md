# Windows desktop app

`apps/windows` is a Tauri 2 shell (Rust) around a TypeScript/React UI. It contains **no version-control
logic**: every operation runs the bundled `rgit.exe --json` ([`gui-interface.md`](gui-interface.md)) and
parses its NDJSON output. Hosting is `rgit serve` as a child process of the app.

> **Verification status (2026-10-20).** The unsigned x64 NSIS installer was cross-built on macOS,
> then installed over the previous version and launched on a real Windows computer. Clicking the
> native app's **Commit and share** button published a commit; a second working copy pulled the file
> through the CLI, with matching SHA256 hashes. A 3,425-file project (62,887,673 bytes) was also
> confirmed on disk and pulled successfully (already up to date). These checks do not cover every
> Windows feature. MSI, clean-machine WebView2 bootstrap, uninstall, autostart and native failure/
> cancellation/retry scenarios remain unverified. The installers are **not signed**.

## Architecture

```
React UI (apps/windows/src)            Rust shell (apps/windows/src-tauri/src)
  pages, i18n, state                     rgit.rs    spawn `rgit --json`, relay stdout/stderr lines
  rgit/client.ts  typed JSON client  ->  serve.rs   `rgit --json serve` lifecycle, address, errors
  platform/tauri.ts  invoke + events     tray.rs    tray menu (open / host status / stop / quit)
                                         settings.rs settings.json, close-to-tray decision
                                         autostart.rs HKCU\...\Run value (per user)
                                                |
                                          rgit.exe  (bundled sidecar, next to the app .exe)
```

* **Runner interface** (`src/rgit/runner.ts`): the only thing the UI needs from the platform. The Tauri
  implementation sends lines as events (`rgit-line`, `rgit-stderr`, `rgit-exit`); tests use a Node child
  process. NDJSON parsing, result/error handling, exit-code rules and cancellation live in
  `src/rgit/client.ts` once.
* **Cancel** terminates the `rgit` process; the engine resumes interrupted transfers on the next run.
  The child's stdin is kept open and closed with the process, as `gui-interface.md` specifies.
* **Hosting** is a child of the app. It is not a Windows service and is never installed as one.
  Closing the window hides it to the tray *only while hosting is running and the user kept
  "Keep hosting in the system tray" on*; otherwise closing exits the app. **Quit** in the tray menu stops
  the host. If the app crashes or is killed, Windows may leave `rgit serve` running until it is ended
  (it holds the repository lock, so a second host on the same folder is refused with a clear error).
* **Start at login** writes `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\RustGit` =
  `"<app.exe>" --background` (per user, no administrator rights, removable in Windows Settings > Apps >
  Startup). `--background` starts hidden in the tray and, if "start hosting when the app starts" is on,
  starts the saved host.
* **Settings** are `%APPDATA%\app.rustgit.desktop\settings.json` (language, tray choice, saved host
  config, recent projects).
* **Single instance:** starting the app again focuses the running window.
* Sleep, hibernation or shutdown make hosting unavailable; the UI says so next to the host state.

## What the UI offers

| Area | Implemented | Notes |
|---|---|---|
| Projects | create/open/clone, invitation join via stdin, tcp/SSH/Iroh addresses, secure hosting with optional LAN discovery and a user-supplied relay | LAN access defaults off. Hosting stays encrypted; new devices need an invitation. The Sharing page can enroll this computer and publish the current project to its hosted copy without replacing an existing pin |
| Changes | virtualized file list with kinds, per-file checkboxes, unified diff per file, commit message, commit and share selected or all, shelve | shared projects automatically send after committing; local-only projects only commit. Failed/cancelled sends retain the commit and offer a send-only retry. Renames commit both names; unresolved merges block submission; see [Large projects](#large-projects) |
| History & branches | commit graph (lanes), details with diff, restore a file from a commit (with overwrite confirmation and recovery snapshot), revert, create/switch/delete branches, merge, conflict resolution (keep mine / keep theirs / edit yourself then mark resolved), abort merge | switching with uncommitted changes offers to set them aside |
| Sharing & connections | invitation creation/list/revoke; device identity/storage, role changes/revoke/last seen; pinned host fingerprints; live mDNS hints and Iroh connection state | Local access administration belongs to the hosting computer. Invitations are view state only and join uses stdin. `projectapp://` dispatch is registered through the Tauri deep-link plugin; actual Windows OS dispatch is untested |
| Storage & recovery | statistics and dedup ratio, verify (progress, cancel), gc (optionally aggressive), reindex, shelves (bring back / discard), recovery guidance | |
| Always visible | branch, "committed locally N not yet received by the server", commits to fetch, hosting state, sync progress with cancel, last sync result | "fetched by a peer" is **not tracked** by the engine; the UI says so instead of guessing |

Languages: English and Simplified Chinese (`src/i18n`; a test fails if the key sets or placeholders
differ). "Follow the system" is the default. Keyboard: `Ctrl+1`..`Ctrl+5` switch areas, `Ctrl+,`
opens settings, `Ctrl+Enter` commits from the message box, arrow keys move in file and commit lists,
dialogs trap focus and close with `Esc`. Screen-reader labels exist on every control, progress bars
expose values, and errors use `role="alert"`. This was reviewed in code and jsdom tests only; it has not
been tried with Narrator or NVDA.

## Large projects

A user reported problems on a 29,953-changed-file project in the macOS app. The Windows app shares the
design, so the same classes of problem were audited and fixed. Root causes and the fixes:

1. **Commit disabled by unrelated activity.** `runOp` (sync, clone, verify, gc) set the same global
   `busy` flag that disables Commit and was cleared only when the child process reported its exit; a
   cancelled or slow process therefore left buttons disabled, and overlapping calls could clear each
   other's flag. Now `busy` is a counter held only by project-changing calls and released in `finally`
   *before* the follow-up status refresh; long operations are tracked as `op` and no longer disable
   Commit; **Cancel clears the activity immediately** and terminates the child in the background
   (`Child::kill`, i.e. `TerminateProcess`; `rgit` spawns no children, so no `taskkill /T` is needed);
   a stale exit from a cancelled run cannot clear a newer activity. Window-focus refreshes are skipped
   while a scan is already running. Tauri commands are `async` so a slow command (stopping the host
   waits up to 3 s) cannot freeze the UI thread.
2. **Tens of thousands of paths on the command line.** Windows limits a command line to about 32k
   characters. The UI now plans the selection (`src/rgit/selection.ts`): everything selected passes no
   list, a majority passes `--except-from -` (the few left out), a minority passes `--paths-from -`; short
   lists (at most 40 paths / 2,000 characters) stay plain arguments. The list travels over **stdin**
   (`RunRequest.stdin` -> `rgit_spawn(input)`; written on its own thread, then closed).
3. **A 30,000-row list.** The changed-files list is virtualized (`VirtualList`: ~20 DOM rows, roving
   keyboard focus with Up/Down/PageUp/PageDown/Home/End). Selection is a mutable set with an incrementally
   known count, so toggling is O(1), the header/select-all is O(1) to render and one O(n) pass to apply, and a
   refresh reconciles in one O(n) pass. The combined diff is **not** fetched automatically above 300
   files (`rgit diff` has no path filter); the page says so and offers to load it. The "not tracked" list
   shows the first 200 entries. A "Scanning for changes…" indicator shows during `status`.
4. **Hosting join address** - see the Projects row above.

Measured (macOS, Node 24, jsdom - no layout/paint, so this is React and our logic only):
30,000 changes render in ~0.5 s with 21 DOM rows; one checkbox toggle ~40 ms; clear-all ~70 ms;
`planSelection` on 30,000 changes ~26 ms. With the real `rgit` on a 30,000-file project: `status`
took ~6.9 s and committing ~29,000 files through `--except-from` ~7.6 s (engine time; the UI stays
responsive and shows the scan). **Not measured:** real painting and scrolling in WebView2, memory use,
the app on Windows, NTFS scan speed, or antivirus overhead on 30,000 new files.

Known remaining limits with huge change sets: the History commit-details file list and diff for a
commit with tens of thousands of files are not virtualized (`show` returns the whole diff in one JSON
line), and the conflict list is not virtualized.

## Prerequisites

To **build** on Windows 10/11 (x64):

* Rust (stable, 1.89 or newer) with the **MSVC** toolchain: <https://rustup.rs>
* **Visual Studio 2022 Build Tools** with the workload "Desktop development with C++" (MSVC + Windows SDK)
* **Node.js 20 or newer** (CI uses 24)
* NSIS and the WiX toolset are downloaded by `tauri build` itself (needs internet on first use)
* For signing: the Windows SDK's `signtool.exe` and a code-signing certificate (below)

To **run** the app:

* Windows 10 version 1809 or newer, x64
* **Microsoft WebView2 Runtime.** Present on Windows 11 and current Windows 10 updates. The installer
  is configured with `downloadBootstrapper` (it downloads WebView2 if missing, so the first install needs
  internet). For offline installs change `bundle.windows.webviewInstallMode` in
  `apps/windows/src-tauri/tauri.conf.json` to `offlineInstaller` or `fixedRuntime` (larger installer).
* Windows Defender Firewall will ask for permission the first time the host listens on a non-loopback
  address (only if you enabled "Allow other devices on this network"). Choose **Allow** for private
  networks, otherwise other devices cannot connect. The app shows this hint next to the join address.
  Not verified on Windows.

## Build

The current x64 desktop installer is `dist/windows/RustGit-0.1.0-windows-x64-setup.exe`
(7,666,362 bytes). It offers Simplified Chinese/English and installs per user. It includes the
GUI executable, `rgit.exe` and `WebView2Loader.dll`; missing WebView2 Runtime is downloaded by the
installer. `SHA256SUMS`, `BUILD-INFO.txt` and `VERIFICATION.json` accompany the artifact. This is an
unsigned cross-build; subsequent native Windows installation and the successful sharing/pull path were verified on 2026-10-20.

To reproduce this NSIS build on macOS with the existing MinGW target:

```bash
brew install mingw-w64 makensis sevenzip
rustup target add x86_64-pc-windows-gnu
scripts/windows/build-cross.sh
```

`verify-installer.py` tests and extracts the NSIS archive, checks x64 PE headers and the GUI subsystem,
and compares each executable/DLL to the release output. The desktop executable differs only by Tauri's
documented source marker for the NSIS bundle type (`UNK` becomes `NSS`). No MinGW runtime DLLs are
required by these payloads. A native MSVC build is still available with the PowerShell script below.

```powershell
# from the repository root, in PowerShell
.\scripts\windows\build.ps1                 # NSIS + MSI, tests included, output in dist\windows
.\scripts\windows\build.ps1 -Bundles nsis -SkipTests
```

The script builds `rgit.exe`, checks that it answers `--json version` with interface 1, stages it as
`apps/windows/src-tauri/binaries/rgit-<triple>.exe`, runs the type check and tests, runs `tauri build`
and writes `dist\windows\*.exe|*.msi` plus `SHA256SUMS`. Manual equivalent:

```powershell
cargo build --release -p rustgit-cli
cd apps\windows
npm ci
node scripts/stage-sidecar.mjs                      # copies target\release\rgit.exe to src-tauri\binaries
npm run tauri -- build --bundles nsis,msi
```

* NSIS installs **per user** (`installMode: currentUser`, no administrator prompt). The MSI follows WiX
  defaults (per machine, asks for elevation); use NSIS for non-admin installs.
* Installers and the executables inside them are **unsigned** unless you pass `-CertThumbprint`.

### Developing without Windows

All of this works on macOS/Linux:

```bash
cd apps/windows
npm ci
npm run typecheck && npm test        # UI, client, contract and (if target/release/rgit exists) live tests
npm run build                        # vite build
node scripts/stage-sidecar.mjs       # host build of rgit as the sidecar so `cargo check` works
(cd src-tauri && cargo check && cargo test)
cargo check --target x86_64-pc-windows-gnu    # needs mingw-w64; placeholder file src-tauri/binaries/rgit-x86_64-pc-windows-gnu.exe
npm run capture-fixtures             # re-record real `rgit --json` output into test/fixtures-real
```

`npm run dev` in a plain browser shows the UI but it cannot run `rgit` (the page says so; it never
simulates an engine). Use `npm run tauri -- dev` to run the real shell on the host OS (tray and
registry autostart are Windows-only paths).

## Signing

Code signing needs a certificate that this repository does not have. Without it, SmartScreen shows
"Windows protected your PC" for downloaded installers, and `Get-AuthenticodeSignature` reports
`NotSigned`.

1. Obtain a code-signing certificate (an OV/EV certificate from a CA, or Azure Trusted Signing).
   EV and Trusted Signing build SmartScreen reputation fastest.
2. Put it in the Windows certificate store (or use your signing service's `signtool` integration) and
   note the SHA-1 thumbprint.
3. Build signed: `.\scripts\windows\build.ps1 -CertThumbprint <thumbprint>`. This runs
   `signtool sign /sha1 <thumbprint> /fd SHA256 /tr http://timestamp.digicert.com /td SHA256` on
   `rgit.exe`, and passes `certificateThumbprint`, `digestAlgorithm` and `timestampUrl` to
   `tauri build`, which signs the app executable and the installers.
4. Verify: `Get-AuthenticodeSignature .\dist\windows\*.exe,.\dist\windows\*.msi | Format-Table Path,Status`
   and `signtool verify /pa /v <file>`.

The signing path in `build.ps1` is unexercised: no certificate or Windows machine was available.
For CI, store the certificate (or the signing-service credentials) in repository secrets and add a
signing step; the workflow in this repository intentionally does not sign.

## What was and was not verified

Verified on macOS 27 / arm64 (Rust 1.99, Node 24):

| Check | Command | Result |
|---|---|---|
| TypeScript type check | `npm run typecheck` | clean |
| Unit/UI tests (vitest, jsdom) | `npm test` | client against a fake `rgit` that replays recorded output; recordings of the **real** `rgit --json` replayed through the client with shape checks (`test/contract.test.ts`); the client driving the **real** `rgit` binary through init, selective commit, branches, merge conflict, resolve, gc/verify/reindex, serve, push, clone, fetch, rejected push, pull, cancel (`test/live.test.ts`); UI flows in jsdom (commit selection, diff, history graph, restore/revert, switch with dirty tree, conflict resolution, storage, language switch, keyboard navigation, progress and cancel) |
| Production UI build | `npm run build` | succeeds |
| Rust shell (host target) | `cargo check`, `cargo clippy --all-targets`, `cargo test` | clean; tests include starting and stopping a real `rgit serve` through the shell's own code and reading its listening address, **authenticated enrollment, pushing and cloning through this machine's LAN IP against a secure host bound to `0.0.0.0`**, a stdin payload (`--except-from -`) reaching a real `rgit commit`, and the address filter |
| Rust shell (Windows target) | `cargo check --target x86_64-pc-windows-gnu`, `cargo clippy --all-targets --target x86_64-pc-windows-gnu` | clean, so the `cfg(windows)` code (registry, process flags, Explorer) compiles |
| Tauri config, UI build hook, release compile | `npx tauri build --no-bundle` on macOS | builds; the macOS binary was started with the real `rgit` and stayed up for several seconds without errors (no window inspection possible) |
| Layout | test-only harness `test/visual` (real UI, recorded data) viewed in a browser pane, English/Chinese, light/dark | looked at once; no automated visual tests |

**Not verified (must be done on Windows):**

* tray icon/menu behavior and close-to-tray (the app window and successful sharing path have been exercised);
* the registry Run key actually launching the app at login (a Rust test of set/get/delete exists and
  runs in CI, but no one has logged in and watched it start);
* the complete engine test suite on Windows (selected real commands have been exercised);
* clean-machine installation, uninstall, and WebView2 bootstrapping (replacement installation succeeded);
* MSI packaging (the NSIS `.exe` has been built and its contents verified on macOS);
* signing, SmartScreen behaviour, antivirus reactions;
* path handling with drive letters, long paths, reserved names, and OneDrive-synced folders;
* Narrator/NVDA, high-contrast mode, and 150% / 200% scaling (the Chinese success flow was exercised on a real display);
* the PowerShell build script (`build.ps1`) was written without a PowerShell to run it.

### Manual checklist for the first Windows run

1. Install the NSIS package on a clean Windows 11 VM; the app opens, `rgit.exe` is next to it.
2. Create a project in `C:\Users\<you>\Documents\demo`, edit files, commit a subset, look at the diff.
3. Create a branch, make conflicting edits on two branches, merge, resolve each way, commit.
4. Host a repository, clone it into a second folder, push/fetch/pull; cancel a large push and repeat it.
5. Close the window while hosting: tray icon stays, `rgit.exe serve` keeps running; **Quit** ends it.
6. Turn on "Start when I sign in", sign out and in: the app starts hidden in the tray (and hosting if configured).
7. Run verify, gc, reindex; use the language switch; navigate by keyboard only; try Narrator.
8. Uninstall; confirm the Run value `RustGit` is gone (`reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run`).

## Known limitations and engine findings

These came out of replaying the real `rgit --json`; they are also in the session hand-over notes.

* `status` carries no `ahead`/`behind` (the interface document lists them as optional). When a server
  is configured the client derives them from two `log` calls (`syncPosition`, at most 500 commits
  each), and shows "the server has no copy of this branch" when the remote-tracking branch is missing.
* A **rejected push** was first observed returning `ok:true` with exit 3; the engine now returns `ref_conflict` (`ok:false`, exit 3) and the recording in `test/fixtures-real` shows that. The client still maps an `ok:true` result with exit 3 to `ref_conflict` as a defensive fallback.
* Result shapes (now documented in `gui-interface.md`) for sync commands: `push`/`fetch` return
  `{state?, up_to_date, stats, updated:[{ref,old?,new,generation?}]}`, `pull` returns `{fetch, merge}`,
  `clone` returns `{branch, dest, head}`, shelves are addressed by `name` (`1`, not `shelf/1`).
  The app follows the real output.
* `fetch --json` emits a `relation` event (`state: "diverged"`); the app turns it into a hint to pull.
* Conflict resolution "edit yourself" opens Explorer at the file (edit in your own editor, then mark
  resolved); there is no in-app merge editor and no conflict-marker preview.
* The app lists at most the latest 200 commits; the graph is a lane layout, not a full DAG viewer.
* Settings, recent projects and the host configuration live in a plain JSON file with no encryption;
  invitation secrets are not persisted there; the engine keeps private keys in the OS key store unless file keys were explicitly requested.

## Authenticated sharing continuation (2026-10-20)

On macOS, 121 TypeScript tests pass, including `sharing-live.test.ts`: two isolated identities, a real
secure host, invitation creation, secret through stdin (absent from argv), join, push, role downgrade,
rejected write, revocation and rejected fetch. Owner enrollment into an existing working project is tested.
The native shell passes 9 tests against a real CLI. The frontend production build passes; the shell also
passes `cargo check --all-targets --target x86_64-pc-windows-gnu` with the Windows sidecar staged.
Those automated results were obtained on macOS. A later native Windows smoke check covered replacement
installation, WebView2 UI interaction, commit-and-share, and pulling files; OS deep-link dispatch, login
items and Windows firewall dialogs remain outside that smoke check. Host discovery and relay state UI
have automated parsing coverage; no live Windows window was viewed.

## Native Windows smoke verification — 2026-10-20

The `v0.1.0` installer (SHA256
`1ebb65a9005e358a829676353391223582751444e3513f377395ccbc753c3368`) installed over the previous
version with exit code 0. The running installed desktop executable had SHA256
`bb26524b6a39d4d48985d5f3971e8c7fda163b6cd7a346abec1f1b5e4325e7e3`.

In an isolated temporary project, the actual **Commit and share** UI action displayed its success
message. The working copy, hosted repository and second working copy agreed on the new commit.
The second copy pulled via CLI and its test-file SHA256 matched the first. No test commits were
made in the user's actual project. The actual project contained 3,425 files / 62,887,673 bytes and
had a clean working tree; a normal LAN pull returned up to date in 0.16 seconds. This was an
incremental no-op pull, not an initial-download benchmark. Temporary hosting was stopped afterward.

Native push failure, cancellation and retry-without-duplicate-commit paths were not manually exercised;
they retain automated frontend coverage. No full native Windows test-suite result is claimed.
