# Synchronization Protocol — version 1

Status: implemented by `crates/protocol` (messages, framing, secure channel, client state
machine, plan computation) and `crates/host` (server side, device authorization). By default
every connection runs over an **encrypted, mutually authenticated channel** (§12) that is
applied on top of any transport; plaintext, unauthenticated serving exists only behind the
explicit `serve --insecure-open-access` on loopback. What is tested and what is not is listed
in [`status.md`](status.md).

The protocol moves *logical objects* (chunks, manifests, trees, commits — see
[`format.md`](format.md) §1) and moves branch heads with compare-and-swap. Nothing on the
wire depends on either side's pack layout, compression level, delta chains or index.

## 1. Layering

```
rgit CLI / apps         clone, fetch, push, serve
protocol::client        state machine for one connection (push, fetch, watch, clone)
protocol::msg + frame   messages, canonical encoding, framing
protocol::secure        Noise XX handshake + encrypted records, device identity proofs (§12)
protocol::transport     Duplex / Transport / Listener traits   <- TCP, SSH (docs/ssh.md), Iroh (docs/relay.md)
host::server            one thread per connection, strictly request/response, per-request authz
host::host              serialized writes, staging, verification, publication, CAS
core / store            objects, refs (CAS + generation), packs
```

A transport only has to provide an ordered reliable byte stream with read/write timeouts
(`Duplex`). Application semantics, encryption and device authentication are shared by every
transport: the secure channel (§12) wraps whatever `Duplex` a transport produces, so an adapter
moves bytes and nothing else, and addresses (and discovery results) are only hints about where
to connect: who the host *is* is decided by the pinned identity fingerprint. The SSH adapter
([`ssh.md`](ssh.md)) carries the same frames over the stdin/stdout of the system `ssh` client and
`rgit serve --stdio`, and local discovery ([`discovery.md`](discovery.md)) only finds addresses to try.

## 2. Framing

```
u32 BE  payload length
u32 BE  CRC-32 of the payload
payload = u8 tag | fields
```

* The length is compared with the negotiated maximum before anything is allocated. The
  limit is 64 KiB for the `Hello` exchange and then the host's `max_frame` (default 16 MiB;
  hard cap 64 MiB).
* A bad CRC, an oversized length, an unknown tag, a truncated or trailing-garbage message
  all end the connection (the host sends a `BadRequest` error first when it can).
  The CRC is an integrity check against transport bugs, not authentication; object ids are
  what make content trustworthy.
* Fields use the repository format's primitives (minimal LEB128 `varint`, `bytes`, `string`,
  32-byte `id`). Decoders reject every non-canonical encoding.

The conversation is strictly request/response: the client sends one request and reads one
response. The host never sends unsolicited frames; "notifications" are a long-poll (`Watch`).

## 3. Version negotiation

`Hello { min_version, max_version, device, project?, credential }` (sent inside the secure
channel). The host picks the
highest version both support and answers `HelloOk { version, repo_uuid, limits, policy,
ref_seq, role? }` (`role` is the role the host granted this connection; absent from hosts that
predate roles). With no overlap it answers `Error VersionMismatch` carrying `Versions {min,max}`
and closes. This document is version 1; a client must not send anything else before `Hello`.

`project` is the repository UUID the client expects. The CLI pins it per remote on first
contact (`remote.<name>.project`), so a remote later pointed at a different project fails with
`ProjectMismatch` instead of mixing histories. `device` is a self-declared name used for
display and for deriving upload session ids; it is not an identity. On the secure channel the
host authorizes the *authenticated device key*, never this name. `credential` is empty for an
enrolled device, or an enrollment presentation (invitation id + secret, §12.5); it is secret,
never logged, and redacted in every `Debug` output.

`HelloOk.limits` (defaults): `max_frame` 16 MiB, `max_batch_objects` 1024, `max_batch_bytes`
4 MiB, `max_object` 8 MiB (logical size of one object), `max_ids_per_request` 8192.
`HelloOk.policy`: `allow_force` (default false), `allow_delete` (default false),
`default_branch`.

## 4. Messages

