# Storage benchmarks

Raw results: `benchmarks/results/v2/*.json` (one file per workload; `-depth16` files are the
delta-depth ablation). Reproduce with:

```bash
cargo build --release
./target/release/bench --tmp /path/with/free/space all --seed 1 --scale 1 \
    --rgit target/release/rgit --out benchmarks/results/mine
# ablation (plan default is depth 4):
./target/release/bench --tmp ... --rgit-delta-depth 16 run --workload snapshots \
    --seed 1 --scale 1 --rgit target/release/rgit --out benchmarks/results/mine
# real history instead of synthetic data:
./target/release/bench replay --repo /path/to/git/repo --commits 100 --rgit target/release/rgit
```

## Method

* Both tools replay identical file contents, commits, branches (a topic branch and a merge every
  tenth step; disjoint files so merges never conflict) and renames. Data comes from a seeded
  generator (`benchmarks/src/workloads.rs`); same seed and scale give the same bytes.
* Size = every file under `.git` / `.rgit` (packs, indexes, SQLite files, Git's sample hooks and
  index, rgit's stat cache), reported as apparent bytes and allocated blocks.
* Maintenance variants run on a copy of the finished repository. Git: `gc_default` =
  `reflog expire` + `gc --prune=now`; `gc_aggressive` = `gc --aggressive --prune=now` (window 250,
  depth 50). rgit: `pre` (no maintenance) and `gc --aggressive` (zstd 19, solid groups,
  skip-delta). Headline ratio is rgit aggressive / Git aggressive.
* Git is run with `gc.auto=0`, `gc.autoDetach=false`, `maintenance.auto=false`. (An earlier run
  without the maintenance setting was corrupted by Git's background maintenance; those results
  were discarded.)
* Hardware/OS: Apple Silicon (arm64), macOS 27 (Darwin 27.0.0), Rust 1.99.1, Git 2.54.0 (Apple).
  Exact CPU and memory are in each JSON `environment` block. One run per workload, one seed:
  no variance estimate yet, so differences of a few percent are not significant.

## Results (seed 1, scale 1, apparent MiB; delta depth 4 unless noted)

| Workload | Git `gc --aggressive` | Git default `gc` | rgit unmaintained | rgit `gc --aggressive` | rgit / Git aggressive | rgit / Git default |
|---|---:|---:|---:|---:|---:|---:|
| source (300 files, 60 commits) | 1.10 | 1.10 | 1.27 | 0.94 | **0.86** | 0.86 |
| bigfile (6 x 16 MiB records, localized edits) | 29.77 | 39.43 | 31.82 | 27.54 | **0.93** | 0.70 |
| bigfile, depth 16 | 29.77 | 39.44 | 31.81 | 26.61 | 0.89 | 0.68 |
| mixed (source + assets + big logs) | 11.19 | 12.53 | 12.21 | 11.53 | 1.03 | 0.92 |
| compressed (24 x 1 MiB random) | 88.10 | 88.10 | 88.10 | 88.08 | 1.00 | 1.00 |
| tiny (20,000 files of 50-500 B) | 8.69 | 9.11 | 15.95 | 9.12 | 1.05 | 1.00 |
| snapshots (full copies of a 6 MiB file with 1.5% of pages changed) | 2.81 | 5.01 | 7.56 | 4.60 | **1.64** | 0.92 |
| snapshots, depth 16 | 2.81 | 5.01 | 3.41 | 3.14 | 1.12 | 0.63 |

Allocated-block ratios are within a few percent of the apparent ones except `tiny` (1.19) and
`snapshots` (1.66), where rgit's many small SQLite pages round up more.

## Against the plan's targets

| Target | Result |
|---|---|
| Repetitive large-file history <= 70% of optimized Git | **Not met.** 0.93 (bigfile) and 1.64 (snapshots) of `gc --aggressive`. The ratio to default `gc` is 0.70 on bigfile, which is not the baseline the plan names. |
| Source-only history <= 110% of baseline | **Met** on the synthetic source workload (0.86). Not yet tried on real repositories. |
| 100 unchanged branches add references and metadata only | **Met.** Object count identical before and after in every workload; repository grew by ~9 KB (the `refs` rows and SQLite page rounding), versus ~19 KB for Git's packed refs. |

## What the numbers say

* The `bigfile` base content is high-entropy-ish text that zstd and zlib compress to about the same
  size, and the edits cost about 1 MiB either way. Reaching 70% on this data is not possible with
  general-purpose compression (xz -9 on a single file is already 74% of the gzip size). The
  workload is therefore not evidence for or against the design; a workload with real redundancy
  that Git handles poorly (files above Git's 512 MiB `core.bigFileThreshold`, which Git does not
  delta; many near-identical files under different names) has not been built.
* `snapshots` shows a real weakness. Git deltas each new file against a similar file with chains up
  to depth 50. With the plan's depth limit of 4, deltas must span several edits and grow, so rgit is
  64% larger; at depth 16 the gap is 12%. Depth is a read-speed vs. size trade-off the plan fixed at
  4; it is configurable here (`delta.max_depth`, readers accept up to 64).
* `tiny` and `mixed` are near parity. In `tiny` the object data is 43% smaller than Git's but the
  SQLite index (~70 bytes per object versus ~28 for Git's idx) and the stat cache give it back.
  A compact sorted per-pack index would close that; not built.
* rgit's maintenance used less temporary space (peak 60 MiB vs 585 MiB on bigfile) and less wall
  time in every workload (for example 16 s vs 113 s on bigfile). Commit-time CPU was 0.36-0.60x
  Git's except `snapshots` (1.2x). Peak RSS during commits is higher (about 11-60 MiB vs 6-21 MiB).
* Similar-file hints for new paths use the neighbouring file in the same directory with the same
  extension. This is a heuristic; it found the previous snapshot here but will miss renamed-and-edited
  files.

## Not measured

Transfer bytes (no sync protocol yet), checkout/restore of a clean clone, behavior at hundreds of
thousands of files, Windows, and variance across seeds.

## Encrypted channel (plan step 9)

Cost of the Noise XX channel versus plaintext TCP, both with the real host and client code over
loopback. Reproduce: `cargo run --release -p rustgit-host --example auth_overhead [-- --json]`
(raw results of one run: `benchmarks/results/auth/auth_overhead.json`). Same machine as above
(Apple silicon, macOS 27, Rust 1.99.1); **the machine was heavily loaded by other work during these
runs (load average ~18) so absolute numbers and ratios vary a lot between runs: two complete runs
are shown, neither is a clean measurement.**

| Metric | plaintext | encrypted (run 1 / run 2) |
|---|---:|---:|
| TCP connect + one tiny round trip, median of 300 | 0.09 / 0.10 ms | 0.51 / 1.50 ms (the handshake adds 0.4-1.4 ms: three messages, two X25519 DH pairs, signatures) |
| Raw throughput, 4 MiB frames, incompressible, request/ack | 3.5 / 1.3 GiB/s | 1.18 / 0.46 GiB/s (a quieter single run measured 4.7 vs 1.15 GiB/s) |
| Wire bytes per payload byte | 1.0000 | 1.0003 (18 bytes per 64 KiB record, plus ~0.5 KiB handshake) |
| Push of 64 MiB incompressible (full pipeline), median of 5 | 0.79 / 1.21 s | 0.77 / 1.05 s |
| Fetch of 64 MiB incompressible, median of 5 | 0.43 / 0.62 s | 0.46 / 0.66 s |
| Push / fetch of 64 MiB highly compressible (5.9 KB on the wire) | 1.14 / 0.30 s, 0.64 / 0.21 s | 0.87 / 0.27 s, 0.77 / 0.23 s |

What this supports: the channel costs ~0.5-1.5 ms per connection, a fixed 0.03 % of bandwidth, and its
single-core record throughput (about 0.5-1.2 GiB/s, `ring` ChaCha20-Poly1305 behind `snow`, plus two
buffer copies) is far above LAN, Wi-Fi and most WAN links. Through the full push/fetch pipeline (hashing,
zstd, staging, SQLite) no consistent slowdown is visible against run-to-run noise of +/-20 % or more.
What it does not support: any claim about a quiet machine, other CPUs, many concurrent connections, or
real networks (loopback hides latency, so the extra 1.5 handshake round trips are not measured).
