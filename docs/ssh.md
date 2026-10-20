# SSH transport

`rgit` can carry the synchronization protocol through the system `ssh` client. This is for
people who already have SSH access to a machine and want to use it as the path to a hosted
project: direct SSH, through jump hosts, or through a tunnel to a server that is reachable.
SSH does **not** replace discovery or NAT traversal: the host still has to be reachable by SSH.

Crates: `crates/transport-ssh` (client transport, `serve --stdio` helpers), CLI wiring in
`crates/cli/src/net.rs`. Tests and what they prove: [section 9](#9-what-was-tested).

## 1. Layering

```
rgit fetch / push / clone / join
   protocol::client   ->  Noise secure channel (device identity, pinned host key)   <- unchanged
   SshTransport       ->  `ssh [options] -- host 'rgit serve --stdio -- /path'`      <- this crate
   system ssh         ->  sshd  ->  rgit serve --stdio  ->  host (same host code as TCP)
```

The transport only moves bytes. Device authentication and encryption are applied by
`protocol::secure` on top of it exactly as on TCP, so a project reached through SSH still needs an
enrolled device (invitation) and the client still pins the host's identity key. SSH adds a second,
independent layer: *who may reach the machine at all* and an encrypted hop.

## 2. URL form

```
ssh://[user@]host[:port]/absolute/path/to/project.rgit
ssh://host/~/relative/to/the/remote/home
ssh://[2001:db8::1]:2222/srv/p.rgit              IPv6 literal in brackets
ssh://user@host/srv/my%20project.rgit            spaces and other characters percent-encoded
ssh://host/srv/p.rgit?jump=bob@bastion:2200,second   jump hosts, in order (ssh -J)
```

* `host` may be a DNS name, an IP literal or a **name from your `~/.ssh/config`** (`Host`
  alias), so keys, `User`, `Port`, `ProxyJump`, `IdentityFile` and friends are honoured.
* The path is the directory of a hosted repository on the remote machine (what
  `rgit serve --init` creates there).
* An invitation can carry an `ssh://` URL as a connection hint (`rgit invite create --hint ssh://...`).
* There is no scp-style `host:path` form and no way to put options, commands or flags in a URL.

### What is validated (and why)

URLs can come from invitations or shared remotes, so they are treated as hostile input. They are
parsed into `{user, host, port, path, jumps}` and each part is checked against an allow-list:

| Part | Allowed | Rejected examples |
|---|---|---|
| user | letters, digits, `.` `_` `-`; no leading `-` or `.`; ≤ 64 | `-oProxyCommand=…`, spaces, `;` |
| host | letters, digits, `.` `_` `-` (no leading `-`/`.`, no `..`) or an IPv6 literal; no percent-encoding | `-F`, `host name`, `$(id)`, newlines |
| port | 1–65535, digits only | `0`, `99999`, `22abc` |
| path | Unicode letters/digits and ` ._-/+@:,=~`; no `..` segment; no leading `-`; ≤ 1024 bytes | `'` `"` `` ` `` `$` `;` `|` `&` `<` `>` `\` `*` `?` `!` `#`, newlines, NUL |
| `?jump=` | comma-separated `[user@]host[:port]`, same rules, ≤ 8 | anything else; any other query key |

Nothing from a URL is ever interpolated into a command string on **your** machine. The local
`ssh` process is started with `std::process::Command` and an explicit argument list; every
URL-derived value either follows `--` (the host) or is the value of its own option (`-l`, `-p`,
`-J`). The one command line that *is* parsed by a shell is the remote command, which the SSH
protocol hands to the remote login shell as a string: `rgit serve --stdio -- '/path'`. The path
allow-list above is what makes that safe (and the path is single-quoted when it contains a space).

The remote machine must therefore have a POSIX-style login shell. Windows OpenSSH servers (default
shell `cmd.exe`, which does not understand single quotes) work only for paths without spaces and
are **untested**.

## 3. Host keys

Host-key verification is always on. The transport sets `StrictHostKeyChecking` explicitly, so a
user config that says `no` cannot weaken it for rgit:

| Situation | Policy passed to ssh |
|---|---|
| a person can answer prompts (stdin is a terminal) | `ask`: unknown keys prompt on your terminal; changed keys always fail |
| no terminal (apps, scripts, CI) | `yes` with `BatchMode=yes`: unknown or changed keys fail, nothing prompts |
| you set `RGIT_SSH_ACCEPT_NEW_HOSTKEY=1` | `accept-new`: an unknown key is trusted the first time; a *changed* key still fails |

There is no setting that turns checking off, and no URL, invitation or config value can select
`accept-new`: only the environment of the user running `rgit`.

A failure is shown with ssh's own words plus what to do, and the JSON interface reports the code
`ssh_host_key_failed`. Examples (shortened):

```
error: ssh host key verification failed for ssh://host/srv/p.rgit: the server's host key is not known
and nothing can ask you. Connect once with `ssh <host>` in a terminal to check and record the
fingerprint, or set RGIT_SSH_ACCEPT_NEW_HOSTKEY=1 to trust it on first use. ssh said: Host key verification failed.

error: ssh host key verification failed for …: the server's host key CHANGED since it was recorded in
known_hosts. Do not continue until the server's owner confirms the new fingerprint; then remove the old
entry with `ssh-keygen -R <host>`. ssh said: @ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @ | Host key verification failed.
```

Verify a first-time fingerprint out of band (on the server: `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`).

Host-key and authentication failures are not retried (retrying cannot help and would re-prompt).

## 4. What the transport asks ssh to do

```
ssh -o BatchMode=<yes|no> -o StrictHostKeyChecking=<ask|yes|accept-new> \
    -o ClearAllForwardings=yes -o ForwardAgent=no -o ForwardX11=no -o PermitLocalCommand=no \
    -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ConnectTimeout=<s> -T \
    [-F config] [-i identity] [-p port] [-l user] [-J jumps] -- host 'rgit serve --stdio -- /path'
```

* One protocol session per ssh process: no forwarding, agent, X11 or local commands are enabled
  even if your `~/.ssh/config` turns them on for that host (checked against the real `ssh -G`, see
  tests). The agent is **not** forwarded to the remote machine.
* `ServerAlive*` detects a dead connection in about 45 s; the protocol's own timeouts apply on top.
* ssh's stderr is shown (warnings, "Permanently added …") and the last lines are quoted in errors.
* Login banners or rc-file output printed by the remote shell before `rgit` starts are skipped:
  `rgit serve --stdio` announces itself with a line `RGIT-STDIO/1`, the client discards everything
  before it (at most 64 KiB) and says so on stderr.

### Environment

| Variable | Meaning |
|---|---|
| `RGIT_SSH` | the ssh program (a path or name only; arguments are not supported, unlike `GIT_SSH_COMMAND`) |
| `RGIT_SSH_CONFIG` | `ssh -F` file |
| `RGIT_SSH_IDENTITY` | `ssh -i` file |
| `RGIT_SSH_BATCH=0\|1` | override "stdin is a terminal" detection |
| `RGIT_SSH_ACCEPT_NEW_HOSTKEY=1` | trust a host key on first use (see section 3) |
| `RGIT_REMOTE_RGIT` | the remote `rgit` command (name or path, validated like the path) |

If `ssh` is not installed, the error is `ssh client not found` (JSON code `ssh_unavailable`).
On Windows, `ssh.exe` from "OpenSSH Client" (an optional Windows feature) is used and started
without a console window; **this has not been run on Windows**.

## 5. Jump hosts and ssh config

* `ProxyJump` / `Host` aliases in `~/.ssh/config` apply as usual (`ssh://myalias/srv/p.rgit`).
* `?jump=a,b` adds `-J a,b` explicitly (each hop validated); use it when the URL must be
  self-contained, for example in an invitation. Each jump host's key is verified under the same policy.

## 6. Reverse tunnels to a reachable server

Pattern: the machine that hosts the project (behind NAT) opens an outbound SSH connection to a server
`R` that everyone can reach, and `R` forwards a port back to it. rgit does not create or supervise the
tunnel; you run `ssh`, then use an ordinary `tcp://` address.

```bash
# on the hosting machine H: serve on loopback, then publish the port on R's loopback
rgit serve /srv/p.rgit --listen 127.0.0.1:7878
ssh -N -R 127.0.0.1:7878:127.0.0.1:7878 -o ServerAliveInterval=15 -o ExitOnForwardFailure=yes user@R

# on a client: reach R's loopback port through an SSH local forward, then dial it
ssh -N -L 7878:127.0.0.1:7878 user@R
rgit join 'projectapp://join/…'      # invitation created with --hint tcp://127.0.0.1:7878
```

* `-R` binds on `R`'s **loopback** by default (sshd `GatewayPorts no`). Do not change that to expose
  the port to the network unless you understand the consequences; clients that can SSH to `R` use `-L`
  as above.
* The rgit protocol is end-to-end encrypted and device-authenticated between client and `H` (Noise
  channel), so `R` forwards ciphertext only. It still sees who connects and when.
* If `R` goes away or the tunnel drops, hosting is unreachable until the tunnel is re-established
  (`autossh` or a service manager can do that; nothing here installs one).
* This needs no `rgit` on `R`.

## 7. Advanced, opt-in: serving through a system sshd

`rgit serve --stdio [DIR]` serves **exactly one** protocol session on stdin/stdout and exits when it ends:
stdout carries protocol frames only (everything for humans goes to stderr; `--json` is refused). It
is meant to be started by sshd. This section describes configuration you do by hand; **rgit never turns on
Remote Login, edits `sshd_config` or `authorized_keys`, or changes firewall rules.**

Prerequisites you provide: a running, hardened sshd (key-only authentication recommended); an account for
this purpose; `rgit` installed where non-interactive logins find it (or use an absolute path below);
a hosted repository created with `rgit serve --init DIR` (run once, then stop it).

### Restricted key (recommended)

In that account's `~/.ssh/authorized_keys`, one line per device key:

```
command="/usr/local/bin/rgit serve --stdio /srv/projects/myproject.rgit",restrict ssh-ed25519 AAAA… alice@laptop
```

* `command=` makes sshd run exactly that, whatever the client asked for (the client's own request is
  available to it only as `SSH_ORIGINAL_COMMAND`, which rgit ignores). The path in the client's URL is
  therefore informational; keep it equal to the real one so that nothing surprises anyone.
* `restrict` turns off port, agent and X11 forwarding, PTY allocation and `~/.ssh/rc` for that key.
* The key can run nothing else: this is the "restricted remote command" mode, with no shell access.

Equivalent in `sshd_config`, for an account used only for this:

```
Match User rgit
    ForceCommand /usr/local/bin/rgit serve --stdio /srv/projects/myproject.rgit
    AllowTcpForwarding no
    AllowAgentForwarding no
    X11Forwarding no
    PermitTTY no
```

### Key storage on the server

The host's identity key normally lives in the OS key store (macOS Keychain, Windows Credential
Manager), which a non-interactive SSH login may not be able to use (a locked macOS login keychain,
for example); in that case `rgit serve --stdio` reports `key_storage_unavailable`. This was not
exercised against a real sshd. Choose consciously: either run the forced command with the
explicit, documented opt-in `rgit --insecure-file-keys --key-dir /srv/rgit-keys serve --stdio …`
(private keys then sit in 0600 files readable by that account), or host over TCP/another transport.
Nothing selects the file store silently.