| Tag | Request | Response |
|---:|---|---|
| 1 | `Hello` | `HelloOk` (101) |
| 2 | `ListRefs{prefix}` | `Refs{records, seq}` (102): name, commit or tombstone, generation |
| 3 | `Have{session?, ids}` | `HaveResult` (103): per id `Missing` / `Complete` / `Staged` |
| 4 | `Begin{session, ref, expected, expected_gen, new_head, force, count, bytes}` | `BeginOk{staged_objects, staged_bytes}` (104) |
| 5 | `Put{session, objects}` | `PutAck{accepted, already_staged, ...}` (105) |
| 6 | `UpdateRef{session, ref, expected, expected_gen, new?, force}` | `RefUpdated{generation, already_applied}` (106) |
| 7 | `Cancel{session, discard}` | `Cancelled` (107) |
| 8 | `Plan{wants, haves}` | `PlanOk{objects, commits}` (108) |
| 9 | `PlanPage{cursor, max}` | `PlanPage{ids, end}` (109) |
| 10 | `Get{ids, allow_delta}` | `Objects{objects, remaining}` (110) |
| 11 | `Watch{since, timeout_ms}` | `WatchResult{seq, changed}` (111) |
| 12 | `Bye` | (connection closes) |
| any | | `Error` (112) |

`Have` states: **Complete** = the object is stored and so is everything it depends on (for a
commit that includes all ancestors, trees, manifests and chunks); the sender may skip it and
everything below it. **Staged** = durably staged in this upload session; do not resend, but its
dependencies are not implied. **Missing** = send it.

### Object batches

`Put` and `Objects` carry batches of objects:

```
count | mode (0 raw, 1 zstd) | raw_len | bytes
  entry: u8 type | id | u8 encoding | [base id] | bytes
```

* The whole batch is zstd-compressed when that shrinks it (level 3). This is a property of the
  message, not of storage.
* `encoding` 0: the bytes are the object's logical bytes. `encoding` 1: the bytes are the
  object compressed with the logical bytes of object `base` as a zstd raw-content
  dictionary (the same construction as the store's delta records, but chosen independently
  for the wire). The receiver rebuilds the logical bytes and checks them against the id, so a
  delta can never change what gets stored. A sender only uses a base that the receiver is
  guaranteed to hold when it processes the object (§6.2); a receiver that lacks the base
  answers `UnknownBase` and the sender resends the batch whole.
* Decompression is bounded (`max_object` per object, 64 MiB per batch) before any allocation.

### Errors

`Error{code, message, retryable, detail}`. Codes are stable numbers; unknown codes degrade to
`Internal`. `detail` is machine-readable: object id lists (`MissingObjects`, `CorruptObject`,
`InvalidObject`, `UnknownBase`), the current ref (`StaleRef`, `NonFastForward`), a limit
(`QuotaExceeded`), or supported versions (`VersionMismatch`).

| Code | Meaning | Client action |
|---|---|---|
| `VersionMismatch`, `ProjectMismatch`, `Unauthorized` | handshake refused | stop |
| `SecurityRequired` | plaintext peer on a secure host (sent as one plaintext frame, then close) | pin the host (`--host-key`) or join with an invitation |
| `UnknownDevice` | authenticated device is not enrolled and presented no invitation | ask the host owner for an invitation |
| `DeviceRevoked` | the device's authorization was revoked (permanent for that key); ends open sessions | stop |
| `Forbidden` | enrolled, but the role does not allow the operation (§12.4) | stop |
| `InvitationInvalid` | unknown invitation **or** wrong secret (indistinguishable on purpose) | stop |
| `InvitationExpired`, `InvitationUsed`, `InvitationRevoked` | reported only to a caller that proved the secret | stop; ask for a new one |
| `HostKeyMismatch`, `HandshakeFailed` | detected by the **client**: the host is not the pinned one / the channel failed | stop; never auto-retry |
| `StaleRef` | ref is not at the expected head **and generation** | fetch, merge, push again |
| `NonFastForward` | head matches but history would be replaced | fetch, merge, push again |
| `ForcePushDisabled`, `DeleteDisabled` | policy | stop |
| `MissingObjects` | closure of the new head is incomplete; ids listed | resend those, ask again |
| `CorruptObject` | hash mismatch (in transit, or a staged file damaged on the host) | resend |
| `InvalidObject` | object does not decode / wrong type | stop |
| `ObjectTooLarge`, `QuotaExceeded`, `OutOfSpace` | limits | stop |
| `Busy`, `Timeout`, `ShuttingDown` | `retryable` | retry with backoff |
| `UnknownBase` | delta base missing at receiver | resend whole |

## 5. Sessions and resume

