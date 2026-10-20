//! Cost of the encrypted, authenticated channel versus plaintext TCP, on loopback.
//!
//! cargo run --release -p rustgit-host --example auth_overhead [-- --json]
//!
//! Measures (1) connection setup: TCP connect + (handshake) + Hello, (2) raw frame throughput
//! with 4 MiB frames, (3) a real push and a real clone of a repository through the full
//! protocol (hashing, zstd, staging, SQLite all included, so crypto is a small part of it),
//! reporting wall time and bytes on the wire for each mode.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustgit_core::Repo;
use rustgit_host::{
    ensure_host_identity, spawn, AuthStore, DeviceAuthorizer, Host, HostConfig, KeyStorage,
    OpenAccess, RunningHost,
};
use rustgit_protocol::secure::{accept_secure, connect_secure};
use rustgit_protocol::transport::TcpDuplex;
use rustgit_protocol::*;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

struct Setup {
    _tmp: tempfile::TempDir,
    host: RunningHost,
    cfg: ClientConfig,
}

fn setup(secure: bool) -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hosted");
    Host::init(&dir).unwrap();
    let (auth, cfg): (Arc<dyn rustgit_host::Authorizer>, ClientConfig) = if secure {
        let store = Arc::new(AuthStore::open(&dir).unwrap());
        let ks = KeyStorage::File(tmp.path().join("keys"));
        let (key, _) = ensure_host_identity("bench", &ks, &store).unwrap();
        let fp = key.fingerprint();
        let me = Arc::new(DeviceKey::generate());
        let (id, secret) = store
            .create_invitation(Role::Admin, 3600, 1, "", rustgit_host::authz::unix_now())
            .unwrap();
        let cfg = ClientConfig::new("bench").secure(me, fp).enrolling(id, *secret.expose());
        (Arc::new(DeviceAuthorizer::new(store, key, vec![])), cfg)
    } else {
        (Arc::new(OpenAccess), ClientConfig::new("bench"))
    };
    let host = Host::open(&dir, HostConfig::default(), auth).unwrap();
    let host = spawn(host, Box::new(TcpListenerAdapter::bind("127.0.0.1:0").unwrap()));
    Setup { _tmp: tmp, host, cfg }
}

fn connect_latency(secure: bool) -> f64 {
    let s = setup(secure);
    let t = TcpTransport::new(s.host.addr.clone());
    // the first connection enrolls; afterwards the device is known
    let first = Client::connect(&t, &s.cfg, CancelToken::new()).unwrap();
    first.close();
    let mut cfg = s.cfg.clone();
    cfg.credential.clear();
    let mut samples = Vec::new();
    for _ in 0..300 {
        let t0 = Instant::now();
        let c = Client::connect(&t, &cfg, CancelToken::new()).unwrap();
        samples.push(t0.elapsed().as_secs_f64() * 1e3);
        c.close();
    }
    median(samples)
}

/// TCP connect (+ Noise XX handshake when `secure`) + one tiny request/response, median of
/// 300, against a persistent accept loop (so the host's accept polling is not measured).
fn handshake_latency(secure: bool) -> f64 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let host_key = Arc::new(DeviceKey::generate());
    let me = DeviceKey::generate();
    let hk = host_key.clone();
    std::thread::spawn(move || {
        while let Ok((s, _)) = l.accept() {
            let hk = hk.clone();
            std::thread::spawn(move || {
                let io: Box<dyn Duplex> = Box::new(TcpDuplex::new(s).unwrap());
                let io = if secure {
                    match accept_secure(io, &hk, &[], Duration::from_secs(10)) {
                        Ok((io, _)) => io,
                        Err(_) => return,
                    }
                } else {
                    io
                };
                let mut ch = Channel::new(io, Some(Duration::from_secs(10)));
                if ch.recv(Some(Duration::from_secs(10))).is_ok() {
                    let _ = ch.send(b"k");
                }
            });
        }
    });
    let mut samples = Vec::new();
    for _ in 0..300 {
        let t0 = Instant::now();
        let io: Box<dyn Duplex> = Box::new(TcpDuplex::new(TcpStream::connect(addr).unwrap()).unwrap());
        let io = if secure {
            connect_secure(io, &me, &host_key.fingerprint(), Duration::from_secs(10)).unwrap().0
        } else {
            io
        };
        let mut ch = Channel::new(io, Some(Duration::from_secs(10)));
        ch.send(b"hello").unwrap();
        ch.recv(Some(Duration::from_secs(10))).unwrap();
        samples.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    median(samples)
}

