# Status against the execution plan

Legend: **Done** = implemented and covered by automated tests; **Partial**; **Not started**.
Nothing here is marked done on the basis of mocks.

## Plan step 1–3: format, storage, local version control

| Requirement | Status | Evidence / limitation |
|---|---|---|
| Repository format spec with test vectors | Done | `docs/format.md`, `crates/core/tests/vectors.rs` |
| Branches share content, creation is metadata only | Done | test `branches_share_content_and_cost_only_references` (100 branches: object count and stored bytes unchanged); benchmark column "100 branches" |
| Every commit restores byte-identical content (adds, deletes, modifies, renames, merges) | Done | `commit_status_log_and_exact_restore_*`, `large_file_local_edit_*`, `gc_drops_unreachable_*` |
| Content-defined chunking, dedup, zstd, packs, delta, solid groups | Done | `crates/store` tests (15 integration, 7 unit) |
| GC with grace period, pinned delta bases, recovery refs as roots | Done | `repack_*` tests, `gc_*` core test |
| verify / reindex, index rebuild without inventing content | Done | `damaged_index_is_rebuilt_*`, `corrupted_pack_is_reported_by_verify` |
| Crash safety of pack append (torn tail, durable-but-unindexed records) | Done at the store level | `torn_append_*`, `durable_pack_records_missing_from_index_*`. Not yet tested with real power loss or full disks |
| Portable paths, unsafe symlinks, case collisions, reserved names | Done | `case_collisions_*`, `unportable_tree_from_a_peer_*`, `symlinks_*` |
| Three-way merge, explicit conflicts, binary conflicts keep both | Done | `text_conflict_*`, `binary_conflict_*`, `delete_modify_conflict_*` |
| Shelve / unshelve, restore with recovery snapshot, revert | Done | corresponding tests |
| Protect uncommitted work on switch | Done | `switch_changes_files_and_protects_uncommitted_work` |
| Detect writes during scan | Partial | size+mtime are compared before/after reading a file (`FileChanged`); no stress test yet |
| Newline conversion setting | Not started | bytes are always preserved |
| Windows path/symlink behavior | Untested | code paths exist but were only run on macOS |

## Storage-efficiency targets

See `docs/benchmarks.md`. Measured per workload; targets that are not met are listed
there as not met.

## Plan step 4: protocol, hosting and synchronization

Implemented in `crates/protocol` and `crates/host`; user-facing commands `rgit serve`, `clone`,
`fetch`, `push`, `pull`, `remote`. Design and failure behaviour: [`protocol.md`](protocol.md).
Everything marked Done runs real `rgit serve` / client **processes** or an in-process host over
real loopback TCP against real repositories; the only fakes are two hostile test peers
(a raw-frame client and a lying host) written specifically to test rejection.

"Crash" below means the process exits abruptly at a named point (`RGIT_FAILPOINT`), with bytes
already handed to the OS surviving. Nothing here was tested against power loss, torn sector
writes, or a full disk.