A push uses an *upload session*. Its id is
`BLAKE3-derive-key("rustgit protocol v1 upload session", device ‖ ref ‖ new_head)[..16]`, so a
client that crashed and restarts computes the same id and finds the same staging area; no
client-side state is needed. A session is owned by one connection at a time (`Busy`
otherwise; the owner releases it when its connection ends). Staged objects survive host
restarts and are deleted when the push is published, when `Cancel{discard:true}` is sent, or
after 24 h without activity.

Resume therefore is: reconnect, `Have{session, ids}` reports the staged ones, send only the
`Missing` rest. Interrupted fetches resume by the receiver's own durable store (§7).

## 6. Push

### 6.1 Client

1. `Hello`, `ListRefs("heads/")`. The record for the target branch gives `(expected head,
   expected generation)`; a deleted branch is a tombstone with its generation.
2. If the host's head equals the local head: up to date. If the host's head is not an
   ancestor of the local head (or unknown locally): stop with `NonFastForward` and the advice
   to fetch and merge. Nothing is uploaded and nothing local changes. (The host repeats this
   check; it does not trust the client.)
3. Compute the objects to offer: commits reachable from the head but not from host ref tips we
   already hold; for each, the trees/manifests/chunks that differ from its first parent
   (`plan::enumerate_hinted`, oldest commit first).
4. `Have{session, ids}` in bounded batches; keep only `Missing`.
5. `Begin` (quota and stale-ref pre-checks happen here, before any byte is uploaded).
6. `Put` batches (≤ 1024 objects and ≤ 4 MiB logical each), one at a time: the next batch is
   not sent before the previous is acknowledged. That is the backpressure: a slow host slows
   the client; memory on both sides is bounded by one batch. The cancel token is checked
   between batches.
7. `UpdateRef`. On `MissingObjects`/`CorruptObject` with ids, resend those objects and repeat
   (at most 3 times). On success, move `remotes/<remote>/<branch>` locally.

Whole operations are retried on retryable errors (connection reset, timeout, `Busy`) with
exponential backoff, reconnecting each time; this is safe because of §6.3 and §5. A refused
connection is not retried.

### 6.2 Delta hints