/// Frame throughput over loopback: client sends `total` bytes in 4 MiB frames, host echoes an
/// ack per frame. Returns (MiB/s, wire bytes sent by the client).
fn raw_throughput(secure: bool, total: usize) -> (f64, u64) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let host_key = Arc::new(DeviceKey::generate());
    let me = DeviceKey::generate();
    let hk = host_key.clone();
    let srv = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let io: Box<dyn Duplex> = Box::new(TcpDuplex::new(s).unwrap());
        let io = if secure {
            accept_secure(io, &hk, &[], Duration::from_secs(10)).unwrap().0
        } else {
            io
        };
        let mut ch = Channel::new(io, Some(Duration::from_secs(30)));
        ch.set_max_frame(8 << 20);
        while let Ok(_p) = ch.recv(Some(Duration::from_secs(30))) {
            if ch.send(b"k").is_err() {
                break;
            }
        }
    });
    let io: Box<dyn Duplex> = Box::new(TcpDuplex::new(TcpStream::connect(addr).unwrap()).unwrap());
    let io = if secure {
        connect_secure(io, &me, &host_key.fingerprint(), Duration::from_secs(10)).unwrap().0
    } else {
        io
    };
    let mut ch = Channel::new(io, Some(Duration::from_secs(30)));
    ch.set_max_frame(8 << 20);
    // incompressible payload so nothing is hidden by compression
    let mut x = 0x9E3779B97F4A7C15u64;
    let frame: Vec<u8> = (0..4 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let t0 = Instant::now();
    for _ in 0..total / frame.len() {
        ch.send(&frame).unwrap();
        ch.recv(Some(Duration::from_secs(30))).unwrap();
    }
    let secs = t0.elapsed().as_secs_f64();
    let wire = ch.wire_counts().0;
    drop(ch);
    let _ = srv.join();
    (total as f64 / (1 << 20) as f64 / secs, wire)
}

fn make_repo(dir: &std::path::Path, mib: usize, compressible: bool) -> Repo {
    fs::create_dir_all(dir).unwrap();
    let mut repo = Repo::init(dir, "main").unwrap();
    let mut x = 0x1234_5678_9ABC_DEF1u64;
    for i in 0..mib {
        let data: Vec<u8> = if compressible {
            {
                let pat: &[u8] = b"fn main() { println!(\"hello\"); }\n";
                (0..1 << 20).map(|j| pat[(j + i) % pat.len()]).collect()
            }
        } else {
            (0..1 << 20)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect()
        };
        fs::write(dir.join(format!("f{i}.bin")), data).unwrap();
    }
    repo.commit("bench", &[]).unwrap();
    repo
}

