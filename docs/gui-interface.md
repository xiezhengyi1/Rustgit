# CLI JSON interface for the desktop apps (version 1)

Both desktop apps drive the same Rust engine by running the bundled `rgit` binary with
`--json`. This keeps repository mutation in one process per command (the engine already
serializes writers with file locks) and avoids an FFI layer. `rgit serve` is the long-running
host process; the apps start and stop it as a child/background process.

## Conventions

* `rgit --json -C <project> <command> ...` prints exactly **one JSON object** on stdout and exits.
  * success: `{"ok":true,"data":{...}}`, exit code 0
  * failure: `{"ok":false,"error":{"code":"<snake_case>","message":"<human text>","paths":[...optional]}}`,
    exit code 1 (2 for `verify` with problems, 1 for `merge` with conflicts, in which case `ok` is
    still `true` and `data.conflicts` is non-empty)
* Commands that can take long with `--json` first print
  zero or more progress lines (implemented for `fetch`, `push`, `pull`; `clone`, `gc`, `verify` emit none yet), each a JSON object with an `"event"` key
  (`{"event":"progress","phase":"upload","done":12,"total":40,"bytes":123456}`), then the final result object
  above. Read stdout line by line; the last line is always the result. Closing stdin or sending SIGINT/
  terminating the process cancels; the operation resumes on the next run.
* Error codes used so far: `not_a_repository`, `dirty_working_tree` (with `paths`), `nothing_to_commit`,
  `merge_in_progress`, `unresolved_conflicts`, `branch_not_found`, `branch_exists`, `invalid_name`,
  `unportable_paths` (with `paths`), `ref_conflict` (push rejected: fetch and merge), `host_unreachable`,
  `permission_denied`, `quota_exceeded`, `out_of_space`, `invalid`, `other`; SSH remotes add `ssh_host_key_failed`
  (unknown or changed host key; never retried), `ssh_auth_failed`, `ssh_unavailable` (no `ssh` client installed) and
  `remote_rgit_missing` (the remote machine has no `rgit` command); see [`ssh.md`](ssh.md).
  Device authentication adds (never retried; the message is for people, the code is the contract):
  `host_key_mismatch` (the host is not the pinned identity: possible interception, do not re-pin without
  the owner confirming the fingerprint), `secure_channel_failed`, `security_required` (plaintext client,
  secure host), `not_enrolled` (device is not enrolled: needs an invitation), `device_revoked`,
  `permission_denied` (role too low for the operation), `invite_malformed`, `invite_invalid` (unknown or
  wrong secret), `invite_expired`, `invite_used`, `invite_revoked`, `key_storage_unavailable` (OS key store
  unusable; the message names `--insecure-file-keys`), `host_key_unavailable`, `not_hosted`, `host_serving`,
  `no_such_device`, `no_such_invitation`, `ambiguous`; see [`protocol.md`](protocol.md) section 12.
* Object ids are 64 hex characters; `short` is the first 10.
* Times are Unix seconds plus `tz_offset_min`.
* Paths use `/` on every platform.
* The schema is versioned: `rgit --json version` returns `{"cli":"0.1.0","json_interface":1,"format":1}`.
  Apps must refuse to run against a different `json_interface`.
* Text encoding is UTF-8. File contents are never sent through JSON except diffs (`diff` field, UTF-8
  text, binary files appear as "Binary files differ").

## Commands and `data` shapes

