# Repository Format Specification — version 1

Status: implemented by `crates/store` and `crates/core`. Test vectors live in
`crates/core/tests/vectors.rs`; changing any value there is a format break.

## 1. Logical model

```
Branch ref -> Commit -> Tree -> (File manifest) -> Chunks
```

All logical objects are immutable and content addressed. A *logical* object ID
never depends on how the bytes are physically stored (raw, compressed, delta,
which pack).

### 1.1 Object IDs

* Hash: BLAKE3, 32 bytes. Displayed as 64 lowercase hex characters.
* Domain separation: `id = BLAKE3_derive_key(context, canonical_bytes)`, where
  `context` is the ASCII string `"rustgit v1 object:" + type` and `type` is one
  of `chunk`, `manifest`, `tree`, `commit`.
* `canonical_bytes` are the exact logical bytes defined below. There is exactly
  one valid encoding of each logical value; decoders reject every other
  encoding (non-minimal varints, unsorted trees, trailing bytes, ...).

### 1.2 Encoding primitives

| Name    | Encoding |
|---------|----------|
| `u8`    | one byte |
| `varint`| unsigned LEB128, at most 10 bytes, minimal (no trailing zero groups) |
| `zigzag`| signed integer mapped `(n << 1) ^ (n >> 63)`, then `varint` |
| `bytes` | `varint` length, then data |
| `string`| `bytes` that are valid UTF-8 |
| `id`    | 32 raw bytes |

### 1.3 Chunk

Raw file bytes (or, for a symlink, the raw target bytes). Any length from 0 up
to the configured `chunk_max` (default 1 MiB). Whole small files are a single
chunk.

### 1.4 Manifest (`type = manifest`)

```
u8      format version = 1
varint  total_len            (bytes of the file)
varint  chunk_count          (>= 2, <= 1,048,576)
id[chunk_count]  chunk ids in file order
```

A manifest is used only for files that were split by content-defined chunking.
A file stored as one chunk is referenced directly from its tree entry with
kind `file`, so tiny files cost one object, not two. Reassembly verifies that
the sum of chunk lengths equals `total_len`.

### 1.5 Tree (`type = tree`)

```
u8      format version = 1
varint  entry_count          (<= 1,000,000)
entry*  strictly ascending by the byte order of `name`
```

Entry:

```
string  name        1..=255 bytes, no '/', no NUL, not "." or "..",
                    must not be a reserved name at the project root (".rgit")
u8      kind        1 = file (id is a chunk)
                    2 = chunked file (id is a manifest)
                    3 = directory (id is a tree)
                    4 = symlink (id is a chunk holding the target bytes)
u8      flags       bit 0 = executable (kinds 1 and 2 only); other bits must be 0
id      object id
```

Empty directories are not representable. Names are preserved as given
(no Unicode normalization); portability checks happen at checkout (§5).
Maximum tree nesting depth when walking is 256.

### 1.6 Commit (`type = commit`)

```
u8      format version = 1
varint  parent_count        (0..=8)
id[parent_count]            ordered; first parent is the line the author was on
id      root tree
string  author_name         (<= 1024 bytes)
string  author_email        (<= 1024 bytes)
zigzag  timestamp           seconds since Unix epoch
zigzag  tz_offset_minutes   -1440..=1440
string  message             (<= 1 MiB)
```

Duplicate parents are rejected. A merge commit has two or more parents.

## 2. Chunking

* Files with length `<= small_file_max` (default 1 MiB) are stored whole.
* Larger files are split with a FastCDC-style gear-hash chunker, "gear-v1":
  * gear table: 256 `u64` words taken from the BLAKE3 XOF of the ASCII seed
    `"rustgit gear v1"`;
  * normalized chunking with two masks (`avg_bits + 2` below the target size,
    `avg_bits - 2` above it);
  * defaults: min 64 KiB, avg 256 KiB, max 1 MiB.
* The chunking configuration is recorded in the repository `config` (§4) with a
  version number. Changing it never invalidates stored data (chunk IDs are
  content hashes); it only changes future deduplication quality.

## 3. Physical storage

### 3.1 Pack files (`objects/pack-NNNNNN.rgpk`)

```
header: "RGPK" | u8 version = 1 | 3 reserved zero bytes          (8 bytes)
record*:
  u8      type        0 = chunk, 1 = manifest, 2 = tree, 3 = commit
  u8      encoding    0 = raw, 1 = zstd, 2 = delta (zstd with raw-content
                      dictionary = base object's logical bytes)
  varint  logical_len
  varint  stored_len
  id      base_id     (only for encoding 2)
  u8[stored_len] payload
  u32 LE  crc32 of every preceding byte of the record
```

