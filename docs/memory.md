# Runtime memory

Peak resident set size (macOS `/usr/bin/time -l`, MiB) for typical operations, measured with
`scripts/mem-bench.sh path/to/rgit scratch-dir`. One run each on one machine (Apple Silicon,
macOS 27): treat differences under ~2 MiB as noise.

| Operation | Before | After |
|---|---:|---:|
| status, 30,000 files, cold stat cache | 72.5 | 24.8 |
| status, 30,000 files, warm | 30.1 | 27.1 |
| commit, 30,000 files (first) | 36.5 | 25.4 |
| gc --aggressive, 30,000 small objects | 38.5 | 28.9 |
| verify, 30,000 files | 15.3 | 16.7 (4.06 s -> 0.21 s) |
| commit, 300 MB + 100 MB incompressible files | 18.5 | 17.3 |
| commit, 300 MB file edited | 21.6 | 13.9 |
| gc --aggressive, 400 MB | 48.6 | 40.2 |
| gc --aggressive, 96 MB text | 44.5 | 34.7 |
| commit, 6 x 16 MB text files | 17.0 | 16.4 |
| push of a 400 MB repository (client) | 96.3 | 52.2 |
| clone of a 400 MB repository | 40.7 | 33.1 |

Both columns are the full `rgit` CLI (`scripts/mem-bench.sh`). Wall-clock times in the "after" run are
not comparable (the machine had a load average of about 13), only the memory figures are.

## What was wrong and what changed

Heap profiling (`cargo build --profile profiling --features dhat-heap -p rustgit-cli`, then
`scripts/dhat_top.py dhat-heap.json`) showed:

* **JSON built even for human output, and as a tree.** `status` created a `serde_json::Value`
  node per changed file (18 MiB for 30,000 files) whether or not `--json` was given. Results are
  now built only with `--json` and serialized directly to text.
* **Duplicated path strings.** The directory scan stored every path twice and used 128-bit
  timestamps. The paths are now only the map key and timestamps are 64-bit.
* **Portability check kept two strings per path component** (4.6 MiB). It now keeps hashes, and
  re-derives names only to report a real collision.
* **gc held every small object's bytes in memory** before grouping them, so its memory grew with
  repository size, not with the group size. It now queues only (id, type, length, hint) and reads the
  bytes of one group at a time.
* **zstd contexts were allocated and freed for every object** (tens of MiB at level 19); on macOS
  the allocator keeps freed large blocks, which is most of the 96 MiB seen during a push. One
  context per thread and level is reused.
* **`log -n 20` read the entire history.** A bounded log now reads about `n` commits.
* **verify built a list of every object id** and visited objects in random order, decompressing the
  same group repeatedly. It now streams in pack order (this also made it 20x faster on 30,000 files).
* The pack write buffer was 4 MiB; it is 1 MiB.

Tried and rejected: swapping the global allocator for mimalloc lowered push from 96 to 53 MiB but
raised commit, clone and gc by 10-25 MiB, so it was removed.

## Known remaining costs

* The working-tree status holds three maps of all paths (scan, last commit, result): roughly 25 MiB for
  30,000 files, linear in file count. Streaming them would need an API change.
* `gc --aggressive` at zstd level 19 needs a ~20-25 MiB compression context.
* gc still keeps ids and hints of all live objects (about 100 bytes per object).
* Protocol batches are up to 4 MiB and a batch is copied three times (encode, compress, send);
  halving `max_batch_bytes` would roughly halve push/clone transients (not done; push is now 52 MiB).

## Revalidation with the packaged-feature CLI (2026-10-20)

Re-ran `scripts/mem-bench.sh` against the full release CLI built with `--features iroh`, on the
same Mac, with other builds/tests running. These are new measurements, not a replacement for the
historical before/after comparison above:

| Operation | Peak RSS (MiB) |
|---|---:|
| status, 30,000 files, cold / warm | 25.2 / 25.9 |
| first commit, 30,000 files | 25.8 |
| aggressive gc, 30,000 small objects | 29.8 |
| verify, 30,000 files | 18.2 |
| commit, 300 MB + 100 MB incompressible data | 17.1 |
| aggressive gc, 400 MB | 41.6 |
| push / clone, 400 MB | 137.3 / 46.5 |

A separate repeat of the network-only workload measured 65.3 / 37.4 MiB for push/clone. Network
peak RSS is variable here; the earlier 52.2 MiB push result was not reproduced. A proposed reuse
of protocol compression contexts measured 108.1 / 43.0 MiB and was discarded because it did not
improve peak memory. No new protocol memory change is shipped from that experiment.
The benchmark now stops on command failure instead of printing a measurement for a failed operation,
and cleans up its own temporary host on exit. Timing under this concurrent build load is not a
smoothness benchmark. The existing app tests cover instant cancellation and 30k-file selections.