| Command | `data` |
|---|---|
| `version` | `{cli, json_interface, format}` |
| `init [--branch B]` | `{uuid, root}` |
| `status` | `{branch, head: id\|null, changes:[{path, kind:"added"\|"modified"\|"deleted"\|"renamed", from?: path}], skipped:[{path,reason}], merge:{kind,label}\|null, conflicts:[{path,kind,resolved}], ahead?, behind?}` |
| `commit -m M [paths]` | `{id, short, branch}` |
| `log [-n N] [rev]` | `{commits:[{id, short, parents:[id], author_name, author_email, timestamp, tz_offset_min, message, branches:[name]}]}` |
| `show REV` | `{commit:{...as above}, changes:[...], diff: text}` |
| `diff [A [B]]` | `{changes:[...], diff: text}` |
| `branch` | `{current, branches:[{name, id, short}]}` |
| `branch NAME [--from REV]`, `branch -d NAME` | `{name, id}` |
| `switch [-c] NAME` | `{branch}` |
| `restore ... / revert / merge / unshelve` | `{commit: id\|null, fast_forward, already_up_to_date, conflicts:[path], restored?:[path]}` |
| `resolve PATH [--ours\|--theirs]` | `{path}` |
| `shelve / shelves / shelf-drop` | `{name}` / `{shelves:[{ref,id,message}], recovery:[...]}` / `{}` |
| `stats` | `{commits, branches, objects, logical_bytes, stored_bytes, delta_objects, pack_files, pack_bytes, index_bytes, repo_bytes}` |
| `verify` | `{objects_checked, commits, problems:[text]}` |
| `gc` / `reindex` | `{moved, dropped, freed_bytes, repo_bytes_before, repo_bytes_after}` / `{objects, packs, problems}` |
| `remote` | `{remotes:[{name, url}]}` |
| `clone URL [DEST]` | `{dest, branch, head}` (no progress events yet) |
| `fetch` | `{up_to_date, updated:[{ref, old, new}], stats:{objects_transferred, objects_already_complete, objects_resumed, logical_bytes, wire_bytes_sent, wire_bytes_received}, state?}`; `state` is the current branch vs its remote-tracking ref: `up_to_date`, `behind`, `ahead`, `diverged`, `no_local_commits`. Also emits `{"event":"relation",...}` |
| `push` | `{up_to_date, updated:[{ref, new, generation}], already_applied?, stats}`; a rejected push is an error with code `ref_conflict` and exit code 3 (local commits are untouched: fetch, merge, push again) |
| `pull` | `{fetch:{...as fetch}, merge:{...as merge}\|null}` |
| `remote [list]` / `remote add N URL [--host-key FP]` / `remote remove N` | `{remotes:[{name,url,secure,host_key}]}` (`secure` is true when the host identity is pinned and the connection is encrypted) / `{}` / `{}` |
| `device id` | `{fingerprint, short, public_key, created, key_storage:"macos-keychain"\|"windows-credential-manager"\|"file", key_storage_protected}`; creates the identity on first use |
| `device reset --yes` | `{deleted:true}` (the device can no longer authenticate anywhere; hosts keep its record) |
| `invite create [--role read\|write\|admin] [--expires 24h] [--single-use[=false] [--max-uses N]] [--hint URL]... [--label T]` (in the hosted project) | `{uri, id, role, expires, max_uses, host_fingerprint, hints, project}`; **`uri` is the only place the secret appears**: show it once, never log it |
| `invite list` / `invite revoke ID` | `{invitations:[{id, role, status:"active"\|"expired"\|"used"\|"revoked", created, expires, uses, max_uses, label}]}` / `{invitation:{...}}` (no secrets) |
| `join URI\|- [DEST] [--add-remote] [--remote N]` | `{project, role, host_fingerprint, url, remote, dest, branch, head, device_fingerprint}`; `-` reads the link from stdin (keeps it out of the process list) |
| `devices list` / `devices revoke D` / `devices role D ROLE` | `{devices:[{fingerprint, short, name, role, status:"active"\|"revoked", added, last_seen, revoked_at, seconds_since_seen}]}` / `{device:{...}}` / `{device:{...}}`; `D` is 6+ fingerprint digits or an exact name |
| `host id` / `host rotate [--compromised]` | `{fingerprint, short, created, key_storage, project}` / `{old_fingerprint, fingerprint, succession_recorded, invitations_revoked}` (rotate needs `serve` stopped) |
| `config KEY [VALUE]` | `{key, value}` |
| `serve` (with `--json`) | prints one `{"event":"listening","addr":"127.0.0.1:PORT","project":uuid,"secure":bool,"fingerprint":hex64\|null}` line, then runs until terminated (no further events yet; `--port-file` is the other way to learn the bound address). **Authenticated and encrypted by default** (`secure:true`, `fingerprint` = the host identity devices pin); `--insecure-open-access` (loopback only) gives `secure:false`. `--insecure-listen-any` is a no-op |
| `serve --advertise` (with `--json`) | additionally prints `{"event":"advertising","instance":"rgit-xxxxxxxx-xxxx","project_hint":"16hex","port":N}` after `listening`; publishes `_rustgit._tcp` over mDNS (see [`discovery.md`](discovery.md)); sends the mDNS goodbye on SIGINT/SIGTERM |
| `serve --iroh [--iroh-relay URL]... [--iroh-key-file F] [--iroh-hint-addrs]` (with `--json`) | additionally prints `{"event":"listening_iroh","url":"iroh://ID?relay=URL","endpoint_id":hex64,"relay_online":bool,"relay":null\|"public"\|[url]}` after `listening`: `url` is what to put into `invite create --hint`; `relay_online:false` means the relay did not answer within 15 s (the host keeps trying). Needs a build with the `iroh` cargo feature (otherwise an error says so) and is refused with `--insecure-open-access`. See [`relay.md`](relay.md) |
| `serve --stdio` | **not for apps**: serves one session on stdin/stdout for `ssh host rgit serve --stdio`; stdout carries protocol frames, so it is refused with `--json` |
| `discover [--timeout S] [--watch] [--project UUID \| --remote NAME] [--include-loopback] [--no-probe]` | event lines `{"event":"host","host":H}` / `{"event":"lost","instance":…}`, then `{hosts:[H], timeout_ms}` where `H = {instance, addresses:[ip], port, urls:["tcp://ip:port"], project_hint: 16hex\|null, protocol_version: n\|null, matches_project: bool\|null, reachable: bool\|null, verified: false, source:"mdns"}`. **Hints only, never identities** (`verified` is always `false`): connect and authenticate before trusting. `--watch` streams events until terminated and has no final object |