| Requirement (plan section 8) | Status | Evidence / limitation |
|---|---|---|
| Versioned protocol, negotiation | Done | `version_negotiation_and_malformed_hello`, message round-trip/truncation tests. Only version 1 exists, so negotiating between two *different* real versions has not been exercised |
| List refs; batched have queries; transfer only missing logical objects | Done | `push_clone_fetch_roundtrip` (3 MiB unchanged file not re-sent: incremental push < 20 KB), `only_changed_objects_are_listed`, CLI resume tests count objects |
| Wire independent of pack layout / compression | Done | wire carries logical bytes (optionally a delta, always re-hashed); benchmark: rgit wire bytes identical for an unmaintained and a `gc --aggressive` repository |
| Staging area, hash + dependency verification before publish | Done | `corrupt_object_is_rejected_and_nothing_is_staged`, `missing_dependencies_block_the_ref_update`, `corrupted_staged_object_is_detected_and_resent` |
| Durable publish (`store.flush`) before the ref CAS; ack after durable | Done | order in `Host::update_ref`; crash tests `crash_before_publish`, `crash_between_publish_and_ref_update`, `crash_after_ref_update_before_ack`. The ack-after-durable ordering is by construction plus those crash points; a test cannot observe an fsync ordering without power loss |
| CAS on expected head **and** generation; fast-forward enforced separately | Done | `update_ref_is_idempotent_and_checks_generation`, `history_replacement_needs_force_and_force_is_off`; mutation check: removing CAS/FF made `concurrent_pushes_from_many_processes_lose_nothing` fail |
| Force push disabled by default; delete disabled by default | Done | CLI `force_push_is_disabled_by_default_and_works_when_enabled`, `force_and_delete_work_when_the_host_allows_them` |
| Retries idempotent | Done | applied-update record committed with the ref; replay test; `crash_after_ref_update_before_ack` (retry is a no-op, no duplicate commits) |
| Resumable transfers, confirmed objects not resent | Done | `client_interrupted_mid_upload_resumes_without_resending`, `host_crash_mid_upload_resumes_after_restart`, `interrupted_fetch_resumes_without_refetching_received_objects` (counts: sent + resumed == total) |
| Bounded batches, backpressure | Done | `batches_are_bounded_and_get_returns_partial_results`; uploads are one acknowledged batch at a time. No bandwidth throttling |
| Quotas, per-device limits | Done, partly | session-bytes quota (`quotas_limit_uploads`), per-device connection limit tested. Total-staging, repo-size, open-sessions-per-device and plan-size limits are implemented but have no test. "Device" is self-declared until step 9 |
| Timeouts | Done | `idle_connections_time_out`, `unresponsive_host_times_out`; frame-stall timeout shares the code path but has no dedicated test |
| Cancellation | Done | `cancelled_upload_stops_between_batches_and_resumes_later` (API-level token; the CLI has no cancel UI beyond Ctrl-C, which is an interruption and resumes) |
| Corrupt objects rejected (both directions) | Done | host side above; `corrupt_objects_from_the_host_are_rejected_by_fetch` (lying host); bad frame CRC and oversized frames refused (`oversized_and_garbage_frames_are_refused`) |
| Concurrent pushes lose nothing; rejected push keeps local commits and explains | Done | `concurrent_pushes_from_many_processes_lose_nothing` (6 processes racing on one branch; 13 rejections had to fetch+merge; every commit present, `verify` clean), `..._different_branches_keep_every_head`, `rejected_push_keeps_local_commits_and_explains` |
| Offline commits sync later | Done | `offline_commits_sync_after_the_host_comes_back` |
| Remote pushes never touch the host user's working directory | Done | `pushes_never_touch_the_host_users_working_directory`; `serve` refuses to host a working project |
| Single writer per hosted repository | Done | OS file lock (`second_serve_on_same_dir_fails`, `hosting_refuses_unsafe_setups`); gc/reindex refuse while serving |
| Notify subscribers of new heads | Partial | `Watch` long-poll + `rgit fetch --watch` (`watch_wakes_on_ref_change`); fetch only, never merges or checks out. No push-style notification, no multi-host fan-out; the CLI watch loop itself is untested |
| gc cannot leave a stale "complete" cache | Done | `gc_does_not_leave_the_host_believing_a_holey_tree_is_complete` builds the hazard (old pack drops a chunk, recent pack keeps the tree) and checks the push is re-sent and `verify` is clean |
| Out-of-space leaves acknowledged history intact | **Not verified** | write errors map to `OutOfSpace` and nothing acknowledged is ever rewritten, but no test fills a disk |
| Power loss / torn writes / `F_FULLFSYNC` behaviour | **Not verified** | durability calls are `sync_all` (which std documents as `F_FULLFSYNC` on macOS) on files and directories; directory fsync is skipped on Windows; none of it has been checked with real power loss or on Windows |
| Encrypted, authenticated connection; device authorization | **Done on macOS (not on Windows)** | see "Plan step 9" below. Plaintext/`OpenAccess` now needs `serve --insecure-open-access` (loopback only) |

Transfer sizes against Git are in [`benchmarks.md`](benchmarks.md).

Known defects and limits of this step (details in `protocol.md` section 11): objects over 8 MiB
cannot be transferred; the fetch plan is held in memory; closure and fast-forward checks are
O(history); one writer lock serializes everything; delta hints are path/position heuristics.

## Plan step 9: encryption, device identity, invitations, roles, revocation

