<p align="center"><img src="assets/branding/rustgit-icon.png" width="128" alt="RustGit"></p>

# RustGit

**English** · [简体中文](README.md)

**Keep project versions and share your work across computers.**

RustGit provides macOS and Windows desktop apps. Save history locally, or host a project on your own computer or server so other devices can work with it.

## Why RustGit

- **Choose where your data lives.** Host on your computer, a LAN machine, or a reachable server.
- **Commit and share in one action.** Select files, describe the change, and click **Commit and share**. Failed sends can be retried without losing the saved version.
- **Reuse existing data.** Chunking, deduplication, and compression reduce repeated storage; synchronization reuses existing content and sends missing data. Savings depend on the files and changes.
- **Keep generated files local.** Project `.gitignore` files, nested rules, and `.rgitignore` help exclude dependencies, caches, and build outputs.
- **Work across platforms.** macOS and Windows use the same engine and repository format, with English and Chinese interfaces.
- **Control access per device.** Encrypted connections, invitations, access roles, and device revocation.

## Download and install

| Platform | Installer |
| --- | --- |
| macOS · Apple Silicon (M-series, macOS 14+) | [Download DMG](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-macos-arm64.dmg) · [ZIP](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-macos-arm64.zip) |
| Windows · x64 (Windows 10 1809+ / Windows 11) | [Download EXE installer](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-windows-x64-setup.exe) |

[All release files and checksums](https://github.com/xiezhengyi1/Rustgit/releases/tag/v0.1.0)

On macOS, open the DMG and drag RustGit into Applications. On Windows, run the EXE. The macOS package is not notarized and the Windows installer is unsigned, so your system may ask you to confirm the source. The installer downloads WebView2 if it is missing.

## How to use it

### 1. Save your first local version

1. Open RustGit. Create a project in **Projects**, or open an existing folder and initialize it.
2. **Check `.gitignore` first.** Exclude dependencies such as `node_modules/`, caches, and build outputs.
3. In **Changes**, select the files to save and describe the version.
4. Click **Commit version** to save locally.

Repeat after editing. Use **History & branches** to inspect changes, create branches, or restore files. Recent projects stay in the sidebar for quick switching.

### 2. Connect another computer

**On the hosting computer:**

1. Open a project with at least one commit. In **Sharing**, choose to host the project.
2. Enable local-network access so other computers can connect.
3. Create a read invitation for downloads, or a write invitation for collaboration.
4. Send the invitation link privately to the other computer.

**On the other computer:**

1. Join through the invitation in **Projects**.
2. Choose a folder and wait for the download.
3. Open the folder and confirm the project files are present.

Invitations are normally single-use; create a separate one for each new device. The hosting computer must remain awake with RustGit running.

### 3. Edit, send, and receive

| What you want to do | Action |
| --- | --- |
| Send your edits | **Changes** → select files → describe the change → **Commit and share** |
| Receive another device's updates | **Sharing** → **Pull and merge** |
| Retry a failed send | **Retry sharing**, without committing again |
| Resolve overlapping edits | Pull, resolve conflicts, commit, then share |
| Recover an older file | **History & branches** → select a version → restore the file |

Successful sharing means the host received the version. Other computers must still pull to update their files.

## Can I host it on a server?

**Yes. Run `rgit serve` on the server and connect desktop clients through invitations.** The server does not need a desktop interface, and your personal computer does not need to stay on. A persistent host, Linux server, or suitably configured NAS can store the shared history.

The server provides RustGit repository synchronization. It currently has no GitHub-style web code browser or account-management dashboard. Each `serve` process hosts one project; use separate directories and ports for multiple projects.

### Start a Linux server

Install Rust 1.89+ and C build tools, then run on the server:

```bash
git clone https://github.com/xiezhengyi1/Rustgit.git
cd Rustgit
cargo build --release -p rustgit-cli
mkdir -p "$HOME/.local/bin" "$HOME/rustgit-repos"
install -m 755 target/release/rgit "$HOME/.local/bin/rgit"

# Use this same directory for the service and administration commands
export RGIT_HOME="$HOME/.local/share/rustgit-server"
"$HOME/.local/bin/rgit" --insecure-file-keys serve   --init "$HOME/rustgit-repos/myproject.rgit" --listen 0.0.0.0:7878
```

Linux currently requires explicit `--insecure-file-keys`: private keys are unencrypted files readable only by their owner. **Network connections still require authentication and encryption.** Use a dedicated system user and protect the configuration directory and backups. macOS and Windows use their system key stores by default. These Linux deployment instructions follow the CLI implementation; this release has no prebuilt Linux binary and this session has not verified a native Linux deployment.

### Invite a computer and upload an existing project

Keep the server running. In another terminal on the server, replace `YOUR_SERVER_IP` with an address or domain reachable by clients:

```bash
export RGIT_HOME="$HOME/.local/share/rustgit-server"
"$HOME/.local/bin/rgit" --insecure-file-keys   -C "$HOME/rustgit-repos/myproject.rgit" invite create   --role write --expires 24h --hint tcp://YOUR_SERVER_IP:7878
```

Allow clients to reach TCP port 7878 in the host firewall and, for a cloud server, its security group. Other devices cannot use `0.0.0.0` or `127.0.0.1` as the server address.

**Upload an existing RustGit project:** Run inside your local project, using the complete invitation:

```bash
rgit join 'projectapp://join/REPLACE_WITH_THE_COMPLETE_INVITATION' --add-remote
rgit push
```

Open that project in the desktop app and use **Commit and share** afterward. If `origin` already points somewhere else, check the intended connection first; use `--remote server` to add a differently named connection.

**Download on other devices:** Create a separate invitation for each device. Join in the desktop app, or run:

```bash
rgit join 'projectapp://join/REPLACE_WITH_ANOTHER_COMPLETE_INVITATION' myproject
```

A newly initialized hosted repository is empty. Upload the first version before other computers download it; otherwise their working folder may contain only `.rgit`.

For a persistent service, automatic restart, backups, SSH, and internet relays, see the **[server hosting guide](docs/server.md)**.

## Common questions

**Can it connect directly to GitHub or an existing Git repository?** Not currently. RustGit uses its own `.rgit` format and does not support Git repository protocols. This GitHub repository distributes the software's source and installers.

**Why does an ignored file still sync?** Ignore rules apply to untracked files; previously committed files remain tracked. Set rules before the first commit. `.git/` and `.rgit/` are always excluded. Global Git ignore rules are not read.

**What matters when sharing between Windows and Mac?** Windows cannot currently restore symbolic links. Exclude dependency caches, build outputs, and macOS app bundles from cross-platform projects.

**How mature is it?** `v0.1.0` is an early release. The macOS app has been run; native Windows replacement installation, the actual **Commit and share** UI, and a second copy pulling a file with a matching hash have passed smoke tests. Keep independent backups of important projects. See [implementation status](docs/status.md) for the full verification scope.

## Developer documentation

[CLI usage](docs/cli.md) · [Server hosting](docs/server.md) · [macOS build](docs/macos-app.md) · [Windows build](docs/windows.md) · [Storage and recovery](docs/format.md) · [Storage benchmarks](docs/benchmarks.md) · [Design decisions](docs/decisions.md)
