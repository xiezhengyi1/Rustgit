# 服务器托管 / Server hosting

[中文 README](../README.md) · [English README](../README.en.md)

README contains the initial Linux server, invitation, upload and download commands. This guide adds
persistent operation and recovery details. A hosted repository is a separate repository containing
shared history, not a live mirror of a working directory. Clients explicitly push and pull versions.

## Linux: keep the process running

Build the base CLI as shown in the README. Run it under a dedicated, unprivileged user. The example
below is a **user** systemd unit; replace `myproject` and the port for each project. `RGIT_HOME` must
match the directory used by invitation and device-management commands.

Save as `~/.config/systemd/user/rustgit-myproject.service`:

```ini
[Unit]
Description=RustGit project host
After=network.target

[Service]
Environment=RGIT_HOME=%h/.local/share/rustgit-server
ExecStart=%h/.local/bin/rgit --insecure-file-keys serve --init %h/rustgit-repos/myproject.rgit --listen 0.0.0.0:7878
Restart=on-failure
RestartSec=5
UMask=0077

[Install]
WantedBy=default.target
```

Stop the foreground instance before starting the service; only one process may serve the same
repository. Create the parent repository directory before starting the unit.

```bash
systemctl --user daemon-reload
systemctl --user enable --now rustgit-myproject
systemctl --user status rustgit-myproject
journalctl --user -u rustgit-myproject -f
```

For the user service to survive logout and start at boot, an administrator must enable lingering for
that dedicated account with `loginctl enable-linger USERNAME`. On systems without systemd, use the
platform's process supervisor with the same command, account, environment and persistent directories.
These systemd examples are deployment instructions, not a claim of a tested Linux release.

## Connecting clients / 连接客户端

- Make TCP 7878 reachable from your clients. A public IP, private network or VPN can provide the path.
- Create a separate invitation per device. The invitation carries the host fingerprint and address.
- Use `join --add-remote` for an existing local RustGit project, then `push` to publish its history.
- Use `join <invitation> <folder>` for a new working copy. Publish a first version before cloning.
- On macOS the bundled CLI is `/Applications/RustGit.app/Contents/MacOS/rgit`. On Windows `rgit.exe`
  is next to the installed desktop executable. Add that directory to PATH or use the full path to run
  README commands. In PowerShell, prefix a quoted executable path with `&`.
- Desktop clients use the system key store. The Linux server's `--insecure-file-keys` flag is for its
  on-disk private-key storage; it does not disable network authentication or encryption. Do not use
  `--insecure-open-access` for a network-facing server.

## Devices and invitations / 设备与邀请

On Linux use the **same account and RGIT_HOME** as the service:

```bash
export RGIT_HOME="$HOME/.local/share/rustgit-server"
"$HOME/.local/bin/rgit" --insecure-file-keys -C "$HOME/rustgit-repos/myproject.rgit" devices list
"$HOME/.local/bin/rgit" --insecure-file-keys -C "$HOME/rustgit-repos/myproject.rgit" invite list
# Replace the placeholder with a fingerprint prefix from devices list:
"$HOME/.local/bin/rgit" --insecure-file-keys -C "$HOME/rustgit-repos/myproject.rgit" devices revoke FINGERPRINT_PREFIX
```

An invitation grants a role and is consumed when redeemed. Read devices can download; write devices
can contribute versions. Revocation prevents future access but cannot erase data already downloaded.
Keep invitation links private; do not put them in public issues or logs.

## Backups / 备份

Stop hosting before taking a consistent filesystem copy. Back up both the full hosted repository
(including its `.rgit` directory) and the service user's `RGIT_HOME`. On Linux that configuration
contains unencrypted private-key files; restrict backup permissions and encrypt the backup medium.
Restore both under the same service user with appropriate ownership, then start the service.

On macOS and Windows, private keys normally live in the OS key store; copying a repository folder
alone does not back up those keys. See [repository recovery](format.md) and [security behavior](protocol.md)
before moving an existing host identity. Do not replace a pinned identity silently.

## SSH and public-network connections / SSH 与公网

TCP sharing already encrypts and authenticates sessions. SSH can provide an additional access path
when a server is reachable through SSH: see the [SSH guide](ssh.md). SSH authentication does not replace
RustGit enrollment; clients still need invitations. GUI clients also need the SSH host fingerprint
verified and recorded beforehand.

If direct reachability is unavailable, build the optional transport with
`cargo build --release -p rustgit-cli --features iroh` (Rust 1.91+) and follow the
[Iroh and self-hosted relay guide](relay.md). A relay is a network transport component; it does not
replace the repository host. A standard HTTP reverse proxy alone cannot proxy the raw TCP service.

## Troubleshooting / 排查

| Symptom / 现象 | Check / 检查 |
| --- | --- |
| Cannot connect / 无法连接 | Process running, reachable IP, correct port, server firewall, cloud security group |
| Folder contains only `.rgit` / 只有元数据目录 | Has the first commit been pushed to this hosted repository? |
| Invitation rejected / 邀请失败 | Expiration, single-use redemption, role and correct host identity |
| Push rejected / 发送被拒绝 | Pull and merge the shared branch, then retry |
| Linux key storage unavailable / 密钥存储不可用 | Explicit file-key option, same RGIT_HOME and dedicated service account |
| Host already running / 已在托管 | Stop the foreground instance before starting the service |
