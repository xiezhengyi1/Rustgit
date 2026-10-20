#!/usr/bin/env python3
"""Top allocation sites at the global heap peak from a dhat-heap.json."""
import json, sys
d = json.load(open(sys.argv[1]))
n = int(sys.argv[2]) if len(sys.argv) > 2 else 8
ft = d["ftbl"]
def short(f):
    f = f.split(": ", 1)[-1]
    return f[:110]
rows = sorted(d["pps"], key=lambda p: -p["gb"])
print(f"peak heap: {sum(p['gb'] for p in d['pps'])/1048576:.1f} MiB  (total allocated {sum(p['tb'] for p in d['pps'])/1048576:.0f} MiB)")
for p in rows[:n]:
    frames = [short(ft[i]) for i in p["fs"]]
    keep = [f for f in frames if "rustgit" in f or "rusqlite" in f or "zstd" in f][:4]
    print(f"{p['gb']/1048576:8.2f} MiB at peak | {' <- '.join(keep) if keep else ' <- '.join(frames[:4])}")
