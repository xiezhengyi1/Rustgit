# Decisions, scope and acceptance criteria

This file records decisions that shape the implementation. It is updated whenever
a decision changes. Status of each requirement lives in
[`status.md`](status.md).

## Scope of this implementation phase

Delivered and tested: the Rust engine (`crates/store`, `crates/core`), the headless
CLI (`crates/cli`, binary `rgit`), the benchmark harness (`benchmarks/`), the sync protocol
(`crates/protocol`) and the per-project hosting process (`crates/host`) over plain TCP.

Not started: SSH transport, invitations and device
authorization (the host's `Authorizer` hook exists, the only implementation allows
everyone), mDNS discovery, desktop applications, packaging. See `status.md` for the exact
list. Nothing in those areas is mocked or stubbed in the code base; the crates simply do
not exist yet. The TCP transport is a real transport but is unencrypted and
unauthenticated, so `rgit serve` binds loopback unless overridden.

## Decisions

### D1. Custom repository format, not Git-compatible
Logical model `branch ref -> commit -> tree -> (manifest) -> chunks`; format in
[`format.md`](format.md). Git import/export is a later extension.

### D2. BLAKE3 with per-type derive-key contexts
Fast, tree-hashable, and domain separation is built into `derive_key`. IDs are 32
bytes. Final choice recorded in `format.md`; the test vectors in
`crates/core/tests/vectors.rs` pin it.

### D3. Chunk IDs are logical; physical form is separate
A chunk may be raw, zstd, a zstd-dictionary delta against another object (depth
<= 4, >= 10% saving), or a member of a solid group of small objects. Re-encoding
never changes any ID. Wire transfer (later) is by logical object.

### D4. Solid groups for small objects
Measured: per-object compression of small files leaves most of the redundancy on
the table (many tiny source files share vocabulary). During `gc`, objects up to 32 KiB
are ordered so versions of the same thing are adjacent and compressed together in
~128 KiB groups. A group is decoded as a unit and cached; reads stay O(group).

### D5. Delta by zstd raw-content dictionary
A delta is the target compressed with the base's bytes as a zstd dictionary. This
is simpler and more robust than a hand-written copy/insert delta, and the decoder
is the standard zstd decoder. If the best base (the hint) is too deep in its chain,
older bases in the chain are tried so a long edit history does not force a full copy
every fifth version.

### D6. SQLite for metadata and rebuildable index; packs are the source of truth
`index.sqlite` can be deleted and rebuilt from packs (`rgit reindex`, automatic on
open). `meta.sqlite` (refs, config, merge state) is not rebuildable. `local.sqlite`
is a stat cache. Durability order: pack fsync, index commit, ref update.

### D7. One working directory; remote pushes never touch it
Hosting uses a separate bare repository per hosted project (`rgit serve --init DIR`: a
repository with no working files and a `HOSTED` marker). `rgit serve` refuses to serve a
directory that holds a working project. The person running a host works in their own clone;
a teammate's push moves only refs in the hosted repository, and the host user's clone sees
it only through `fetch`, with `merge` refusing to overwrite uncommitted edits.

### D8. Refs are compare-and-swap rows with a generation
Deleting a branch writes a tombstone (null commit, higher generation), so replication
can propagate deletions.

### D9. Conservative working-tree safety
Checkout, switch, restore and merge refuse (changing nothing) when they would overwrite
uncommitted edits, untracked files, or ignored files. `restore --force` first keeps a
recovery snapshot under `recovery/N`; `shelve` saves everything under `shelf/N`.
All of these refs are garbage-collection roots.

### D10. Portable paths are enforced at commit and at checkout
Case/Unicode-form collisions, Windows reserved names, trailing dot/space, forbidden
characters, paths over 240 bytes, and unsafe symlinks are rejected. A tree from a peer
that cannot be represented is refused before anything is written.