For each offered object the planner records a *hint*: the object at the same path in the
first parent (a commit's hint is its first parent; a chunk's is the previous version of the
file, or the chunk at the same index of the parent's manifest). Because commits are processed
oldest first and the parent's closure is either complete on the receiver or itself in the
offered list (sent earlier in the same stable type order), the hint is always available when
the delta is decoded. This is an optimization with a fallback, never a requirement.

### 6.3 Host: `Begin`, `Put`, `UpdateRef`

* `Begin` checks authorization, that the ref is `heads/<valid name>`, force policy, that the
  ref is still at the expected head/generation, and quotas (declared bytes vs. session,
  staging-total and repository quotas), then claims the session.
* `Put` verifies the **whole batch before keeping any of it**: every object is rebuilt to its
  logical bytes, hashed with its type's domain separation and compared with its claimed id
  (`CorruptObject` lists the ids), non-chunk objects must decode strictly (`InvalidObject`),
  sizes and quotas are checked. Objects are then written to the staging area (§6.4) and the
  acknowledgement is sent only after they are durable.
* `UpdateRef` runs entirely inside the host's single writer lock:

  1. authorization; ref name; force/delete policy;
  2. **idempotency**: if the same update (same session, ref, expected head/generation, new
     head) was already applied, answer `RefUpdated{already_applied:true}` with the original
     generation and do nothing else;
  3. the ref must be at exactly the expected head **and** generation, else `StaleRef` with the
     current head/generation;
  4. **closure verification**: walk from the new head through staged and stored objects
     (re-hashing staged files); stop at objects recorded as complete; list anything absent
     (`MissingObjects`);
  5. **publish**: put the staged objects into the pack store in dependency order, then
     `store.flush()` (pack fsync, then index commit);
  6. **fast-forward rule** (separate from 3): the current head must be an ancestor of the new
     head unless `force` (which the host rejects unless `allow_force`). Matching expected
     head alone never permits replacing history;
  7. record the verified objects as complete;
  8. compare-and-swap the ref (head and generation) **and** write the idempotency record in one
     SQLite transaction (`synchronous=FULL`);
  9. wake `Watch`ers, delete the staging area, and only now return `RefUpdated`.

### 6.4 Staging area

`<host>/.rgit/host/staging/<session>/obj/<id>` holds `type byte | logical bytes`. Each object
is written to `tmp/<id>.part`, `fsync`ed (`sync_all`: `F_FULLFSYNC` on macOS,
`FlushFileBuffers` on Windows), renamed into `obj/`, and the directory is fsynced before the
batch is acknowledged. A file in `obj/` is therefore always a complete, hash-checked object
that the host promised to remember; `.part` files are interrupted writes and are removed when
the session is reopened. Staged data is invisible to readers of the repository until step 5
of `UpdateRef` moves it into the pack store.

### 6.5 Failure matrix

| Crash / failure point | State left behind | What a retry does |
|---|---|---|
| client dies mid-upload | staged objects (acknowledged ones) | `Have` reports them `Staged`; only the rest is sent |
| host dies mid-batch | objects renamed before the crash (possibly unacknowledged) | same; the unacknowledged batch is redone idempotently |
| host dies before publish (step 5) | staged objects, ref unchanged | nothing re-sent; verification and publish run |
| host dies between publish and ref CAS (steps 5-8) | objects durable but unreferenced, not marked complete, ref unchanged | chunks are `Complete`; trees/commits resent (small); ref update succeeds |
| host dies after the CAS, before the reply | ref moved, idempotency record committed | client sees the host's head equals its own: "up to date"; a replayed `UpdateRef` returns `already_applied` |
| staged file damaged on the host's disk | `CorruptObject` at publish; file deleted | client resends exactly those objects |
| host out of disk | `OutOfSpace`; nothing previously acknowledged is touched | stop; fix space; retry |
| ref moved by someone else | `StaleRef` (at `Begin`, or at `UpdateRef` if the race is later) | fetch, merge, push; objects already published are reused |

Objects published for an update that is then rejected at the ref (stale or non-fast-forward)
stay in the store unreferenced, so the retry after merging does not resend them;
`rgit gc` reclaims them if they are never used.

## 7. Fetch

1. `ListRefs("heads/")`; for every branch whose `remotes/<remote>/<branch>` differs, the
   head is a *want*. Branches the host deleted (tombstones) remove the tracking ref.
2. *Haves*: commits reachable from the client's `heads/`, `remotes/` and `tags/` refs (the
   refs' targets are complete by construction), tips first, at most 4096.
3. `Plan{wants, haves}`: the host checks each want is a complete commit, takes the haves it
   holds completely as the boundary, and lists the objects introduced by the commits behind
   the wants (same enumeration as §6.1 step 3). The id list stays on the host (limit
   `max_plan_objects`, default 4,000,000) and is paged out with `PlanPage`.
4. The client drops ids it already has (this is what makes an interrupted fetch cheap to
   repeat) and requests the rest with `Get`. The host answers with as many objects as fit the
   batch limits and reports `remaining`; the client asks again. Deltas are used only against
   objects the client is guaranteed to hold (§6.2) and only if the client sent
   `allow_delta`.
5. Every received object is rebuilt, hashed, checked for structure, and **only then** stored.
   The store is flushed after every batch, so an interruption loses at most one batch.
6. After all pages: the client verifies that everything the new commits introduce is present,
   and only then moves `remotes/<remote>/<branch>` (compare-and-swap). Fetch never touches
   the working directory, `heads/`, or uncommitted work. `merge origin/main` then merges with
   the usual protection for local changes.

## 8. Watching

`Watch{since, timeout_ms}` blocks on the host (up to 60 s) until the ref sequence number
exceeds `since`; the sequence is returned by `HelloOk` and `Refs` and increases on every ref
change. `rgit fetch --watch` uses it to fetch automatically. It fetches only; it never merges
or checks out.

## 9. Limits, authorization and security status

* **Timeouts.** Host: 30 s to finish a frame once started, 300 s idle between requests,
  configurable. Client: 30 s per frame, 60 s per ordinary reply, 300 s for `Plan` and
  `UpdateRef`.
* **Quotas / limits** (`HostConfig`): connections (64 total, 8 per device), open upload
  sessions per device (4), staged bytes per session (8 GiB) and in total (32 GiB), optional
  repository size cap, plan size, object size (8 MiB), batch sizes. There is no bandwidth
  throttling.
* **Authorization.** Every request is authorized against the live device records (§12.4)
  before it runs: the host looks the authenticated device key up on **each request**, so a
  revocation or role change applies to connections that are already open. `OpenAccess` (everyone
  is an admin) exists only for `rgit serve --insecure-open-access`, which accepts loopback
  addresses only.
* **Confidentiality / integrity in transit.** Encrypted and authenticated by the secure
  channel (§12) on every transport, unless the host was started with `--insecure-open-access`.
  Content integrity never depended on the transport (every object is verified by hash and
  structure); the channel additionally hides content, authenticates both ends and detects
  withheld, reordered, replayed or modified records.
* **Refs.** Only `heads/<name>` can be pushed. Tags, shelves and recovery refs are local.

## 10. Hosted repository layout

A hosted project is created with `rgit serve --init DIR` (or `Host::init`): an ordinary
repository (`DIR/.rgit/...`, no working files) plus

```
.rgit/HOSTED                marker; commands that need a working directory refuse to run
.rgit/host/LOCK             exclusive lock held by `rgit serve`; also taken by offline gc/reindex
.rgit/host/host.sqlite      `closed` set (objects whose dependencies are all present) + epoch
.rgit/host/authz.sqlite     devices, invitations (secret hashes only), host public key + succession links (§12)
.rgit/host/staging/...      upload sessions (§6.4)
```

`rgit serve` refuses to host a directory that contains a working project, and a second
`serve` on the same directory fails on the lock. Refs live in `meta.sqlite` as
`(name, commit, generation)` rows; the idempotency records are in the table `applied_updates`
of the same database so they commit atomically with the ref. The `closed` set is a cache of
verified facts and is discarded and rebuilt from the ref tips whenever `gc` has run
(`maintenance.epoch`), because repacking can drop part of an unreachable subtree.

## 11. Known limitations

* Objects larger than 8 MiB (logical) cannot be transferred; the push fails with
  `ObjectTooLarge`. Large files are chunked (≤ 1 MiB by default), so this affects only
  unusually large single trees or manifests.
* The fetch plan keeps its id list in memory (4M objects by default); larger initial clones
  need a streaming plan.
* Closure verification and the fast-forward test walk history; both are O(commits) per update
  in the worst case.
* One writer lock per host: a large `UpdateRef` blocks other writers and, briefly, readers.
* Delta hints are positional/path-based heuristics; renamed-and-edited files are sent whole.
* No compression dictionary is shared across batches, and no cross-object delta exists on the
  wire beyond the hints above.

## 12. Secure channel, device identity, invitations and authorization

Implemented in `protocol::secure`, `protocol::identity`, `protocol::invite` and in `host::auth`,
`host::authz`, `host::hostid`, `host::keystore`; CLI in `crates/cli/src/auth.rs`. Why these
choices: [`decisions.md`](decisions.md) D19-D24.

### 12.1 Identities and key custody

* A **device** (and each hosted project, for its host) has an **Ed25519** key pair. Its
  *fingerprint* is `BLAKE3-derive-key("rustgit device fingerprint v1", public key)`, shown as 64
  hex digits (`rgit device id`, `rgit host id`).
* The Noise static key (X25519) is **derived** from the same 32-byte seed
  (`BLAKE3-derive-key("rustgit noise static key v1", seed)`), so there is one secret per identity.
  The Ed25519 key signs a binding to the X25519 key during the handshake (12.2).
* The seed lives in the platform's protected store: **macOS Keychain**, **Windows Credential
  Manager** (service `rustgit`, account `device` or `host-<project uuid>`). If that store cannot
  be used the command **fails** with `key_storage_unavailable`; it never falls back by itself.
  `--insecure-file-keys` (or `RGIT_INSECURE_FILE_KEYS=1`) selects, explicitly, a `0600` file per
  key under `--key-dir` / `RGIT_KEY_DIR` (default `<config dir>/keys`); a key file readable by
  other users is refused, and a warning is printed on every run. That file is not encrypted:
  whoever can read it is the device.
* Seeds are zeroed on drop and have redacted `Debug` output; they are never logged or printed.
  Platform-store calls are time-bounded (30 s) so a locked keychain or an unseen approval dialog
  fails with an explanation instead of hanging.

### 12.2 Handshake (Noise XX)

```
client                                                         host
  magic "RGSEC\0\1\n" | u16 len | e                      ->            (message 1)
                                        <- u16 len | e, ee, s, es  + HOST PROOF     (message 2)
  verify HOST PROOF against the pinned fingerprint; abort here if it does not match
  u16 len | s, se  + DEVICE PROOF                          ->            (message 3)
  ----- transport records: u16 len | ChaCha20-Poly1305 ciphertext, per-direction counter nonce ----
```

* `Noise_XX_25519_ChaChaPoly_BLAKE2s`, prologue `"rustgit secure channel v1"`, via the `snow`
  crate. Static keys are sent encrypted; the three messages give mutual authentication and
  forward secrecy (ephemeral-ephemeral DH): recording a session and later stealing a static key
  does not decrypt it.
* A **proof** is `version | Ed25519 public key | signature over ("rustgit noise static binding
  v1" ‖ role byte ‖ Noise static public key) | succession links`. The Noise DH authenticates
  *possession of the static private key*; the signature ties that key to the Ed25519 identity.
  Both sides verify it strictly (`verify_strict`).
* **Pinning.** The client accepts the host only if the presented key's fingerprint equals the pin
  (from the invitation or `remote.<name>.host_key`) or is reachable from it through verified
  *succession links* (12.6). Otherwise it sends **nothing further**: a wrong or intercepting host
  never learns which device was connecting (`HostKeyMismatch`, tested).
* The device key is delivered in message 3 and only then does authorization (12.4) run, above
  this layer. The handshake itself never tells an unauthorized device anything about the host
  beyond what it already pinned.
* A host that receives plaintext instead of the magic sends **one plaintext** `SecurityRequired`
  error frame and drops the connection; a secure client reaching a plaintext host gets a
  closed connection (the magic reads as an oversized frame length) and reports that the host may
  be running `--insecure-open-access`.
* Transport phase: records carry up to 65519 plaintext bytes (Noise's 65535 limit minus the
  tag). Writes are buffered and sealed per `flush`, so a frame's header and payload share a
  record. Any record that fails authentication (modified, replayed, reordered, truncated
  mid-record) permanently breaks the channel; the receiver reports it and closes. Partial
  records survive read timeouts (the host polls with 1-second deadlines).
* Unauthenticated connections are capped (`max_unauthenticated`, default 32) and time out like
  any stalled frame (30 s), so half-open handshakes cannot exhaust threads.

Cost (loopback, Apple silicon; see [`benchmarks.md`](benchmarks.md) "Encrypted channel"): record
framing adds 18 bytes per 64 KiB (0.03 %), the handshake about 0.5 KiB and 1.5 round trips.

### 12.3 Roles

| Role | Allows |
|---|---|
| `read` | `ListRefs`, `Have`, `Plan`, `PlanPage`, `Get`, `Watch` (fetch, clone, watch) |
| `write` | read, plus `Begin`, `Put`, `Cancel`, `UpdateRef` (fast-forward pushes) |
| `admin` | write, plus force-push and branch deletion (when the host policy also enables them) |

Roles are per device and per project (all branches). They are enforced in the host on **every
request** (`Op::required_role`), after the request is decoded and before any work: a hand-written
client gets `Forbidden`. Remote *management* (inviting, listing, revoking over the protocol) is
not implemented; the host owner runs `rgit invite|devices|host` on the host machine, and `admin`
today gates only force/delete.

### 12.4 Authorization records and revocation

`.rgit/host/authz.sqlite` (outside the immutable object store, `synchronous=FULL`, mode 0600)
holds `devices` (public key, self-declared name, role, added, **last seen**, revoked-at,
invitation used), `invitations` (id, **hash** of the secret, role, created, expires, uses,
max uses, revoked-at, label) and the host's public key plus succession links. `rgit invite|devices|host`
write it from another process while `rgit serve` runs; the server re-reads it on every request.

* **Revoke device** (`rgit devices revoke`): from the device's next request on it is refused
  with `DeviceRevoked` and the connection is closed. Idle connections are re-checked every
  second, and a blocked `Watch` long-poll re-checks every second, so open sessions end within
  about a second; a request already being processed (for example one large `Put`) completes.
  A revoked key stays revoked: even a fresh invitation cannot re-enroll it (the device must
  create a new identity: `rgit device reset --yes`, then join again).
* **Revocation prevents future access only.** Everything the device already fetched stays with
  it; changes it already pushed stay in history. Revoke, and if content was sensitive, treat it
  as disclosed.
* **Revoke invitation** (`rgit invite revoke`): unusable from then on; devices that already
  redeemed it are unaffected (revoke them separately).
* **Rotate role** (`rgit devices role`): applies to the next request.

### 12.5 Invitations

`projectapp://join/<base64url>`, payload (canonical encoding): version 1, project uuid, the
host's 32-byte fingerprint, up to 8 connection hints (`tcp://host:port`, later `ssh://`,
`iroh://`), expiry (unix seconds), role, 16-byte invitation id, **32-byte random enrollment
secret** (OS CSPRNG). Defaults: role `read`, lifetime 24 h, **single use**. Whitespace from a
pasted link is ignored; parse errors never echo the input. A link is a *bearer credential*: whoever
redeems it first gets the role. Treat it like a password; `rgit join -` reads it from stdin so
it does not appear in the process list.