### Connection state of `iroh://` remotes

Every command that used an `iroh://` remote or invitation hint (`join`, `clone`, `fetch`, `push`, `pull`,
`remote` operations that connect) adds a `connection` object next to its result in `data`, and to `error` when
it failed:

```json
{"transport":"iroh","state":"relayed","path":"relay https://relay.example.org/","used_relay":true,
 "used_direct":false,"path_switches":0,"error":null,"remote":"<host endpoint id>","local":"<this device's endpoint id>"}
```

* `state`: `direct` | `relayed` | `unreachable` (| `connecting` while a connection is being made). It is the path
  QUIC is sending on *now*; `used_relay`, `used_direct` and `path_switches` describe the connection's history (a
  connection usually starts relayed and may upgrade to direct, or fall back). Present relayed as "working, via
  relay" and `unreachable` as "cannot reach the host right now" (`error` has the reason in words; the command's
  error `code` is `host_unreachable`). Never present direct as guaranteed: it depends on both networks.
* While a command runs, `--json` also streams `{"event":"connection", ...same fields...}` whenever `state` or `path`
  changes, so a UI can show the live path during a long transfer.
* Commands on `tcp://` and `ssh://` remotes have no `connection` object.

Global options for key custody (all commands): `--insecure-file-keys` / `RGIT_INSECURE_FILE_KEYS=1`
keeps private keys in `0600` files under `--key-dir` / `RGIT_KEY_DIR` instead of the OS key store. It is an
explicit opt-in that is never chosen automatically; an app should not pass it unless the user asked.

The apps must not parse human-readable output; only `--json`.

`restore --from REV .` restores the entire tracked working tree, including the first/root version.
It preserves the current branch/head and unrelated untracked files. Dirty target files require
`--force`, which saves a recovery snapshot first, including ignored local files that would be overwritten.
The desktop history action "Restore to this version" uses this operation, not `revert`.