### D11. Conflicts are recorded, never auto-resolved
Text merges that diffy can merge cleanly are applied. Everything else (overlapping text
edits, binary files, delete/modify, type changes) is stored as a conflict with the base,
ours and theirs versions retained; the user resolves with `resolve --ours|--theirs` or by
editing. A binary conflict keeps "ours" at the path and writes theirs next to it as
`<path>.rgit-theirs`.

### D12. Deltas and groups depend on IDs, not offsets
A delta names its base by object ID and a group is self-contained, so repacking can
rearrange physical layout without invalidating other records. Garbage collection pins
the bases of surviving deltas.

### D13. The wire carries logical objects; deltas on the wire are an encoding
Messages name objects by id and type and carry logical bytes, optionally as a zstd delta
against a similar object the receiver is guaranteed to hold. The receiver always rebuilds the
logical bytes and checks the hash, so pack layout, compression level and delta chains on
either side are invisible to the protocol. The delta is a measured necessity: without it an
incremental push of ten source commits was 6x Git's bytes; with it, 1.09x
([`benchmarks.md`](benchmarks.md)). Deltas can always be turned off (fallback to whole
objects) without affecting correctness.

### D14. Ref updates: CAS on (head, generation), fast-forward enforced separately
`UpdateRef` carries the head **and** the generation the client saw. The host compares both
inside the ref's SQLite transaction; independently it requires the old head to be an
ancestor of the new one unless force is requested *and* enabled on the host (off by default,
as is branch deletion). A matching expected head therefore never permits replacing history,
and a deleted-and-recreated branch cannot be confused with the old one (tombstones keep
generations).

### D15. Durability order for a push; acknowledgement last
stage (fsync + rename + directory fsync) -> verify hashes and dependency closure -> put into
packs and `flush` (pack fsync, index commit) -> fast-forward check -> CAS the ref and the
idempotency record in one `synchronous=FULL` SQLite transaction -> acknowledge. A retried
update after any crash converges because the idempotency record commits atomically with the
ref. Objects published for a push whose ref update is then rejected stay in the store as
unreferenced objects (reused by the retry; reclaimed by `gc`).

### D16. Upload sessions are derived, not negotiated
The session id is a hash of (device, ref, new head). A restarted client finds its staged
objects without keeping state, and an upload cannot be confused with another's.

### D17. Completeness is remembered, and invalidated by gc
Hosts keep a `closed` set of commits/trees/manifests whose whole dependency closure was
verified present, so `Have` can tell a sender to skip an entire subtree and verification
need not re-walk history. `gc` can drop part of an unreachable-but-recent subtree, so every
`gc` bumps `maintenance.epoch`; a host that sees a different epoch discards the set and
re-verifies from the ref tips on start. (This was found by reasoning about pack-granular
repacking, not by a failing test; see `status.md` for what is covered.)

### D18. Threads and blocking I/O, one writer
The host uses one thread per connection with blocking sockets and timeouts, and one mutex
around the repository for every operation that touches it. That is simple and obviously
serialized; it limits concurrency (a large update blocks other writers) and is fine for the
intended scale (a few devices per project). An async runtime would add a dependency and
nothing the plan requires yet.

### D20. Iroh is the internet transport, as an optional feature, with relays we choose
**Evaluated 2026-10-20 from primary sources only** (crates.io API, docs.rs, the published crate
source of `iroh` 1.3.0 / `iroh-relay` 1.3.0 / `noq` 1.3.0, docs.iroh.computer). Names below were read
from that source, not recalled; the 1.0 release renamed much of the pre-1.0 API (`NodeId` became
`EndpointId`, `Endpoint::bind` takes a preset, `RelayMode::Custom(RelayMap)`), so older tutorials do
not match.