/// (push seconds, push wire bytes sent, clone seconds, clone wire bytes received)
fn push_and_clone(secure: bool, mib: usize, compressible: bool) -> (f64, u64, f64, u64) {
    let s = setup(secure);
    let tmp = tempfile::tempdir().unwrap();
    let mut repo = make_repo(&tmp.path().join("w"), mib, compressible);
    let t = TcpTransport::new(s.host.addr.clone());
    let mut c = Client::connect(&t, &s.cfg, CancelToken::new()).unwrap();
    let t0 = Instant::now();
    c.push(
        &mut repo,
        &PushOptions {
            branch: "main".into(),
            remote_branch: "main".into(),
            remote: "origin".into(),
            force: false,
        },
    )
    .unwrap();
    let push_secs = t0.elapsed().as_secs_f64();
    let push_wire = c.stats.wire_bytes_sent;
    c.close();
    let mut cfg = s.cfg.clone();
    cfg.credential.clear();
    let t1 = Instant::now();
    let before = rustgit_host::authz::unix_now();
    let _ = before;
    let dest = tmp.path().join("clone");
    // clone_into hides its client; measure wire bytes with a plain fetch instead
    let mut r2 = Repo::init(&{
        fs::create_dir_all(&dest).unwrap();
        dest.clone()
    }, "main")
    .unwrap();
    let mut c2 = Client::connect(&t, &cfg, CancelToken::new()).unwrap();
    c2.fetch(&mut r2, "origin").unwrap();
    let clone_secs = t1.elapsed().as_secs_f64();
    let clone_wire = c2.stats.wire_bytes_received;
    c2.close();
    (push_secs, push_wire, clone_secs, clone_wire)
}

fn main() {
    if let Some(i) = std::env::args().position(|a| a == "--only-raw") {
        let secure = std::env::args().nth(i + 1).as_deref() == Some("secure");
        let (mibs, wire) = raw_throughput(secure, 2048 << 20);
        println!("raw {} {:.0} MiB/s wire {wire}", if secure { "secure" } else { "plain" }, mibs);
        return;
    }
    let json = std::env::args().any(|a| a == "--json");
    let mut rows = Vec::new();
    let mut out = |k: &str, plain: f64, sec: f64, unit: &str| {
        rows.push((k.to_string(), plain, sec, unit.to_string()));
    };

    out("TCP connect + 1 round trip (median of 300)", handshake_latency(false), handshake_latency(true), "ms");
    out("full client connect + Hello (host accept loop polls every 5 ms)", connect_latency(false), connect_latency(true), "ms");
    let (p, pw) = raw_throughput(false, 512 << 20);
    let (s, sw) = raw_throughput(true, 512 << 20);
    out("raw 4 MiB-frame throughput (512 MiB, incompressible)", p, s, "MiB/s");
    out("raw wire bytes sent per payload byte", pw as f64 / (512u64 << 20) as f64, sw as f64 / (512u64 << 20) as f64, "x");
    for (label, comp) in [("incompressible", false), ("compressible", true)] {
        // alternate the modes and take the median of 5 so background load hits both alike
        let (mut pp, mut sp, mut pc, mut sc) = (vec![], vec![], vec![], vec![]);
        let (mut ppw, mut spw, mut pcw, mut scw) = (0u64, 0u64, 0u64, 0u64);
        for _ in 0..5 {
            let a = push_and_clone(false, 64, comp);
            let b = push_and_clone(true, 64, comp);
            pp.push(a.0);
            sp.push(b.0);
            pc.push(a.2);
            sc.push(b.2);
            (ppw, pcw, spw, scw) = (a.1, a.3, b.1, b.3);
        }
        out(&format!("push 64 MiB {label}: time (median of 5)"), median(pp), median(sp), "s");
        out(&format!("push 64 MiB {label}: wire bytes sent"), ppw as f64, spw as f64, "B");
        out(&format!("fetch 64 MiB {label}: time (median of 5)"), median(pc), median(sc), "s");
        out(&format!("fetch 64 MiB {label}: wire bytes received"), pcw as f64, scw as f64, "B");
    }

    if json {
        let v: Vec<_> = rows
            .iter()
            .map(|(k, p, s, u)| format!("{{\"metric\":{k:?},\"unit\":{u:?},\"plaintext\":{p:.4},\"encrypted\":{s:.4}}}"))
            .collect();
        println!("[{}]", v.join(",\n"));
    } else {
        println!("{:<58} {:>14} {:>14} {:>9}", "metric", "plaintext", "encrypted", "ratio");
        for (k, p, s, u) in &rows {
            println!("{:<58} {:>10.3} {:<3} {:>10.3} {:<3} {:>8.3}x", k, p, u, s, u, s / p);
        }
    }
}