### Subsystem

`rgit serve --stdio` is a plain stdin/stdout program, so `Subsystem rgit-sync /usr/local/bin/rgit serve
--stdio /srv/p.rgit` in `sshd_config` would also work on the server side. The client in this version
only runs a command (`ssh host rgit serve --stdio`), it does not request subsystems, so this is **not
usable end to end yet**.

### Access control

Who may *connect* is decided by sshd (the key). Who may *read or write the project* is still decided by
the host: the device must be enrolled by invitation (roles read/write/admin) exactly as over TCP.
`rgit serve --stdio --insecure-open-access` exists (plaintext, no device check) and would make every holder
of the SSH key a full user; it is not recommended and not covered by the tests.

## 8. Troubleshooting

| JSON `error.code` | Message starts with | Meaning / action |
|---|---|---|
| `ssh_host_key_failed` | `ssh host key verification failed` | unknown or changed host key; see section 3 |
| `ssh_auth_failed` | `ssh authentication failed` | key refused; check ssh-agent / `IdentityFile` / the account (`RGIT_SSH_BATCH=0` in a terminal allows passphrase prompts) |
| `ssh_unavailable` | `ssh client not found` | install the OpenSSH client or set `RGIT_SSH` |
| `remote_rgit_missing` | `ssh connected to …, but the remote machine has no rgit` | install rgit there, or set `RGIT_REMOTE_RGIT` to its full path (non-interactive logins use a short PATH) |
| `host_unreachable` | `ssh connection refused` / `ssh could not reach` / `ssh could not resolve` | nothing listens, wrong name, or no network |
| `other` | `invalid SSH remote: …` | the URL failed validation (section 2); nothing was started |