| Question | Finding |
|---|---|
| Versions pinned | `iroh =1.3.0`, `iroh-relay =1.3.0` (relay binary and test server), pulled in: `noq 1.3.0` (n0's QUIC implementation with multipath, not `quinn`), `iroh-base 1.3.0`. Cargo.lock pins the rest |
| Maturity | `1.0.0-rc.0` 2026-05-07, `1.0.0` 2026-06-15, then 1.0.1 (06-29), 1.0.2, 1.0.3 (07-20), 1.1.0 (08-25), 1.2.0 (09-09), 1.3.0 (09-28); about 3.4 M downloads. Young as a 1.x: seven releases in 15 weeks, `unstable-*` feature gates still exist (custom transports, net-report), `Builder::ca_roots_config` was deprecated within 1.0. We saw no crash or protocol error in the test runs here (several dozen transfers); two API surprises (below) |
| Licence | `MIT OR Apache-2.0` for all of the above (same as this repository) |
| MSRV | Rust 1.91 (this workspace says 1.89); the transport crate and the CLI's `iroh` feature therefore need 1.91+. Everything else still builds on 1.89 |
| Dependency weight | the `iroh` feature adds 165 crates to `rgit` (135 -> 300 in `cargo tree -e normal`), among them rustls+ring, tokio, hyper, reqwest, noq, netwatch |
| Build time (Apple M3, 8 cores, cold, `--release`, thin LTO, separate target dirs) | `rgit` without Iroh 46 s, with Iroh 99 s (+53 s); incremental rebuild of the CLI with the feature 22 s |
| Binary size | `rgit` release 8.1 MB without, 17.9 MB with Iroh (+9.8 MB, +121%); stripped (`strip -x`) 7.3 MB vs 15.4 MB |
| Does the shipped binary include it? | **Yes, since the user's decision of 2026-10-20**: `scripts/package-cli.sh`, `scripts/macos/build-app.sh`, `scripts/windows/build.ps1` and `.github/workflows/windows.yml` build `rgit` with `--features iroh` (cost above). A plain `cargo build` still omits it, and a build without the feature answers every `iroh://` operation with a message saying so |

**What Iroh gives us.** Identity by Ed25519 public key (the endpoint id) that survives IP changes; TLS 1.3 with
raw public keys (RFC 7250) inside QUIC, verified against the dialed id (`iroh/src/tls/verifier.rs`), so a
relay only forwards ciphertext; hole punching and relay fallback with live path migration; a relay server
(`iroh-relay`, 1.3.0, MIT/Apache) that is self-hostable with Let's Encrypt, access lists, shared tokens and rate
limits. API used: `Endpoint::builder(presets::Minimal)` with `.alpns`, `.relay_mode(RelayMode::Custom(RelayMap))`,
`.secret_key`, `.ca_tls_config(CaTlsConfig::custom_roots)`, `.clear_ip_transports()`, `.bind_addr`, `Endpoint::connect(EndpointAddr,
ALPN)`, `accept()`, `Connection::open_bi/accept_bi`, `Connection::paths()` with `Path::is_selected/is_relay/is_ip`,
`paths_stream()`, `Endpoint::network_change()`, `online()`, `bound_sockets()`, `addr()`, `RelayMap::with_auth_token`.

**Decisions.**

* *Separate crate, optional feature.* `crates/transport-iroh` implements the existing `Duplex`/`Transport`/`Listener`
  traits; `crates/cli/src/iroh.rs` plus four one-line hooks (`net.rs`, `out.rs`, `main.rs`, `sync.rs`) wire it in.
  `crates/protocol` and `crates/host` are untouched: device authentication (Noise, pinned host key, invitations)
  runs unchanged on top of it, so Iroh adds encryption to the relay path and NAT traversal but is not what
  authorizes anyone.
* *One QUIC bidirectional stream per protocol connection.* The blocking protocol is bridged to async Iroh with a
  small private tokio runtime per transport/listener (`block_on` from plain threads, timeouts via
  `tokio::time::timeout` on cancel-safe reads and writes). The client endpoint is created once and reused across
  retries, so reconnects keep their relay connection and path knowledge.
* *No default to anyone else's infrastructure.* We use the `Minimal` preset, not `N0`: no pkarr/DNS address
  lookup and no n0 relays unless the user sets `public_relay = true`. Peers are found through the relay URL (or
  addresses) in the `iroh://` URL, which the host prints and invitations carry. Consequence: with no relay
  configured only direct addresses work, and the CLI says so. Production relays are the operator's own
  (`docs/relay.md`); relay lists, a private CA (`relay_ca`) and an access token are configuration.
* *Iroh default features off.* `portmapper` (UPnP/NAT-PMP/PCP: silently asks the router to open ports) and
  `metrics` are off; `fast-apple-datapath` is off because it relies on private Apple socket calls. Cost:
  possibly fewer direct connections behind routers that would have accepted a port mapping (not measured).
  Each can be turned on in `crates/transport-iroh/Cargo.toml`.
* *Ephemeral client identity.* Clients use a throw-away endpoint key per run unless `key_file` is set, so a relay
  cannot link a person's sessions; hosts keep a persistent key (`<hosted>/.rgit/host/iroh.key`, 0600), which is what
  makes `iroh://<id>` stable across IP changes. The key is a transport identity only.
* *The UI state comes from Iroh's selected path* (`Path::is_selected`, `is_relay`), reported as `direct | relayed |
  unreachable` with `used_relay`, `used_direct` and `path_switches` for history.

**Alternatives considered** (from documentation and package metadata; none was built here).

* *libp2p* (`libp2p 0.57.1`, MIT, updated 2026-10-15): circuit relay v2 plus DCUtR hole punching are the
  equivalent pieces, with Noise/TLS and QUIC. Much larger surface (Kademlia, gossipsub, identify, multiaddrs, a
  behaviour/swarm model) than a point-to-point stream needs, relay v2 is built around limited-duration/limited-bytes
  reservations that would have to be tuned for repository sync, and its relay is a libp2p node rather than a
  standalone HTTPS service. Not chosen; would have been the fallback if Iroh had failed.
* *Plain QUIC (`quinn 0.11.12`) plus STUN*: smallest dependency set, but we would own candidate exchange, a
  signalling channel, hole punching, path migration policy and a relay that forwards encrypted packets, which is
  what Iroh is. Not chosen; remains an option for a LAN-only transport.
* *WireGuard/Tailscale-style overlay*: needs accounts or a daemon on every device; out of scope for a transport
  inside the app.

**Surprises worth knowing.** `Builder::bind_addr` only replaces the default socket of that address family (an IPv6
`[::]` socket stays), so exclusive binding needs `clear_ip_transports()` first (found because a "loopback only" test
selected a global IPv6 address). `Endpoint::network_change()` only nudges the network monitor: it does nothing when no
interface changed, so it is not a way to simulate a change (our tests simulate one differently, see `relay.md` §8).
Without a relay hint an `iroh://<id>` URL is unusable (no address lookup), by design.

**Residual risks.** Real hole punching across two NATs, real network changes, Windows/Linux and a public relay with a
real certificate are untested ([`status.md`](status.md), [`relay.md`](relay.md) §8). Iroh's relay and client protocols are
versioned by the crate version; mixed versions untested. The self-hosting page of the Iroh documentation says it is
community-maintained and may lag the code; our guide was checked against the 1.3.0 source and binary.

### D-ssh. SSH is a byte-stream transport over the system `ssh` client, never a shell
`ssh://[user@]host[:port]/path` runs `ssh -- host 'rgit serve --stdio -- /path'` through `Command` with
an explicit argument list. The URL is parsed into validated parts (strict allow-lists, no leading `-`,
no shell metacharacters in the path, jump hosts as a validated list); nothing from an invitation or URL
reaches a local shell, and the only string a shell parses is the remote command, built from validated
pieces. Host-key checking is always on (`ask`, `yes` when no terminal, `accept-new` only by an
environment opt-in; never selectable from a URL). Device authentication stays in the secure channel above
the transport, so SSH adds access control and a second encryption layer without replacing invitations.
The server half is `rgit serve --stdio` (one session, stdout = frames only, a `RGIT-STDIO/1` line first so
login-banner noise is skipped). Chosen over embedding an SSH library (russh/libssh2): it reuses the user's
config, agent, ProxyJump and keys, and keeps host-key policy in ssh. Cost: a dependency on an installed
OpenSSH client and a POSIX shell on the remote side; Windows OpenSSH untested. Details: [`ssh.md`](ssh.md).

### D-mdns. Discovery publishes a hint, nothing identifying, and is never trusted
`_rustgit._tcp` carries only protocol version, port and a 16-hex hash of the project UUID; instance and
host names are random-looking (`rgit-<hint8>-<rand4>`), never machine or project names. Results are
sanitized, capped and marked `verified:false`; joining still needs the invitation and pinned host key.
Advertising is opt-in (`--advertise`). Residual leak: the stable hint lets a LAN observer link repeated
hosting of one project. Details: [`discovery.md`](discovery.md).

### D19. Encryption and authentication: Noise XX in the protocol layer, not in each transport
Requirement: encrypted, mutually authenticated, the host pinned from the invitation, the same
for TCP, SSH and Iroh, and no hand-rolled cryptography. Chosen: **`Noise_XX_25519_ChaChaPoly_BLAKE2s`
via the `snow` crate**, applied on top of any `Duplex` (`protocol::secure`).

* *Why Noise.* Pinning a peer key is the native model: no certificates, names, clocks or
  revocation lists to parse in the trust path. The spec is short, has public test vectors, and the
  same pattern family is used by WireGuard and libp2p. XX (rather than IK) because a joining
  device does not know the host's *X25519* key (the invitation carries the host *identity*
  fingerprint, which survives key rotation through succession links), and XX lets the client
  verify the host's proof before revealing its own identity.