Records are append-only. A record never stores its own ID; the ID is the hash of
the reconstructed logical bytes, so a pack can be re-indexed by decoding it.
Delta records carry the base ID. Writers keep chains at depth `<= 4` by default
(`delta.max_depth`, configurable); readers accept chains up to depth 64 and reject
deeper ones as corrupt. A delta must be at least 10% smaller than the best non-delta
form (including the 32-byte base reference). Cycles are impossible by construction
(a base must already exist when a delta is written) and are re-checked by `verify`.
`encoding = 3` (group) stores several small objects as one zstd frame (types and
lengths first, then the concatenated bytes); the frame carries its decompressed size,
which readers bound at 8 MiB and 8192 members.

Compression is used only when `stored_len < logical_len`.

### 3.2 Index (`index.sqlite`, rebuildable)

`objects(id BLOB PRIMARY KEY, type, pack, offset, stored_len, encoding,
logical_len, base BLOB, depth, member)` and `packs(id, name, indexed_len)`.

The index is a cache of what the packs contain. `rgit reindex` rebuilds it by
scanning packs. A truncated or corrupt record at the end of the newest pack
(crash during append) is truncated away on open; records before it are kept.
Reindexing cannot recreate content that is not in any pack: that needs another
replica or backup.

### 3.3 Durability rules

1. Append records to the pack, `fsync` the pack.
2. Commit the index transaction (`synchronous=FULL`).
3. Only then may a ref update or network acknowledgment depend on the objects.

On macOS `fsync` is followed by `F_FULLFSYNC`; on Windows `FlushFileBuffers`.

## 4. Local and replicated metadata

| Data | File | Replicated between peers |
|------|------|--------------------------|
| Immutable objects | `objects/*.rgpk` | yes, as logical objects |
| Object index | `index.sqlite` | no (rebuildable) |
| Repository identity (UUID, format version) | `FORMAT`, `meta.sqlite` | UUID yes |
| Branch refs `heads/<name>` `(name, commit, generation)` | `meta.sqlite` | yes, through compare-and-swap ([`protocol.md`](protocol.md)) |
| Remote-tracking refs `remotes/<remote>/<branch>` | `meta.sqlite` | no; local record of what a host had at the last fetch/push; garbage-collection roots |
| Remote definitions `remote.<name>.url`, `.project`, `device.name` | `meta.sqlite` (config) | no |
| Applied-update records (idempotency keys for retried ref updates) | `meta.sqlite`, table `applied_updates` | no; hosts only |
| `maintenance.epoch` (incremented by every `gc`) | `meta.sqlite` (config) | no |
| Config (chunking, ignore, identity) | `meta.sqlite` | no |
| HEAD, merge state, shelves, recovery refs | `meta.sqlite` | no |
| Stat cache | `index.sqlite` | no |

`FORMAT` is a small JSON file: `{"format":1,"uuid":"<uuid>"}`. A repository whose
`format` is greater than the build's supported version is refused, never opened
read-write.

The repository directory is `<project>/.rgit/` for a working repository. A hosted
repository has the same layout in a directory with no working files, plus `.rgit/HOSTED`
and `.rgit/host/` (lock, `host.sqlite`, staging); see [`protocol.md`](protocol.md) §10.

## 5. Portable checkout rules

Before modifying the working directory, a checkout plans every write and rejects
the whole operation if any path is unrepresentable on the current platform:

* two paths in one directory that differ only by case (always rejected so that
  trees are portable between macOS/Windows and Linux);
* Windows reserved names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`, with
  or without extension), names ending in `.` or space, and characters
  `<>:"\|?*` and control bytes;
* paths longer than 240 bytes in total;
* symlinks are created only on Unix; a symlink target is stored verbatim but is
  refused at checkout if absolute, if it contains `..` components, or if the
  link's directory ancestors include another symlink. Checkout never writes
  through a symlink.

File bytes are preserved exactly. No newline conversion is performed.

## 6. Compatibility

* `format` increments for any change to object encoding, hashing, or pack
  layout. Readers must refuse unknown versions.
* New *optional* behavior (new compression policies, new chunk configs) does not
  change `format`.
* Object IDs and pack records are permanent: a repository created by version N
  is readable by all later versions.

## 7. Recovery procedure

1. `rgit verify` re-hashes every reachable object and checks structure and delta
   chains. Exit code 0 means everything reachable reconstructs byte-exactly.
2. If only the index is damaged (`verify` reports index errors, objects are fine)
   run `rgit reindex`.
3. If objects are missing or corrupt, `verify` lists them; restore from another
   replica (`rgit fetch`) or a backup. Reindex never invents data.
