# Relays and Iroh connectivity: running your own relay

`rgit` can reach a host over the internet through [Iroh](https://docs.iroh.computer/): the two devices
try to open a **direct** UDP path (hole punching) and, while that is not possible or not yet
established, send everything through a **relay**. The relay is a small stateless server that only
forwards already-encrypted QUIC packets. This page covers how the pieces fit together, what a relay can and
cannot see, and how to run and test your own. The evaluation behind the choice is decision D20 in
[`decisions.md`](decisions.md); what was and was not tested is in [`status.md`](status.md).

**Nothing here is a promise that two devices will connect directly.** Whether hole punching works depends
on both NATs and firewalls. A relay is what makes the connection *work* in the cases where it does not; it
costs latency and bandwidth on a server you run. Iroh's own documentation says roughly nine in ten
network situations allow a direct path; we have not measured that and could not (see
[What was tested](#8-what-was-tested)).

## 1. How a connection is made

```
device A  ---- direct UDP (when hole punching works) ----  host
    \                                                       /
     \--- HTTPS/WebSocket ---  relay  ---- HTTPS/WebSocket --/     (always available first)
```

1. The host binds an Iroh *endpoint* with a long-lived key. The public half is its **endpoint id**
   (64 hex characters). It keeps one connection open to its **home relay**.
2. The host prints (or an invitation carries) `iroh://<endpoint-id>?relay=<home relay URL>`.
3. A client dials the id. Iroh reaches the host through the relay named in the URL, completes the
   QUIC/TLS handshake *end to end with the host's key* (a relay cannot impersonate the host, see §2) and
   at the same time exchanges candidate addresses so both sides can try to punch a direct path.
4. Data flows on the relay path until a direct path works, then moves to it; if the direct path later
   dies (Wi-Fi change, NAT mapping expiry) it falls back to the relay and tries again. The state is
   reported to the UI as `direct`, `relayed` or `unreachable` (§6).

The host's endpoint id is stable across restarts and IP address changes as long as its key file is kept
(default `<hosted dir>/.rgit/host/iroh.key`, mode 0600). Addresses in a URL are hints only; the relay
hint is what lets a client find a host whose IP address changed. Keep this key file private and backed up
with the project: it is the host's *transport* identity. It is **not** what authorizes devices; invitations,
device keys and the pinned host fingerprint (`docs/protocol.md` §12) still decide who may read or write.

## 2. What a relay can and cannot see

The application protocol runs inside QUIC, which runs inside TLS 1.3 whose certificates are the endpoints'
raw Ed25519 public keys (RFC 7250). The client verifies that the key presented is the endpoint id it
dialed, and the host learns the client's id the same way. The relay forwards opaque UDP payloads between
two ids and never holds a key that decrypts them. On top of that `rgit` adds its own Noise channel with
device authentication (`docs/protocol.md` §12), so a relay faces two independent layers.

A relay (or whoever runs it, or anyone watching its network) **can see**:

* the endpoint id of every device that connects to it, and the destination endpoint id of every packet,
  i.e. *which devices talk to which*; it is in the clear in the relay protocol (this is tested, below);
* IP addresses of the connecting devices, connection times, durations, packet sizes and counts, and
  therefore roughly how much data a sync moved;
* the relay URL in the invitation if it was intercepted (the id is not secret either).

It **cannot**: read or modify file contents, ref names, commit messages or object ids (all inside
encryption); pretend to be the host; or learn invitation secrets. It **can** drop, delay or block traffic
(a denial of service); the client then reports `unreachable` or stays on whatever path still works.

What was verified, not just claimed: `the_relay_sees_only_ciphertext_and_some_metadata` in
`crates/transport-iroh/tests/iroh_transport.rs` runs a real `iroh-relay` server over plain HTTP behind a
recording TCP tap (so the tap sees exactly what the relay receives and sends), transfers a repository
containing a unique marker (random file bytes, a commit message, a text file) and asserts that none of
the marker appears in the captured relay traffic while the host's endpoint id does; a control test
shows the same tap *does* see the marker when the plaintext TCP transport is used. A second test
(`a_relay_cannot_stand_in_for_the_host`) dials an id nobody holds the key for through a working relay
and checks that the connection is refused.

Public relays operated by n0 (the Iroh developers) are available and are the default of the Iroh library,
but `rgit` **never uses them unless you opt in** (`public_relay = true`). They are rate limited, come with
no guarantees and are meant for development; they would see the metadata above for your devices.
For anything you depend on, run your own relays or use a provider you have an agreement with.

## 3. Running a relay

Requirements: a machine with a public IP address and a DNS name (for example `relay.example.org`), ports
below open, and a few hundred MB of RAM. A relay stores nothing; its cost is network bandwidth (the whole
payload of every relayed sync passes through it) and CPU for TLS and packet forwarding. Run at least two
in different regions for redundancy: clients and hosts are given the list and fail over by themselves
(Iroh documentation: "run at least two relays in different regions").

Get `iroh-relay`:

* Release binaries: <https://github.com/n0-computer/iroh/releases>, or the Docker image
  `n0computer/iroh-relay` (both per the Iroh documentation; **not used here**), or build the exact
  version this repository was tested with:

  ```bash
  cargo install iroh-relay --version 1.3.0 --features server --locked
  ```

  `rgit` is built against Iroh 1.3.0. Relay and client speak a versioned protocol, but run the same
  minor version on both ends unless you have tested another combination.

### Ports

| Port | Protocol | Purpose | Expose to the internet? |
|---|---|---|---|
| 443 | TCP | HTTPS + WebSocket: the relay itself | **yes** |
| 80 | TCP | plain HTTP: Let's Encrypt HTTP-01 challenge and a captive-portal check | yes while using Let's Encrypt |
| 7842 | UDP | QUIC address discovery (helps clients learn their public address for hole punching; relaying works without it) | optional; `enable_quic_addr_discovery = true` |
| 9090 | TCP | Prometheus metrics, **no authentication** | **no**: bind to `127.0.0.1` or firewall it |

### Configuration (TOML)

`/etc/iroh-relay/config.toml`, started with `iroh-relay --config-path /etc/iroh-relay/config.toml`.

**With Let's Encrypt** (the configuration from the Iroh documentation; needs port 80 and 443 and a DNS
name pointing at this machine):

```toml
http_bind_addr = "[::]:80"
enable_metrics = true
metrics_bind_addr = "127.0.0.1:9090"      # never public

[tls]
https_bind_addr = "[::]:443"
hostname = "relay.example.org"
cert_mode = "LetsEncrypt"
cert_dir = "/var/lib/iroh-relay/certs"
contact = "ops@example.org"
```

**With your own certificate** (internal CA, or a certificate you already manage; certificates are read
again periodically with `cert_mode = "Reloading"`, so renewing does not need a restart):

```toml
http_bind_addr = "[::]:80"

[tls]
https_bind_addr = "[::]:443"
cert_mode = "Reloading"
manual_cert_path = "/etc/iroh-relay/relay.crt"   # full chain, PEM
manual_key_path  = "/etc/iroh-relay/relay.key"
```

Clients must then trust the issuing CA: give each device `relay_ca = /path/to/ca.pem` (§5).

**Who may use the relay.** By default anyone who knows the URL can relay traffic through it (and consume
your bandwidth). Restrict it (these `access.*` lines are top-level keys: put them above the first `[table]`
header of the file):

```toml
# only these endpoint ids may relay (hosts AND clients: clients need a persistent key_file, §5)
access.allowlist = ["<64 hex endpoint id>", "<64 hex endpoint id>"]

# or: a shared secret that hosts and clients present (`relay_token`, §5)
access.shared_token = ["a-long-random-token"]

# or: ask your own HTTP service for every connecting endpoint id
[access.http]
url = "https://auth.example.org/relay-allowed"
bearer_token = "..."
```

Per-client rate limit (bytes per second the relay reads from one client connection):

```toml
[limits.client.rx]
bytes_per_second = 5000000
max_burst_bytes = 20000000
```

The complete list of keys is the `Config` struct in `iroh-relay/src/main.rs`; the Iroh documentation says
the options are not otherwise published. This page documents what we ran (§7).

### Running as a service with resource limits

```ini
# /etc/systemd/system/iroh-relay.service
[Unit]
Description=iroh relay
After=network-online.target
Wants=network-online.target

[Service]
User=iroh-relay
Group=iroh-relay
ExecStart=/usr/local/bin/iroh-relay --config-path /etc/iroh-relay/config.toml
Restart=always
RestartSec=2
StateDirectory=iroh-relay
AmbientCapabilities=CAP_NET_BIND_SERVICE   # bind 80/443 without root
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
LimitNOFILE=65536                          # one socket per connected device
MemoryMax=1G
TasksMax=4096
CPUQuota=200%

[Install]
WantedBy=multi-user.target
```

Sizing rules of thumb (measured numbers are in §7): memory grows with the number of connected endpoints,
not with traffic; bandwidth is the sum of the relayed syncs. Because the relay is stateless, restarting or
replacing it only drops the connections that used it; clients reconnect and `rgit` resumes interrupted
transfers (tested, §7).

Health check:

```bash
curl --noproxy '*' --fail --show-error https://relay.example.org/healthz
```

Firewall: allow inbound TCP 443 (and 80 for Let's Encrypt), UDP 7842 if address discovery is on. Hosts
and devices need only *outbound* HTTPS to the relay (and UDP for direct paths); they open no inbound port.

## 4. Pointing a host at your relay

```bash
rgit serve /srv/project.rgit --init --iroh --iroh-relay https://relay.example.org
```

prints

```
Iroh endpoint 3745592a...
Iroh URL iroh://3745592a...?relay=https://relay.example.org
```

and, with `--json`, a `{"event":"listening_iroh","url":...,"relay_online":true}` line (`relay_online`
is false when the relay could not be reached within 15 s; the host keeps trying). Hand the URL out in
an invitation so devices need nothing else:

```bash
rgit invite create --role write --hint 'iroh://3745592a...?relay=https://relay.example.org'
```

`--iroh` is refused together with `--insecure-open-access`: reaching the host from the internet needs
device authentication. `--iroh-hint-addrs` adds the machine's IP addresses to the URL (a faster first
connection on a LAN, but everyone who gets the URL learns your private addresses; off by default). With no
relay configured the URL carries direct addresses automatically, which only works on a LAN or with a
forwarded UDP port.

## 5. Client and host settings

Resolved in this order: command-line flags (`serve`), environment, then `iroh.conf` in the rgit config
directory (`RGIT_HOME`, else `~/Library/Application Support/rgit` on macOS, `%APPDATA%\rgit` on Windows,
`~/.config/rgit` elsewhere). `iroh.conf` is `key = value` lines; `#` starts a comment; `relay` may repeat.

| `iroh.conf` key | Environment variable | Meaning |
|---|---|---|
| `relay` | `RGIT_IROH_RELAY` (comma separated) | Your relay URL(s). With none, only direct addresses work |
| `public_relay` | `RGIT_IROH_PUBLIC_RELAY` | `true` opts in to n0's public relays (development only; cannot be combined with `relay`) |
| `relay_ca` | `RGIT_IROH_RELAY_CA` | PEM file with the CA(s) that signed a private relay's certificate; replaces the built-in web roots |
| `relay_token` | `RGIT_IROH_RELAY_TOKEN` | Bearer token for a relay with `access.shared_token`. A secret: put it in the environment or a private `iroh.conf`, never in a URL or an invitation |
| `direct` | `RGIT_IROH_DIRECT` | `false` never opens a UDP socket: every byte goes through the relay (the "forced relay" mode) |
| `bind` | `RGIT_IROH_BIND` | Bind the only UDP socket to this `ip:port` |
| `key_file` | `RGIT_IROH_KEY_FILE` | Persistent endpoint key. Clients use a throw-away key per run by default (a relay cannot link their sessions); set this when the relay's `access.allowlist` must name them |
| `connect_timeout` | `RGIT_IROH_CONNECT_TIMEOUT` | Seconds to wait for a connection, default 20 |

Example `iroh.conf` for a team with its own relay:

```
relay = https://relay.example.org
```

A URL without a relay hint (`iroh://<id>`) uses the configured relays as the hint. An `iroh://` URL has
these parameters: `relay=<url>` (repeatable), `addr=<ip:port>` (repeatable); anything else is rejected.

## 6. What the app is told

Every command that used an iroh remote adds a `connection` object to its `--json` result (and to
`error` when it failed), and `--json` streams `{"event":"connection",...}` lines when the path changes:

```json
{"transport":"iroh","state":"relayed","path":"relay https://relay.example.org/",
 "used_relay":true,"used_direct":false,"path_switches":0,"error":null,
 "remote":"3745592a...","local":"c01d1d..."}
```

* `state` is `direct` (application data flows over a direct UDP path), `relayed` (through the relay)
  or `unreachable` (the host could not be reached, or the connection was lost and has not come back;
  `error` says why in words); `connecting` only appears while a connection is being made.
* `local` is this device's own endpoint id. The state reflects the path QUIC is sending on *now*. A connection often starts relayed and upgrades to
  direct; `used_relay` / `used_direct` / `path_switches` record the history of the connection. Show
  "relayed" as slower-but-working, not as a failure.
* Human-mode output prints one `iroh: relayed via relay https://...` line on stderr per change.
* `host_unreachable` is the error `code`; the message names the usual causes (host offline, relay down,
  both networks blocking direct paths with no relay).

## 7. Validation of this guide

The released `iroh-relay` 1.3.0 binary (built with `cargo install iroh-relay --version 1.3.0 --features
server --locked`) was run on this machine with the configuration below, a private CA and a certificate
for `127.0.0.1` (made with `openssl`, `cert_mode = "Reloading"`), and `rgit` host and client processes
went through it:

```toml
http_bind_addr = "127.0.0.1:38080"
enable_metrics = true
metrics_bind_addr = "127.0.0.1:39090"
access.shared_token = ["a-long-random-token"]

[tls]
https_bind_addr = "127.0.0.1:38443"
cert_mode = "Reloading"
cert_dir = "..."
manual_cert_path = ".../relay.crt"
manual_key_path = ".../relay.p8.key"      # PKCS#8 PEM

[limits.client.rx]
bytes_per_second = 50000000
max_burst_bytes = 100000000
```

| Check | Result |
|---|---|
| The TOML above parses and the relay starts; `curl --cacert ca.crt https://127.0.0.1:38443/healthz` | `200`. The plain-HTTP port answers `/healthz` with 404: it only serves the captive-portal check, so health-check the HTTPS URL |
| Metrics on a loopback-only address | served (`relayserver_*`); stays unauthenticated, so keep it off public interfaces |
| `rgit serve --iroh` through it with `RGIT_IROH_RELAY_CA` (private CA), forced relay | works; `relay_online: true`; the invitation's `iroh://...?relay=https://127.0.0.1:38443/` hint is accepted |
| Host without `RGIT_IROH_RELAY_TOKEN` | the relay refuses it: `relay_online: false` after 15 s and the warning explains that devices can then only use direct addresses |
| `rgit join` (clone) and a 60 MiB `push` through the relay, `--json` | `connection.state` is `relayed`; the push took 1.4 s in a release build on loopback (not a network figure) |
| Relay cost of that push | 65 MB in and 65 MB out per the relay's own metrics; the relay process peaked at **9.7 MB RSS** and about **33% of one core** (sampled every 0.3 s) with 4 connections accepted, 0 rate-limited |
| `access.allowlist = [host id, client id]` | the host (persistent key) registers; a client with a persistent `key_file` on the list connects (the relay path is used; the pinned-host check above it then answers as usual); a client with the default ephemeral key is `unreachable`. The client's own id is in `connection.local` of any `--json` result or error, so an operator can read it off a first failed attempt |
| `access.shared_token` and the client token | enforced on the real binary (above) and, separately, in `relay_access_token_is_enforced_and_presented` (no token and a wrong token are refused, the right one carries a transfer) |

Resource limits (`systemd` `MemoryMax`, `LimitNOFILE`, `CPUQuota`) are standard systemd settings and were
**not** exercised here; the numbers above are what a single idle-to-busy relay with a handful of
connections used.

## 8. What was tested

Everything ran on **one macOS machine behind one NAT** (Darwin 27, arm64), with a real `iroh-relay`
server (in-process from the `iroh-relay` crate for the test suite, and the released binary for §7) and real `rgit` and host
processes; repositories were compared byte for byte after every transfer.

| Scenario | Result |
|---|---|
| Direct connection on loopback, push + clone of a real repository | works, state `direct` |
| **Forced relay**: no UDP socket on either side, everything through a locally started relay, multi-MiB push + clone | works, state `relayed`, files identical |
| Same through the CLI (`serve --iroh`, invitation with `iroh://` hint, `join`, `push`, `fetch`, JSON `connection`) | works |
| What the relay sees (tap on the relay's wire) | no file bytes, commit message or note text visible; endpoint ids visible |
| Wrong endpoint id through a working relay | refused (`unreachable`), the relay cannot answer for it |
| Relay down / nothing listening / host offline | `unreachable` within the connect timeout, clear message, JSON `host_unreachable` |
| Host restarts on another address with the same key | same endpoint id; old URL (stale address hint) still connects |
| Relay killed during a 60 MiB push, back 1.5 s later | the open connection absorbed it (one attempt) |
| Relay killed during a push, back 8 s later, 3 s stall timeout | attempt failed, retried 3 times, push completed |
| NAT-rebinding-like change (the peer's source address changes twice mid-push, UDP proxy) | one connection, transfer slowed (8.0 s vs 3.5 s baseline) but completed |
| Relay access control: shared token (right, wrong, none) and endpoint allowlist | enforced, see §7 |
| The whole transport test suite (16 tests) with all outbound network except loopback denied (macOS `sandbox-exec`, proxy variables unset; a control `curl` to the internet fails there) | passes: nothing in these flows needs n0's relays, DNS or any other third-party service |
| OS network-change notification during a push (`network_changed()`) | harmless (does not simulate a change: nothing really changed) |

**Not tested, and not claimed:**

* **Hole punching between two different NATs.** Both devices were on one machine, so the "direct" paths
  above are loopback/LAN paths; no packet was ever punched through a NAT. Iroh's hole punching is the
  library's own; whether it succeeds for a given pair of networks (symmetric NATs, carrier-grade NAT,
  UDP-blocking firewalls) is unknown here. Expect to need the relay for some users.
* A relay on the public internet with a real certificate and DNS (`LetsEncrypt` mode): the configuration
  is from the Iroh documentation and the source, run locally only with `cert_mode = "Manual"`/`Reloading`.
* Real network changes (switching Wi-Fi, VPN, sleep/wake, IPv4/IPv6 changes). Only synthetic source-address
  changes were made.
* Windows and Linux; Docker image; IPv6-only networks; many simultaneous devices; throughput limits.
* Iroh's public (n0) relays and DNS discovery are not used and were not tested.
