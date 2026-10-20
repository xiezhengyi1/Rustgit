<p align="center"><img src="assets/branding/rustgit-icon.png" width="128" alt="RustGit icon"></p>

# RustGit

**English** · [简体中文](README.md)

Save project versions on your own computer and share them between macOS and Windows. RustGit includes desktop apps and a Rust CLI engine. You can host a project from your own computer without deploying a central server.

> RustGit uses its own `.rgit` repository format. It does not currently support Git repository protocols or pushing directly to GitHub. This GitHub repository hosts RustGit's source code and release downloads.

## Download and install

| Platform | Download | Requirements |
| --- | --- | --- |
| macOS · Apple Silicon | [Download DMG](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-macos-arm64.dmg) · [Download ZIP](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-macos-arm64.zip) | macOS 14 or later, Apple Silicon |
| Windows · x64 | [Download EXE installer](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-windows-x64-setup.exe) | Windows 10 1809 or later, or Windows 11 |

[All releases](https://github.com/xiezhengyi1/Rustgit/releases) · [SHA256 checksums](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/SHA256SUMS)

- **macOS:** Open the DMG and drag RustGit into Applications. The current app is not notarized by Apple. If macOS blocks it, confirm the download's source and use **Open Anyway** in System Settings → Privacy & Security. An Intel Mac desktop package is not included.
- **Windows:** Run the EXE and follow the installer. It is unsigned, so Windows may report an unknown publisher. The installer downloads WebView2 if the runtime is missing.

## Get started

### Save versions locally

1. Create a project in **Projects**, or open an existing folder and initialize it.
2. Select files in **Changes** and describe the version.
3. Click **Commit version**. Use **History & branches** to inspect, compare, or restore files later.

Recently opened projects stay in the sidebar for quick switching.

### Share between two computers

1. **Hosting computer:** Open the project and host it from **Sharing**. Enable local-network access when other computers need to connect.
2. **Hosting computer:** Create an invitation with the appropriate permissions and send the link privately to the other computer.
3. **Other computer:** Join through the invitation in **Projects**, choose a folder, and download the project.
4. **After editing:** Describe the change in **Changes** and click **Commit and share** to save locally and send it in one action.
5. **Receive changes:** Pull and merge the latest version from **Sharing** on the other computer.

The hosting computer must stay awake with RustGit running. Both computers need a reachable LAN address or another configured connection. For internet connections and a self-hosted relay, see the [Iroh guide](docs/relay.md).

If sharing fails, your local commit is retained. Use **Retry sharing** to send it without creating another commit. If someone updated the shared branch first, pull, resolve any merge conflicts, and share again.

## Features

- **Version history and recovery:** Select files to commit, inspect diffs, browse history, restore files, and revert commits.
- **Branches:** Create, switch, merge, and resolve conflicts.
- **Self-hosted sharing:** Encrypted connections, device authentication, single-use invitations, access roles, and revocation.
- **Ignore rules:** Automatically respects project `.gitignore` files, including nested rules; add RustGit-specific rules in `.rgitignore`.
- **Storage efficiency:** Chunking, deduplication, delta compression, integrity checks, and index recovery.
- **English and Chinese:** Native SwiftUI on macOS; Tauri, React, and WebView2 on Windows.

Ignore rules apply to untracked files. Adding an already committed file to `.gitignore` does not stop it from syncing. `.git/` and `.rgit/` are always excluded. Global Git ignores and rules above the project root are not read.

## Release status and verification

`v0.1.0` is an early release. The macOS Apple Silicon desktop app has been built and run. The Windows x64 installer has been installed over the previous version on a real Windows computer. Clicking **Commit and share** published a commit, and a second working copy pulled the file with a matching SHA256 hash. A separate project containing 3,425 files, about 63 MB, was also verified on disk on Windows.

This does not establish coverage for every environment or feature. Windows failure, cancellation, and retry paths have automated coverage but have not yet received equivalent native manual testing. Windows cannot restore symbolic links; exclude dependency caches, build outputs, and macOS app bundles from cross-platform projects. See the [Windows guide](docs/windows.md) and [implementation status](docs/status.md) for details.

## Build from source

### Rust engine and CLI

The base engine requires Rust 1.89+. Iroh support requires Rust 1.91+. SQLite is bundled.

```bash
cargo build --release -p rustgit-cli --features iroh
cargo test --workspace
```

```bash
# Run inside a project folder
rgit init
rgit config user.name "Your name"
rgit status
rgit commit -m "First version"
rgit log
```

For hosting, invitations, and permissions, see the [CLI guide](docs/cli.md).

### macOS desktop app

Requires macOS, Rust, and Xcode Command Line Tools or Xcode, with Swift 6+.

```bash
scripts/macos/build-app.sh
# Output: .app, .dmg, and .zip in dist/macos/
```

See the [macOS build guide](docs/macos-app.md) for SDK selection and testing.

### Windows desktop app

On Windows, install Rust with the MSVC toolchain, Visual Studio 2022 C++ Build Tools, and Node.js 20+, then run:

```powershell
.\scripts\windows\build.ps1
# Output: EXE, MSI, and checksums in dist\windows\
```

A [macOS cross-build script](scripts/windows/build-cross.sh) is also available. See the [Windows build guide](docs/windows.md) for prerequisites and signing.

## Source layout and documentation

| Path | Contents |
| --- | --- |
| `crates/` | Rust storage, version control, CLI, synchronization, and transports |
| `apps/macos/` | macOS SwiftUI desktop app |
| `apps/windows/` | Windows Tauri and React desktop app |
| `packages/swift-client/` | Swift adapter for the CLI |
| `scripts/` | Build, packaging, and verification scripts |

[Repository format and recovery](docs/format.md) · [Sync protocol](docs/protocol.md) · [SSH](docs/ssh.md) · [LAN discovery](docs/discovery.md) · [Storage benchmarks](docs/benchmarks.md) · [Design decisions](docs/decisions.md)