## 9. What was tested

Run `cargo test -p rustgit-transport-ssh -p rustgit-cli --test ssh_standin`.

| What | How | Real SSH? |
|---|---|---|
| Framing and robustness of the byte-stream adapter: round trips, a 12 MiB frame through bounded queues, ordering, idle/stalled-frame/stalled-write timeouts, peer close, truncated/corrupt/oversized/garbage frames, marker scanning with banners | `crates/transport-ssh/tests/pipe_framing.rs`, real OS pipes | no ssh involved |
| URL validation and argument-injection cases (hosts starting with `-`, spaces, newlines, `ProxyCommand`-like strings, shell metacharacters in paths and jump lists, `..`, percent-encoded tricks) | `target.rs` unit tests; `argv_and_standin.rs`; CLI `hostile_urls_are_rejected_before_ssh_is_ever_started` (also checks the stand-in was never started and no file was created) | no |
| Our argument list is read by the **real `ssh` client** as intended, and overrides a hostile user config (`StrictHostKeyChecking no`, `ForwardAgent yes`, `LocalForward`, `LocalCommand`) | `real_ssh_resolves_our_arguments_…` runs `ssh -G` with our argv | real `ssh` client, no server, no connection |
| The real ssh client's failure output is classified (connection refused to 127.0.0.1:1; missing client) | `real_ssh_failures_are_classified_without_a_server` | real client, no server |
| Full flow join → push → clone → fetch with two enrolled devices, Noise channel over the ssh byte stream, banners, jump option, JSON output, unenrolled device refused, error mapping and no retry on host-key/auth failures, `serve --stdio` stdout hygiene and exit behaviour | `crates/cli/tests/ssh_standin.rs` | **stand-in only** (`fake-ssh.sh`: *not SSH*; no network, encryption, authentication or host-key check; it just runs the remote command locally) |
| Reverse-tunnel recipe (section 6) | not executed: it needs a real sshd; the pieces (loopback `serve`, `tcp://` join, Noise over TCP) are covered by the TCP tests | **not run** |
| Real sshd round trip | `real_ssh_roundtrip`, `#[ignore]`, needs `RGIT_REAL_SSH_URL` | **not run**: no sshd was enabled on the development machine (`nc -z localhost 22` failed) and none was started or enabled for testing |

Not verified: a real sshd (host-key prompts and mismatches, key auth, `command=`/`restrict`, ProxyJump
through real hosts, `-J` chains), interactive passphrase prompts (the non-batch path), `ssh-agent`, Windows
OpenSSH client/server, Linux, a remote `cmd.exe`/PowerShell login shell, large transfers over a real
high-latency link. Windows: the code is cross-type-checked for `x86_64-pc-windows-gnu`, never run.
