#!/bin/sh
# Peak resident memory (MiB) of rgit for representative operations.
# Usage: scripts/mem-bench.sh path/to/rgit scratch-dir [bench-binary]
set -eu
R=$1; S=$2; B=${3:-./target/release/bench}
mkdir -p "$S"
SP=
trap '[ -z "$SP" ] || kill "$SP" 2>/dev/null || true' EXIT
peak() { # label, then command...
  label=$1; shift
  if ! /usr/bin/time -l "$@" >/dev/null 2>"$S/time.out"; then
    cat "$S/time.out" >&2
    return 1
  fi
  rss=$(grep "maximum resident" "$S/time.out" | awk '{print $1}')
  real=$(grep " real" "$S/time.out" | awk '{print $1}')
  printf "%-44s %8.1f MiB  %6.2fs\n" "$label" "$(echo "$rss/1048576" | bc -l)" "$real"
}
fresh() { rm -rf "$S/$1"; mkdir -p "$S/$1"; }

# 1. many small files
fresh tiny; $B materialize --workload tiny --scale 1.5 --dir "$S/tiny" >/dev/null
$R -C "$S/tiny" init >/dev/null
peak "status, 30k files, cold stat cache"      $R -C "$S/tiny" status
peak "commit, 30k files (first)"               $R -C "$S/tiny" commit -m first
peak "status, 30k files, warm"                 $R -C "$S/tiny" status
peak "log"                                     $R -C "$S/tiny" log -n 5
peak "gc --aggressive, 30k small objects"      $R -C "$S/tiny" gc --aggressive
peak "verify, 30k files"                       $R -C "$S/tiny" verify

# 2. large incompressible files
fresh big; mkdir -p "$S/big"; head -c 300000000 /dev/urandom > "$S/big/a.bin"; head -c 100000000 /dev/urandom > "$S/big/b.bin"
$R -C "$S/big" init >/dev/null
peak "commit, 2 files 300 MB + 100 MB"         $R -C "$S/big" commit -m first
peak "status after, warm"                      $R -C "$S/big" status
printf 'x' >> "$S/big/a.bin"
peak "commit, edit of 300 MB file (append)"    $R -C "$S/big" commit -m second
peak "gc --aggressive, 400 MB"                 $R -C "$S/big" gc --aggressive
peak "verify, 400 MB"                          $R -C "$S/big" verify
peak "switch to older commit (restore 300 MB)" $R -C "$S/big" branch old --from HEAD~1
peak "  switch old"                            $R -C "$S/big" switch old

# 3. compressible big records with local edits
fresh rec; $B materialize --workload bigfile --scale 1 --dir "$S/rec" >/dev/null
$R -C "$S/rec" init >/dev/null
peak "commit, 6 x 16 MB text files"            $R -C "$S/rec" commit -m first
peak "gc --aggressive, 96 MB text"             $R -C "$S/rec" gc --aggressive

# 4. network
fresh host; fresh clone
$R serve --init "$S/host" --insecure-open-access --listen 127.0.0.1:0 --port-file "$S/port" >/dev/null 2>&1 &
SP=$!
i=0; while [ ! -f "$S/port" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done
URL=tcp://$(cat "$S/port")
$R -C "$S/big" remote add origin "$URL" >/dev/null
peak "push, 400 MB repo"                       $R -C "$S/big" push
rm -rf "$S/clone"
peak "clone, 400 MB repo"                      $R clone "$URL" "$S/clone"
kill $SP 2>/dev/null || true