* *Why not TLS 1.3 (rustls) with raw public keys.* Possible, but the trust path would need custom
  verifiers or certificate plumbing for a pinned-key model, a larger dependency tree (webpki/ASN.1
  and a crypto provider), and client-certificate handling; Iroh's own QUIC/TLS channel cannot
  serve an SSH byte stream, so a transport-independent layer was needed anyway.
* *Why not rely on SSH/Iroh authentication.* Each authenticates a different thing (a unix account,
  an Iroh node id). Device enrollment, roles and revocation must mean the same on every transport,
  so they are tied to one device key checked by the host in one place.
* *What is and is not vetted.* `snow` implements the Noise spec (its repository's CI runs the Noise test vectors), but
  **its README states it has not been formally audited**; with the `ring-accelerated` feature the
  X25519 and ChaCha20-Poly1305 primitives come from `ring` (BLAKE2s from RustCrypto). No code in
  this repository implements a primitive; the handshake pattern, payload format and record framing
  are ours and have had no independent review. Documented as a limitation in `protocol.md` 12.7.
* *Cost.* Measured in [`benchmarks.md`](benchmarks.md): ~1.1 GB/s record throughput on one core
  (loopback, vs ~4 GB/s plaintext), 18 framing bytes per 64 KiB, one extra half round trip.

### D20. One seed per identity: Ed25519 identity, X25519 channel key derived from it
The identity is an Ed25519 key (signatures, fingerprint); the Noise static key is
`BLAKE3-derive-key("rustgit noise static key v1", seed)` so key storage holds a single 32-byte
secret and the two algorithms never share a raw key. A signature over (domain, role, Noise public
key) in each handshake payload binds the channel key to the identity. Using the converted
Ed25519->X25519 key directly was rejected (key reuse across algorithms); random independent keys
were rejected (two secrets to store, rotate and lose).

### D21. Invitations are bearer secrets; enrollment is the first `Hello`; the host stores a hash
A link (`projectapp://join/...`) carries a 256-bit random secret. Only a domain-separated hash is
stored on the host; redemption is one `BEGIN IMMEDIATE` transaction (check secret in constant time,
then revoked / expired / uses left, then insert the device bound to the connection's authenticated
key). Defaults: expiring (24 h), single use, `read`. Redeeming inside `Hello` (rather than a
separate pre-auth request type) means there is no unauthenticated application message on the
host at all. Unknown id and wrong secret are indistinguishable; the specific failure is revealed
only to someone who proved the secret. The same device repeating its own redemption is
idempotent (lost reply / crash), so a failed `join` can simply be rerun.

### D22. Authorization is a separate store, checked on every request
`authz.sqlite` is not part of the immutable content and is writable by the CLI while the host runs.
The server asks the authorizer before *every* request, so revoking a device or changing a role takes
effect on open connections (idle connections and `Watch` long-polls re-check every second). The
alternative (checking only at connect) would leave a revoked device with an arbitrarily long
session. Cost: one indexed SQLite read per request. Revocation is permanent for a key (a
tombstone), so re-enrolling needs a new device identity.

### D23. Private keys: platform store by default, a plain file only on explicit request
macOS Keychain / Windows Credential Manager through the `keyring` crate. If the store is
unavailable the command fails and names `--insecure-file-keys`; it never falls back silently. The
file store is `0600`, refuses group/world-readable files, and warns on every use. Platform-store
calls are time-bounded because a legacy-keychain ACL prompt (macOS asks again whenever the
*binary's signature* changes, which an unsigned dev build does on every rebuild) otherwise blocks a
headless process forever. Release builds should be signed with a stable identity so the item's
access list keeps matching.

### D24. `serve` is authenticated by default; plaintext is an explicit, loopback-only mode
`rgit serve` requires device authentication and encryption. `--insecure-open-access` restores the
old behaviour (everyone admin, plaintext) and is accepted only for loopback addresses;
`--insecure-listen-any` is now a no-op (an authenticated host may listen on any interface). The
apps and tests that started `serve` for local development pass the new flag.

### D25. Host key rotation leaves a signed trail
Rotation signs "old key vouches for new key" and the host presents the chain, so clients pinned
to the old fingerprint re-pin automatically and no one is locked out by routine rotation. A
suspected compromise must not honour the old key's word, so `--compromised` records no link,
revokes unused invitations and requires out-of-band re-pinning. The private key is staged before
the database changes and promoted afterwards, so an interrupted rotation completes on the next
start.

## Open decisions (need input or measurement)

* **Index scale.** The SQLite index costs about 70 bytes per object, versus about 28 for
  Git's `.idx`. This shows in the tiny-files benchmark. A compact per-pack sorted index
  for repacked packs is the planned fix; measure before building.
* **Per-chunk vs. solid compression for large files.** Whole-file compression beats
  256 KiB chunk compression by ~12% on text-like data but costs random access.
* **Remote administration.** Inviting, listing and revoking are local CLI commands on the host
  machine; the `admin` role currently only gates force-push and branch deletion. A protocol-level
  admin API (so an admin device can manage others) is not designed yet.
* **Transport library.** Decided for the internet path: Iroh 1.3.0, optional feature (D20). Open: whether the
  shipped apps include the feature build (+9.8 MB), and the crates' MSRV (1.91) against the workspace's 1.89.
* **Rust version.** The workspace now requires Rust 1.89 (stable `File::try_lock`, used for
  the host lock). Earlier builds needed 1.85.
* **macOS app integration.** The plan prefers a local IPC API to the Rust host. Xcode is
  not installed on the development machine, so the SwiftUI target cannot be built here.

## Acceptance criteria used for the storage requirement

From the plan: repetitive large-file history <= 70% of optimized Git; source-only history
<= 110% (guardrail); 100 unchanged branches add references only. Results and the exact
reproduction commands are in [`benchmarks.md`](benchmarks.md). A target that is not met is
reported as not met.