Redemption happens in the first `Hello` of an authenticated connection (`credential =
0x01 | id | secret`): the host, in one `BEGIN IMMEDIATE` SQLite transaction, checks the secret
hash in constant time, then revocation, expiry and remaining uses, increments `uses` and inserts
the device bound to the **device key of this connection**. Properties, all tested:

* Concurrent redemption of a single-use invitation enrolls exactly one device (real processes
  racing, and threads over separate connections to the database).
* Unknown invitation id and wrong secret give the same `InvitationInvalid` answer; expired,
  used and revoked are reported only after the secret was proved, so they do not reveal which
  ids exist.
* The same device retrying its own redemption (lost reply, crashed client) is idempotent; a
  different device replaying a spent invitation gets `InvitationUsed`.
* The client checks the expiry locally first and tries the hints in order; a verdict from the
  host (wrong identity, invitation errors) stops the search, an unreachable hint moves on. A
  failed attempt before the host answered consumes nothing.
* A leaked database does not leak invitations: only secret hashes are stored.

### 12.6 Host key rotation

`rgit host rotate` (host stopped) creates a new key. The old key **signs a succession link**
to the new one, the host presents the chain in its proof, and clients pinned to the old
fingerprint verify it, accept the new key and persist the new pin. `rgit host rotate
--compromised` (or a lost key) records no link, revokes every unused invitation and forces
clients to be re-pinned out of band (`rgit remote add ... --host-key NEW`) after the owner
confirms the new fingerprint; enrolled devices stay enrolled (they are identified by their own
keys). A missing or mismatching host private key is an error, never a silent new identity.

