# Desktop release downloads

The current desktop release is available on [GitHub Releases](https://github.com/xiezhengyi1/Rustgit/releases/tag/v0.1.0):
macOS Apple Silicon DMG/ZIP, Windows x64 EXE, and a shared `SHA256SUMS` file.
The Windows desktop installer was subsequently installed and smoke-tested on native Windows;
see [the verification record](windows.md#native-windows-smoke-verification--2026-10-20).

The CLI archives below were produced earlier and are not the desktop downloads in this release.
Their original verification limits apply to those specific archives.

# Packaging the command-line tool

`scripts/package-cli.sh` builds `rgit` and writes `dist/` (archives + `SHA256SUMS`):

| Archive | Target | Built | Run here |
|---|---|---|---|
| `rgit-<ver>-aarch64-apple-darwin.tar.gz` | Apple Silicon | yes | **yes**: extracted into a clean directory, `init`, `commit`, `verify` pass |
| `rgit-<ver>-x86_64-apple-darwin.tar.gz` | Intel Mac | yes (cross-compiled) | **no**: this machine has no Rosetta |
| `rgit-<ver>-x86_64-pc-windows-gnu.zip` | 64-bit Windows | yes (cross-compiled with mingw-w64) | **no**: no Windows or Wine available; checked only that it is a PE32+ x86-64 console executable importing only system DLLs (KERNEL32, WS2_32, the Universal CRT `api-ms-win-crt-*`, ntdll, bcryptprimitives, USERENV), so it needs Windows 10 or newer and nothing else |

All three are **unsigned**. macOS will quarantine a downloaded unsigned binary
(`xattr -d com.apple.quarantine rgit`, or sign with a Developer ID and notarize); Windows SmartScreen
will warn. Neither signing credential exists on this machine.

The Windows build has never been executed on Windows. The Rust test suite has only run on macOS, and
code that is Windows-specific (directory fsync skipped, no symlink support, path rules) is unproven
there. Run `cargo test --workspace` on a Windows machine before relying on it.

## Reproduce

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-gnu
brew install mingw-w64          # provides x86_64-w64-mingw32-gcc
./scripts/package-cli.sh        # first run takes ~10 minutes (SQLite and zstd compile from C)
```

`.cargo/config.toml` points the Windows target at the mingw linker. For an MSVC-ABI build, build on
Windows with `cargo build --release --target x86_64-pc-windows-msvc`.

## Desktop applications

The macOS and Windows apps are separate work under `apps/macos` and `apps/windows`; they drive the
bundled `rgit --json` described in [`gui-interface.md`](gui-interface.md). Their packaging (an `.app`/`.dmg`,
an NSIS/MSI installer) is documented by those projects and is not covered by this script.
