# Local discovery (mDNS / DNS-SD)

Crate `crates/discovery` (library `mdns-sd` 0.21.6, checked against its docs.rs/source; no async runtime), CLI wiring
in `crates/cli/src/net.rs`.

```bash
rgit serve /srv/p.rgit --listen 0.0.0.0:7878 --advertise   # host: publish on the local network
rgit discover                      # client: list hosts for 3 s
rgit --json discover --timeout 5 --project <uuid>
rgit discover --watch              # keep listening, report hosts appearing and disappearing
```

**Discovery results are hints, never identities.** Anyone on the network can publish a `_rustgit._tcp`
service with any address and any text. A hit means "something that claims to host a project with this
hint is here". Joining still goes through the invitation, the pinned host fingerprint and the encrypted,
device-authenticated handshake; a discovered address is only a place to try.

## What is advertised

Service type `_rustgit._tcp.local.`

| Record | Content |
|---|---|
| instance name | `rgit-<8 hex of the project hint>-<4 random hex>`, e.g. `rgit-3f9a07b1-c2d4` |
| host name | `<instance>.local.` (never the machine's name) |
| SRV port | the port `rgit serve` is listening on |
| addresses | the address it listens on; for a wildcard bind (`0.0.0.0`) the machine's IPv4 interface addresses that the library selects (IPv6 as well for `[::]`); on the development machine the LAN address (plus 127.0.0.1 on the loopback interface, only to same-machine browsers); the proxy-tunnel address was not published. Answers are per interface, so a loopback address is only ever sent on the loopback interface (visible to a browser on the same machine, which drops it by default; other machines cannot receive it) |
| TXT `v` | highest protocol version this host speaks (`1`) |
| TXT `p` | project hint: 16 hex characters = first 8 bytes of `BLAKE3-derive-key("rustgit discovery v1 project hint", project UUID)` |

Nothing else: no project name, branch names, user names, device names or file names.

Privacy limits you should know about: the hint is stable, so a passive observer on the LAN can tell that
the same project is being hosted again (and when), though not recover the UUID. A person who already knows
a project's UUID can recognise its hint (that is how `--project` / `--remote` mark matches). Advertising is
off by default and only happens with `--advertise`. A host that listens on loopback only advertises
`127.0.0.1`, which other machines will not see (and browsers drop by default).

## Client side

`rgit discover [--timeout SECS] [--watch] [--project UUID | --remote NAME] [--include-loopback] [--no-probe]`

* `--timeout` (default 3, 0.1–600 s): how long to listen. `--watch` listens until terminated and streams
  `host`/`lost` events.
* Entries are sanitized: instance names have control characters removed and are cut at 63 characters, TXT
  values must be well-formed (`p`: 16 lowercase hex, `v`: a number) or are ignored, port 0 and unusable
  addresses (unspecified, multicast, IPv6 link-local, and loopback unless `--include-loopback`) are
  dropped, at most 256 hosts and 16 addresses per host are kept.
* `--project` marks hosts whose hint equals that project's; `--remote NAME` takes the UUID pinned for that
  remote of the current project.
* Unless `--no-probe`, each host gets a plain TCP connect (no bytes sent, 0.8 s) so stale records show up as
  `reachable: false`. mDNS records outlive a host killed without the goodbye packet (this library sets 75 minutes for
  the PTR/TXT records a browser lists from and 2 minutes for SRV/address records).
* `rgit serve --advertise` stops cleanly on SIGINT/SIGTERM (Ctrl-C handler) and sends the goodbye so the
  entry disappears at once. `SIGKILL`, a crash, a forced app quit or power loss send nothing; neighbours keep
  the record until it expires.

## JSON (docs/gui-interface.md)

`rgit --json discover` prints event lines, then the result object:

```json
{"event":"host","host":{"instance":"rgit-3f9a07b1-c2d4","addresses":["192.168.1.5"],"port":7878,
  "urls":["tcp://192.168.1.5:7878"],"project_hint":"3f9a07b1…16hex","protocol_version":1,
  "matches_project":true,"reachable":true,"verified":false,"source":"mdns"}}
{"event":"lost","instance":"rgit-3f9a07b1-c2d4"}
{"ok":true,"data":{"hosts":[ {…as above…} ],"timeout_ms":3000}}
```

`matches_project` is `null` without `--project/--remote`; `reachable` is `null` with `--no-probe`; `verified` is
always `false`. With `--watch` there is no final object (the process runs until terminated).

`rgit --json serve … --advertise` additionally emits `{"event":"advertising","instance":…,"project_hint":…,"port":…}`.

## When it does not work

mDNS needs multicast on the interface. Common blockers: guest/"client isolation" Wi-Fi, corporate networks,
VPN/tunnel adapters taking the route (the test suite found a proxy TUN on the development machine and had to
select the real interface), host firewalls, and on macOS the *Local Network* privacy permission for apps
(no prompt appeared when the tests ran from a terminal here; a signed GUI app may behave differently and was not tried). The CLI prints
a hint when nothing is found; an empty list is not proof that no host exists. The fallback is always an
invitation with an explicit `tcp://` or `ssh://` address.

## What was tested (macOS 27 arm64, Wi-Fi/Ethernet LAN interface `en0` plus loopback; Windows and Linux not run)

| Test | Result on this machine |
|---|---|
| unit: hint is stable/opaque, instance names sanitized, unusable addresses dropped | pass |
| two real processes (`crates/discovery/tests/two_processes.rs`): advertiser child + browser, on loopback and on the LAN address; content is exactly `v`/`p`/port, nothing identifying; the advertiser's clean exit produces a `Lost` event in a browse that was already open | pass, both interfaces (goodbye seen after ~2 s) |
| loopback addresses hidden from browsers unless requested | pass |
| wildcard IPv4 bind publishes no IPv6; a default browser view has no loopback address | pass (LAN IPv4 address; a same-machine browser may additionally see 127.0.0.1 answered on `lo0`, observed in one run, which the first version of this test wrongly forbade) |
| CLI (`crates/cli/tests/discover_processes.rs`): real `rgit serve --advertise` and `rgit --json discover`: event stream then result, hint/`matches_project`/`reachable`, nothing identifying in the record, wildcard server found at the LAN IP with IPv4 only, SIGTERM → `lost` event in `discover --watch` | pass |

Tests skip with an explicit `SKIPPED … NOT exercised` message when multicast is blocked; read the `--nocapture`
output to confirm they ran. Not tested: more than two machines, a second physical host on the LAN, IPv6 browsing,
networks that filter multicast, name conflicts between two advertisers, macOS Local Network permission prompts
from a GUI app, behaviour when interfaces change while running.