### 12.7 Threat model

Protected against: a passive network observer (content, ref names, object ids are all inside the
channel); an active attacker on the path, including one who answers at the right address with a
valid but different identity (pin mismatch, nothing revealed); a modified, replayed, reordered or
truncated record; an unenrolled, revoked or insufficiently privileged device; invitation replay,
guessing (256-bit secrets), races and expiry; leaked logs and `--json` output (no secrets are
printed, logged or stored in clear); a stolen authorization database (hashes only).

What a **relay** or intermediary can see (an Iroh relay, an SSH jump host, a Wi-Fi network, the
hosting provider's network): who connects to whom (addresses, timing), message sizes and counts,
and the handshake's ephemeral keys. It cannot read or change the traffic without detection, cannot
present the host's identity, and can only drop or delay. mDNS only exposes an opaque project hash
(`discovery.md`).

What the **host** necessarily sees: all content (it is the store), the devices' fingerprints, the
names they chose at enrollment, their IP addresses and last-seen times.

Not protected against: a compromised host machine; a compromised device (a copied key file or an
unlocked keychain *is* the device: revoke it); an invitation link that leaks before it is used
(mitigations: short life, single use, `read` by default, `devices list` shows who joined);
traffic analysis by size and timing; denial of service by a party that can reach the port
(only the connection and pre-authentication caps exist); a malicious *host owner* clock
(expiry uses the host's wall clock). The `snow` crate has not been formally audited (its
README says so); the construction is a standard Noise pattern with primitives from the RustCrypto
and dalek crates. Nothing here has had independent review.

### 12.8 Not implemented / limits

* No remote management protocol (invite/revoke from another device); roles are per project, not
  per branch.
* No Windows run: the Credential Manager path and Windows file permissions for the file store
  are untested (the file store relies on the profile directory's ACL there).
* No rate limiting of failed enrollment attempts beyond the caps (guessing is infeasible, but
  noisy peers are not throttled).
* `serve --stdio` over SSH on macOS runs in a non-GUI session where the login keychain may be
  locked; use `--insecure-file-keys` with a key directory on such hosts.
* Handshake is 1.5 round trips (no 0-RTT, no session resumption).