Implemented in `crates/protocol` (`secure`, `identity`, `invite`), `crates/host` (`auth`, `authz`,
`hostid`, `keystore`) and `crates/cli/src/auth.rs`. Design and threat model:
[`protocol.md`](protocol.md) section 12; reasons: [`decisions.md`](decisions.md) D19-D25. Tests are
real `rgit serve` / `rgit` processes over real loopback TCP (`crates/cli/tests/auth_processes.rs`),
an in-process host over real TCP (`crates/host/tests/auth_tests.rs`), and the channel against real
TCP relays that forward, swap and corrupt bytes (`crates/protocol/tests/secure_channel.rs`).
Nothing in them mocks cryptography. The only fakes are hostile peers written to attack the channel.

| Requirement | Status | Evidence / limitation |
|---|---|---|
| Ed25519 device key in platform-protected storage | **Done on macOS Keychain; Windows Credential Manager written, never run** | `keystore.rs`; `platform_store_roundtrip` (ignored by default, run with `-- --ignored`) passed against the real login keychain; every other test uses the file store so it never touches the Keychain. Dev builds are unsigned: macOS asks again for approval whenever the binary changes (a hung prompt is cut off after 30 s with an explanation); release builds need a stable signing identity |
| File fallback only with an explicit flag, never silent | Done | `--insecure-file-keys` / `RGIT_INSECURE_FILE_KEYS`; no fallback code path exists; file must be `0600` (group-readable file refused); warning on every use (`device_identity_is_stable_private_and_never_silently_unprotected`). Not tested: the "keychain unavailable" error on a machine whose keychain is really unusable |
| Encrypted + mutually authenticated channel, vetted construction, transport-agnostic | **Done** | Noise XX (`snow`) inside the protocol layer over any `Duplex`; TCP and the SSH stand-in run it. Independent review: **none**; `snow` is unaudited (D19) |
| Host identity pinned from the invitation; addresses are hints only | Done | `wrong_pinned_host_key_and_man_in_the_middle_are_rejected` (wrong pin, key swap at the remote's address, invitation hint rewritten to an attacker: all `host_key_mismatch`, the invitation is not consumed); the client aborts before revealing its identity (`wrong_pinned_host_key_is_rejected_and_the_device_is_not_revealed`) |
| Tampered ciphertext rejected | Done | bit flips in handshake messages and records, replayed record, host and client directions (`secure_channel.rs`, `tampering_with_a_live_session_kills_it_and_the_host_keeps_serving`) |
| Invitation `projectapp://join/<base64url>` with version, project, host fingerprint, hints, expiry, role, secret; default expiring + single-use; manual paste | **Done** | `invite.rs` unit tests (round trip, whitespace-tolerant paste, malformed inputs never echoed); defaults in `invite_needs_a_hint_and_defaults_are_expiring_single_use_read_only`. Redeeming over SSH/Iroh hints is implemented only as far as the transport accepts the URL (only `tcp://` is exercised) |
| Expired / replayed / revoked / malformed / unauthorized invitations fail safely with distinct errors | Done | `invitation_failures_are_distinct_and_safe`, `the_host_rejects_expired_used_revoked_and_unknown_invitations_with_distinct_errors`; unknown id and wrong secret are indistinguishable by design |
| Single-use under concurrent redemption | Done | 8 real processes race one invitation (`single_use_invitation_survives_a_race_between_processes`), 24 threads on separate database connections (`authz.rs`): exactly one wins |
| Roles read/write/admin enforced per operation | Done | `the_host_enforces_roles_per_operation_even_against_a_hand_written_client`; read-only device cannot push (`join_clone_push_fetch_and_roles...`), write cannot force/delete, admin can (`write_role_cannot_force_or_delete_but_admin_can`). Roles are per project, not per branch |
| Revoke device; active sessions end; revoked key cannot re-enroll | Done | `revoking_a_device_ends_its_open_sessions` (idle connection and a blocked `Watch` fail within seconds), `an_idle_connection_of_a_revoked_device_is_closed_by_the_host`, CLI `revoked_device_loses_access_on_the_next_connection`. A request already executing (one large `Put`) finishes. Prevents future access only |
| Revoke invitation; rotate host credential; list devices with last seen | Done | `invite revoke`, `host rotate` (succession followed automatically; `--compromised` forces re-pinning: `host_key_rotation_follows_succession_and_compromise_requires_repinning`), `devices list` shows `last_seen` (updated at connect and every 60 s) |
| Secrets never in logs / `--json` output | Done for what was tested | `secrets_never_appear_in_logs_lists_or_error_output` scans host `--log` output, all JSON/stderr of joins (successful, replayed, tampered), `invite list`, `devices list`, `device id`, and the authorization database for the enrollment secret in hex/base64/URI form and for both key seeds. The link itself is printed by `invite create` by design |
| `serve` authenticated by default; plaintext only with `--insecure-open-access` on loopback | Done | `open_access_stays_loopback_only_and_authenticated_serving_may_listen_anywhere`; all pre-existing process tests now pass the flag |
| Overhead of encryption | Measured | [`benchmarks.md`](benchmarks.md) "Encrypted channel" |
| Remote administration over the protocol (invite / revoke from another device) | **Not started** | local CLI on the host machine only |
| Windows | **Not run** | Credential Manager and Windows file permissions unexercised |
| Desktop apps | **Integrated; verified on macOS only** | Both adapters/UI expose secure hosting, invitations, roles/revocation, fingerprint pinning, invitation join via stdin, nearby hints and Iroh state. Swift adapter 32 tests, macOS app 27, TypeScript 121, native shell 9. See app docs for limits; Windows OS execution and deep-link dispatch unverified |

## Plan steps 5–7: desktop integration and remaining validation

| Area | Status |
|---|---|
| Hosting tray/menu bar, login item | Windows: **written, never run on Windows** (tray menu, close-to-tray, `HKCU\...\Run` login item; see the Windows section below). macOS: menu-bar item with per-project start/stop and a keep-hosting-when-window-closes option, built and launched (window not inspected); login item via `SMAppService` is wired but **never exercised**; see the macOS section below |
| Invitations, device authorization, roles, revocation | **Integrated in the engine/CLI and both desktop apps; tested on macOS**: see "Plan step 9" below |
| mDNS local discovery | **Done on macOS** (advertise + browse, two real processes, loopback and LAN interface); Windows/Linux untested. See [Connectivity](#connectivity-ssh-transport-and-mdns-discovery) and [`discovery.md`](discovery.md) |
| SSH transport (`ssh://`, `rgit serve --stdio`) | **Done against a test-only stand-in and the real `ssh` client's argument handling; never run against a real sshd.** See [Connectivity](#connectivity-ssh-transport-and-mdns-discovery) and [`ssh.md`](ssh.md) |
| NAT traversal, relay, Iroh | **Evaluated and implemented as an optional feature; real hole punching across two NATs NOT verified.** `iroh://` remotes, `serve --iroh`, forced-relay and relay-outage scenarios, self-hosting guide validated against the real `iroh-relay` binary; see [Connectivity: Iroh](#connectivity-iroh-nat-traversal-and-relay) and [`relay.md`](relay.md). Packaging scripts now build `rgit` with `--features iroh`; a plain `cargo build` does not |
| Tauri/React Windows app | **Written, NOT run on Windows.** UI, JSON client and Rust shell are tested on macOS only; see the Windows section below |
| SwiftUI macOS app and Swift adapter | Adapter: **done**, 32 tests against real `rgit`. App: **built with SwiftPM (no Xcode), launched, every screen rendered offscreen; not seen or clicked in a live window**. See the macOS section below and [`macos-app.md`](macos-app.md) |
| Installers, signing, clean-machine tests | Windows NSIS: cross-built on macOS, archive and bundled x64 files verified; native install/launch untested. MSI: unbuilt. Windows artifacts unsigned. macOS: `build-app.sh` builds `RustGit.app` + `.zip`/`.dmg`, ad-hoc signed, **not notarized**; Developer ID/notarization steps documented, never run; arm64 only. No clean-machine test on any OS |
| Cross-platform (Windows <-> macOS) interoperability | Not started; protocol and host code have only run on macOS |
| Chinese/English localization | Windows app: done in code and tested for key/placeholder parity; never seen on a real Windows display. macOS app: English + Simplified Chinese `Localizable.strings`, key/format parity tested; never viewed in Chinese UI |

## Connectivity: SSH transport and mDNS discovery

Added after step 4; details in [`ssh.md`](ssh.md) and [`discovery.md`](discovery.md). Evidence is from macOS 27
(arm64), OpenSSH_10.3p1 client, a Wi-Fi/Ethernet LAN interface plus loopback. **Windows and Linux: not run.**
**No sshd was available** (`nc -z localhost 22` failed) and none was enabled, started or configured for testing;
nothing here changed Remote Login, `sshd_config`, `authorized_keys` or any firewall.

| Requirement | Status | Evidence / limitation |
|---|---|---|
| Application protocol over a framed byte stream through the system `ssh` client, structured arguments, no shell interpolation of invitation/URL data | Done | `crates/transport-ssh`; `argv_and_standin.rs` checks that URL-derived values only appear after `--` or as an option's own value |
| URL validation and argument-injection resistance (hosts starting with `-`, spaces, newlines, `ProxyCommand`-like strings, shell metacharacters in paths/jumps, `..`, percent-escapes) | Done | 5 unit tests (about 80 hostile strings) plus CLI test: rejected before `ssh` is started, no file created, same for a hostile value planted in `remote.origin.url` |
| `rgit serve --stdio` (one session over stdin/stdout; usable as an sshd forced command) | Done on macOS | `ssh_standin.rs`: stdout carries only the marker and frames, exits when the client vanishes, survives garbage, refuses `--json` and a missing repository |
| `ssh://[user@]host[:port]/path` remotes, user ssh config, ProxyJump / `-J` (`?jump=`) | Done for argument handling | the real `ssh -G` resolves our argv as intended (user, port, `-J`, host); real `ssh` **connections** through jump hosts were never made |
| Host-key verification stays on; failure shown clearly | Done for the policy and messages; **not shown against a real server** | `StrictHostKeyChecking` is always set to `ask`/`yes`/`accept-new` (opt-in by environment only, never by URL/invitation); `ssh -G` confirms it overrides a user config containing `StrictHostKeyChecking no`; the real client's connection-refused output is classified; host-key and auth failure texts are exercised through the stand-in (which reproduces ssh's wording, it is not ssh) |
| Device authentication and encryption over SSH | Done | the Noise channel from step 9 runs unchanged over the ssh byte stream: join, push, clone, fetch with two enrolled devices, and an unenrolled device is refused (stand-in) |
| Reverse tunnels to a reachable server | Documented only | `ssh -R`/`-L` recipe in `ssh.md`; the client just dials the forwarded `tcp://` address; not executed (needs sshd) |
| System sshd setup (`authorized_keys` `command=...,restrict`, `Match`/`ForceCommand`) | Documented as advanced opt-in, **never tried** | the commands are written from the sshd documentation, not validated against a running sshd |
| Real-sshd test | `#[ignore]` test provided, **not run** | `real_ssh_roundtrip` with `RGIT_REAL_SSH_URL` |
| mDNS/DNS-SD advertise `_rustgit._tcp` with minimal metadata (opaque short hint, protocol version, port; no project name) | Done on macOS | `crates/discovery`; two real processes on loopback and on the LAN address; record content asserted; `rgit serve --advertise` |
| Browse / `rgit discover --json` with timeout and streaming; results are hints only | Done on macOS | `discover_processes.rs` (events then result, `--watch`, hint matching, reachability probe, `verified:false` always) |
| mDNS goodbye on shutdown | Done for clean exit and SIGINT/SIGTERM | SIGTERM test sees the `lost` event; SIGKILL/crash/power loss send nothing, records live up to 75 min (the reachability probe flags them) |
| Multicast skipped when blocked | Implemented | tests print `SKIPPED ... NOT exercised`; on this machine nothing was skipped |
| Not covered | | more than one physical machine; IPv6 browsing; multicast-filtering networks; macOS Local Network permission for a GUI app; name conflicts between advertisers; Windows/Linux; NAT traversal/relay (not part of this work) |

## Connectivity: Iroh (NAT traversal and relay)

Decision, versions and costs: [`decisions.md`](decisions.md) D20. How to run a relay and what it can see:
[`relay.md`](relay.md). Code: `crates/transport-iroh` (transport, listener, status), `crates/cli/src/iroh.rs`
(wiring, config), cargo feature `iroh` of `rustgit-cli` (off in a plain `cargo build`, **on in the packaging scripts**
(`package-cli.sh`, `build-app.sh`, `build.ps1`, the Windows workflow); +9.8 MB, +53 s cold build; Windows-gnu and
x86_64-apple-darwin builds with the feature were type-checked only, the Windows one never run).
Evidence is from **one macOS 27 (arm64) machine behind one NAT**, Iroh/`iroh-relay` 1.3.0, Rust 1.99.1.
Windows and Linux were not run. Every transfer below compared the resulting repository byte for byte.

| Requirement (plan section 9, Connectivity) | Status | Evidence / limitation |
|---|---|---|
| Evaluate Iroh from primary documentation; pin versions; record build cost, maturity, licence, alternatives | Done | D20: API names read from the 1.3.0 crate source; `iroh =1.3.0`, `iroh-relay =1.3.0`; MIT OR Apache-2.0; 1.x since June 2026 (seven releases in 15 weeks); MSRV 1.91 (workspace 1.89); +165 crates; libp2p and QUIC+STUN assessed from documentation only, **not built** |
| Transport adapter carrying the unchanged protocol over Iroh; dial by endpoint id with hints | Done | `iroh_transport.rs` (16 tests): push + clone over loopback-direct and over a relay; the protocol, host and device-auth layers are untouched and run on top |
| `iroh://<endpoint-id>[?relay=URL][&addr=ip:port]` for `serve`/`clone`/`fetch`/`push`/`join`/`remote add` | Done | `cli_processes.rs`: `serve --iroh`, invitation with an `iroh://` hint, `join`, `push`, `fetch` as real processes, with a locally started relay; URL parser unit tests |
| Stable address when IPs change | Done for host restarts and stale hints | host restarted on another UDP port with the same key file: same endpoint id, the old URL (stale `addr`) still connects (`host_keeps_its_identity_when_its_address_changes`). No test with a real DHCP/Wi-Fi change |
| Hole punching | **Not verified** | Iroh's own implementation is used unchanged. Both endpoints were on one machine, so "direct" meant loopback/LAN; **no packet was punched through a NAT, symmetric/CGNAT/UDP-blocked cases were never seen.** Iroh's documentation claims about 9 in 10 networks allow direct paths; we neither measured nor rely on that |
| Encrypted relay fallback; relay cannot decrypt | Done and tested | TLS 1.3 raw-public-key authentication inside QUIC (read in `iroh/src/tls`); `the_relay_sees_only_ciphertext_and_some_metadata` taps a real relay's wire: no file bytes, commit message or note text visible, endpoint ids visible; a control shows the tap sees plaintext on TCP; `a_relay_cannot_stand_in_for_the_host` |
| **Forced-relay scenario** | Done | no UDP socket on either side (`direct=false`), local relay, multi-MiB push + clone, state `relayed`, via the library and via the CLI (also against the released `iroh-relay` binary, 60 MiB push) |
| Network changes | Partly | source-address change of the peer twice mid-push (UDP proxy): one connection survived, 8.0 s vs 3.5 s baseline. OS notification call (`network_changed`) mid-push is harmless but, on an unchanged network, does nothing. **Real interface changes (Wi-Fi switch, VPN, sleep/wake, IPv4/IPv6) were not made** |
| Unavailable infrastructure | Done | relay killed mid-push: 1.5 s outage absorbed by the open connection; 8 s outage (3 s stall timeout) forced reconnects (3 attempts) and the push completed; dead relay, host offline behind a live relay, wrong endpoint id, no hints, direct-only without relay: `unreachable` within the timeout, readable message, JSON `host_unreachable` |
| Controlled production relay config; self-hosting guide; no reliance on public dev relays | Done, **not deployed publicly** | `relay_*`/`direct`/`bind`/`key_file` settings (flags, env, `iroh.conf`), relay token and private CA support, no n0 relays/DNS unless `public_relay = true` (suite also passes with all non-loopback network denied). [`relay.md`](relay.md) configs were run on the released binary (private-CA certificate, shared token, allowlist, rate limit, metrics, `healthz`). **Let's Encrypt mode, a public DNS name, systemd limits, Docker image and a relay across the internet were not exercised** |
| State for the UI: direct, relayed, unreachable | Done | `connection` object in `--json` results/errors and `{"event":"connection"}` lines (see [`gui-interface.md`](gui-interface.md)); the state is Iroh's selected path, not a guess; both apps now show connection events; real two-NAT app use remains unverified |
| Not covered | | two real NATs; cellular/CGNAT; IPv6-only; Windows/Linux; more than a handful of devices; relay load and throughput limits; mixed Iroh versions; the n0 public relays; hostile relays that selectively delay traffic |

## Environment used

macOS 27 (Darwin 27.0.0) on arm64, Rust 1.99.1 (installed via rustup, minimal profile),
Swift 6.4 command line tools, Git 2.54.0 (Apple), SQLite bundled with rusqlite 0.40.

## Packaging and the JSON interface (added by the main session)

| Item | Status | Evidence / limitation |
|---|---|---|
| `rgit --json` for all commands (contract in `gui-interface.md`) | Done | `crates/cli/tests/json_interface.rs` (5 tests, including real push progress lines); progress events only for fetch/push/pull |
| CLI archives for macOS arm64, macOS x86_64, Windows x86_64 + checksums | Built | `scripts/package-cli.sh`, `docs/packaging.md` |
| Those archives actually run | arm64 macOS only | the Intel and Windows builds were not executed (no Rosetta, no Windows); all unsigned |

## Windows desktop app (`apps/windows`, docs in [`windows.md`](windows.md))

**Native Windows smoke verification completed on 2026-10-20:** replacement installation, app launch,
the actual Commit and share button, and a second working copy pulling a matching file all passed.
The installer remains an unsigned macOS cross-build. Most automated results in the table below were
obtained on macOS or by cross-compilation; they are not a full native Windows test-suite result.
See [the detailed Windows verification record](windows.md#native-windows-smoke-verification--2026-10-20).

| Item | Status | Evidence / limitation |
|---|---|---|
| JSON client (`src/rgit/client.ts`): typed calls, NDJSON progress, error mapping, cancel, exit-code rules | Done (macOS) | `test/client.test.ts` against a fake `rgit` replaying recordings; `test/contract.test.ts` replays recordings of the **real** `rgit --json` and checks every shape; `test/live.test.ts` drives the real binary (init, selective commit, branches, merge conflict, resolve, gc/verify/reindex, serve, push, clone, fetch, rejected push, pull, cancel) |
| Projects, Changes, History & branches, Storage & recovery screens | Done in code; UI flows tested in jsdom | `test/app.test.tsx`; successful commit-and-share UI flow also exercised in native Windows WebView2 |
| Sharing & connections | Integrated; tested on macOS | Invitations, devices, role changes/revocation, identity/pins, mDNS hints and Iroh status. Real secure process flow in `test/sharing-live.test.ts`; OS deep-link dispatch and live Windows rendering unverified |
| "Committed locally / received by server / fetched by peer" | Partial | local-vs-server counts are derived from `log` because `status` has no ahead/behind; "fetched by a peer" is not tracked by the engine and the UI says so |
| Hosting (`rgit serve` as a child), tray, close-to-tray, quit stops host | Written; host start/stop/error reporting **tested with a real `rgit serve` on macOS** (`cargo test` in `src-tauri`) | tray, window hiding and the Windows process flags cannot be exercised here |
| Start at login | Written; **not run** | per-user `HKCU\...\Run` value via `winreg`, no service, no elevation; a Rust test of set/get/delete exists and only runs on Windows (CI) |
| English + Chinese, keyboard navigation, labels | Done in code; tested in jsdom | `i18n` parity tests; not tried with Narrator/NVDA or at high DPI |
| Tauri shell compiles and builds | Done | `cargo check`/`clippy`/`test` and `tauri build --no-bundle` on macOS (the built binary starts and stays up; no window inspection); `cargo check`/`clippy --all-targets --target x86_64-pc-windows-gnu` clean |
| Windows installers and build workflows | NSIS cross-build completed; native PowerShell/CI flow unexecuted | `scripts/windows/build-cross.sh` produces `dist/windows/RustGit-0.1.0-windows-x64-setup.exe` on macOS. Archive CRC, x64 payloads, GUI subsystem and release-source matches verified. Chinese/English per-user installer. 125 frontend tests and production build pass. Native replacement installation, launch and successful sharing/pull verified; MSI remains unverified; the Windows workflow defines these smoke tests |
| Large projects (30k changed files): commit not blocked by activity, instant cancel, paths via stdin, virtualized list | Done in code; tested on macOS | `test/large.test.tsx` (30,000 rows in jsdom: 21 DOM rows, toggle ~40 ms; Commit enabled during a running push; Cancel returns to idle even if the process never exits; failing call releases the lock), `test/selection.test.ts` and `test/client.test.ts` (30,000 paths go through stdin, never argv), `test/live.test.ts` (real `rgit`, 30,000 files, `--paths-from`/`--except-from`), Rust test of the stdin payload. jsdom has no layout: WebView2 painting/scroll performance and the Windows command-line limit itself were not exercised. Engine `status` on 30k files took ~7 s here |
| Hosting join address and "Allow other devices on this network" | Secure flow tested on macOS | LAN access off by default; `serve --listen 0.0.0.0:PORT --advertise` preserves authentication. Show addresses, create invitations, enroll the owner, publish the working project. Native-shell test pushes/clones through this Mac's LAN address. Windows firewall and adapter behavior unverified |
| Code signing | **Not signed** | no certificate; `build.ps1 -CertThumbprint` and the signtool commands are documented but unexercised |

Engine behaviours found while replaying real `--json` output were reported to the main session and fixed or documented there (rejected push now `ref_conflict`, result shapes in `gui-interface.md`). `status` still has no `ahead`/`behind`, so the client derives them from `log`.

## Follow-up fixes (2026-10-20)

Project-local `.gitignore` files now work without Git metadata, including nested patterns and negations.
`.rgitignore` overrides rules at the same directory; rules outside the project and global Git ignores
are not used. Tracked files remain tracked even when new ignore rules match them. Real CLI validation
confirms ignored untracked files do not enter status, a commit, a push or a clone.

Whole-version restoration uses `restore --from REV .`, not `revert`. It works for root commits and
preserves history and unrelated local files. Dirty files, including ignored local files colliding with
historical paths, require confirmation and are included in the recovery snapshot before overwriting.
The history preview is cancellable and reports load failures instead of spinning forever.

The macOS Sharing page now leads with an invitation workflow and separate send/get actions;
connection details and device fingerprints live under collapsed advanced settings. Invitation setup
explains LAN versus internet connections and permissions before a link is created.

Validation: workspace Rust tests passed (245 passed, 2 ignored), and macOS tests passed
(27 tests in 10 suites). Temporary repositories exercised root-version restoration, dirty-file
protection/recovery and ignored-file exclusion through push/clone. Build `202610201657` was packaged,
signature-checked and installed at `/Applications/RustGit.app`; its DMG checksum verification passed.
Live UI checks opened and cancelled the restore confirmation and invitation sheet without restoring
the user's files or creating another invitation.

The Changes screen now separates filenames and directories, supports file/path filtering, and uses
numbered diff rows with a full-width document for horizontal scrolling. The commit composer has an
example placeholder, a selection summary and a prominent action. Filtering leaves the global selection
unchanged. Normal and minimum-window snapshots were reviewed.

The sidebar shows the persisted recent-project list (up to 30). Switching reuses each project's model,
including its file selection and commit-message draft for the current session. Detail view state resets
between projects; failed opens keep the current project and show the error. A newer open request wins
over an older in-flight one. Tests with two identically named temporary projects verify independent
drafts/selections, recent-list persistence and failure handling. The macOS suite passed 31 tests in
11 suites, including diff line numbering across hunks, new files and deleted files.

Build `202610201713` was packaged, signature-checked and installed over the previous app; its DMG
checksum verification passed. Live checks verified the two existing sidebar projects, switching in
both directions, filtering without changing selected-file counts, and horizontal scrolling reaching
both ends of a long diff line. These checks did not commit or restore the user's project files.

## Combined commit and share (2026-10-20)

Both desktop apps now use **Commit and share** for projects with a remote, preserving a local-only
commit action otherwise. The operation commits the selected files, then sends to origin (or the first
configured connection when origin is absent). A failed commit never sends; a failed/cancelled send
keeps the successful commit, clears its draft and offers a send-only retry. Project-changing actions
stay locked during submission. Windows learns the connections before exposing the first commit action;
cancelling a send also releases the awaiting workflow even if the child process never reports exit.

Validation: 33 macOS tests passed, including real temporary repositories/hosts for selective publication,
host-offline failure, retry without another commit, and a remote named other than origin. All 125 Windows
frontend/client tests passed, including failure and cancellation workflows. The Mac app was packaged and
installed as build `202610201801`; live UI shows the Chinese combined action. The Windows artifact remains a cross-build; it was subsequently installed and launched on native Windows
by the coordinated Windows task, which verified the successful commit-and-share and pull path.

The Windows NSIS artifact was rebuilt and its archive/payloads verified after this change;
`VERIFICATION.json` and `SHA256SUMS` accompany the updated installer.


## First GitHub source release — v0.1.0

The source release includes Chinese and English READMEs, macOS Apple Silicon DMG/ZIP downloads,
an unsigned Windows x64 EXE installer, and SHA256 checksums. Build caches, local repository data,
private recovery backups and previous installers are excluded from Git. The desktop downloads
are the artifacts used in the macOS and Windows smoke checks described above.
