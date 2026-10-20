//! Real-network tests for the Iroh transport: real `iroh-relay` servers, real hosts, real
//! repositories, bytes compared after transfer. Everything runs on one machine, so direct
//! paths are loopback paths and no test here crosses two NATs (see docs/status.md).

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use rustgit_transport_iroh::{endpoint_id_of, IrohConfig, IrohUrl, PathState, RelaySetting};

fn loopback() -> IrohConfig {
    IrohConfig {
        bind_addr: Some("127.0.0.1:0".parse().unwrap()),
        ..IrohConfig::default()
    }
}

fn sample_repo(parent: &std::path::Path, name: &str, mib: usize) -> rustgit_core::Repo {
    let mut r = new_repo(parent, name);
    write(&r, "src/main.rs", b"fn main() { println!(\"hello\"); }\n");
    write(&r, "README.md", b"# demo\n\nover iroh\n");
    write(&r, "data/blob.bin", &noise(1, mib * 1024 * 1024));
    write(&r, "data/other.bin", &noise(2, 300_000));
    r.commit("first", &[]).unwrap();
    r
}

#[test]
fn direct_connection_on_loopback_transfers_a_real_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let host = start_host(&tmp.path().join("hosted"), &loopback(), true);
    // No relay anywhere: the only way in is the direct address hint.
    assert!(!host.join_url.contains("relay="), "{}", host.join_url);
    assert!(
        host.join_url.contains("addr=127.0.0.1:"),
        "{}",
        host.join_url
    );

    let mut a = sample_repo(tmp.path(), "a", 3);
    let t = transport(&host.join_url, loopback());
    let head = push(&t, &mut a, "dev-a");
    let st = t.status();
    assert_eq!(st.state, PathState::Direct, "{st:?}");
    assert!(!st.used_relay);

    let dest = tmp.path().join("b");
    let b = clone_to(&t, &host.join_url, &dest);
    assert_eq!(b.head_commit().unwrap(), Some(head));
    assert_same_tree(&a.root, &dest);
    assert!(b.verify().unwrap().problems.is_empty());
    drop(t);
    host.running.stop();
}

#[test]
fn forced_relay_transfers_a_real_repo_through_a_local_relay() {
    let relay = TestRelay::https(0);
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = relay_cfg(&relay);
    cfg.direct = false; // no UDP socket at all: every byte must go through the relay
    let host = start_host(&tmp.path().join("hosted"), &cfg, false);
    assert!(
        host.sockets.is_empty() || host.sockets.iter().all(|s| s.port() == 0),
        "{:?}",
        host.sockets
    );
    assert!(
        host.join_url.contains("relay=https%3A") || host.join_url.contains("relay=https://"),
        "{}",
        host.join_url
    );

    let mut a = sample_repo(tmp.path(), "a", 5);
    let t = transport(&host.join_url, cfg.clone());
    let head = push(&t, &mut a, "dev-a");
    let st = t.status();
    assert_eq!(st.state, PathState::Relayed, "{st:?}");
    assert!(st.used_relay && !st.used_direct, "{st:?}");
    assert!(
        st.selected.as_deref().unwrap().starts_with("relay "),
        "{st:?}"
    );

    let dest = tmp.path().join("b");
    let b = clone_to(&t, &host.join_url, &dest);
    assert_eq!(b.head_commit().unwrap(), Some(head));
    assert_same_tree(&a.root, &dest);
    assert!(b.verify().unwrap().problems.is_empty());
    assert_eq!(t.status().state, PathState::Relayed);
    drop(t);
    host.running.stop();
}

#[test]
fn the_relay_sees_only_ciphertext_and_some_metadata() {
    // A plain-HTTP relay with a recording TCP tap in front of it: the tap sees exactly the
    // bytes the relay receives and sends (websocket frames carrying QUIC packets).
    let relay = TestRelay::http(0);
    let tap = TcpTap::new(format!("127.0.0.1:{}", relay.port).parse().unwrap());
    let tapped_url = format!("http://127.0.0.1:{}", tap.port);
    let cfg = IrohConfig {
        relay: RelaySetting::Custom(vec![tapped_url.clone()]),
        direct: false,
        ..IrohConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = start_host(&tmp.path().join("hosted"), &cfg, false);

    let mut a = new_repo(tmp.path(), "a");
    let secret = noise(77, 200_000);
    write(&a, "secret.bin", &secret);
    write(
        &a,
        "notes.txt",
        b"TOP-SECRET-PROJECT-NOTES-0123456789 do not leak\n",
    );
    a.commit("TOP-SECRET-COMMIT-MESSAGE-ABCDEF", &[]).unwrap();
    let t = transport(&host.join_url, cfg.clone());
    push(&t, &mut a, "dev-a");
    assert_eq!(t.status().state, PathState::Relayed);
    drop(t);
    host.running.stop();

    let seen = tap.all();
    assert!(
        seen.len() > secret.len(),
        "the relay carried the transfer ({} bytes)",
        seen.len()
    );
    // Content never appears in the clear: not file bytes, not the commit message.
    for off in [0usize, 50_000, 100_000, 150_000, 199_000] {
        assert!(
            !contains(&seen, &secret[off..off + 32]),
            "file bytes visible to the relay at {off}"
        );
    }
    assert!(!contains(&seen, b"TOP-SECRET-COMMIT-MESSAGE"));
    assert!(!contains(&seen, b"TOP-SECRET-PROJECT-NOTES"));
    // Metadata the relay does learn: endpoint ids appear in its protocol (who talks to whom).
    let host_id = hex_bytes(&host.id);
    assert!(
        contains(&seen, &host_id),
        "the destination endpoint id is visible to the relay"
    );
}

fn hex_bytes(h: &str) -> Vec<u8> {
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

#[test]
fn tap_control_plain_tcp_does_expose_the_same_bytes() {
    // The same marker over the plaintext TCP transport must be visible to the tap, otherwise
    // the test above would prove nothing about the tap.
    use rustgit_host::{spawn, Host, HostConfig, OpenAccess};
    use rustgit_protocol::{TcpListenerAdapter, TcpTransport};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hosted");
    Host::init(&dir).unwrap();
    let host = Host::open(&dir, HostConfig::default(), std::sync::Arc::new(OpenAccess)).unwrap();
    let l = TcpListenerAdapter::bind("127.0.0.1:0").unwrap();
    let addr = l.socket_addr().unwrap();
    let running = spawn(host, Box::new(l));
    let tap = TcpTap::new(addr);
    let mut a = new_repo(tmp.path(), "a");
    let secret = noise(77, 200_000);
    write(&a, "secret.bin", &secret);
    a.commit("TOP-SECRET-COMMIT-MESSAGE-ABCDEF", &[]).unwrap();
    let t = TcpTransport::new(format!("127.0.0.1:{}", tap.port));
    push(&t, &mut a, "dev-a");
    let seen = tap.all();
    assert!(
        contains(&seen, &secret[50_000..50_032]),
        "tap would see plaintext file bytes"
    );
    running.stop();
}

#[test]
fn a_relay_cannot_stand_in_for_the_host() {
    // Dialing an id whose key nobody here holds, through a relay that is fine: the relay
    // cannot answer for it, because TLS inside QUIC authenticates the endpoint id.
    let relay = TestRelay::https(0);
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = relay_cfg(&relay);
    cfg.direct = false;
    let host = start_host(&tmp.path().join("hosted"), &cfg, false);
    let stranger = endpoint_id_of(&[9u8; 32]);
    assert_ne!(stranger, host.id);
    let mut u: IrohUrl = host.join_url.parse().unwrap();
    u.id = stranger;
    let t = transport(&u.to_string(), cfg.clone());
    let started = Instant::now();
    let err = match rustgit_protocol::Transport::connect(&t, Duration::from_secs(4)) {
        Err(e) => e,
        Ok(_) => panic!("connected to an endpoint that is not the host"),
    };
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(t.status().state, PathState::Unreachable);
    assert!(err.to_string().contains("cannot reach iroh host"), "{err}");
    host.running.stop();
}

#[test]
fn unavailable_relay_is_reported_as_unreachable_quickly() {
    // Nothing listens on this port.
    let dead = "https://127.0.0.1:9";
    let cfg = IrohConfig {
        relay: RelaySetting::Custom(vec![dead.into()]),
        direct: false,
        ..IrohConfig::default()
    };
    let t = transport(
        &format!("iroh://{}?relay={dead}", endpoint_id_of(&[5u8; 32])),
        cfg,
    );
    let started = Instant::now();
    let err = match rustgit_protocol::Transport::connect(&t, Duration::from_secs(3)) {
        Err(e) => e,
        Ok(_) => panic!("connected through a relay that does not exist"),
    };
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
    let st = t.status();
    assert_eq!(st.state, PathState::Unreachable, "{st:?}");
    assert!(err.to_string().contains("relay may be down"), "{err}");
    assert!(st.error.is_some());
}

#[test]
fn host_offline_behind_a_working_relay_is_unreachable() {
    let relay = TestRelay::https(0);
    let mut cfg = relay_cfg(&relay);
    cfg.direct = false;
    let url = format!("iroh://{}?relay={}", endpoint_id_of(&[6u8; 32]), relay.url);
    let t = transport(&url, cfg);
    let r = rustgit_protocol::Transport::connect(&t, Duration::from_secs(3));
    assert!(r.is_err());
    assert_eq!(t.status().state, PathState::Unreachable);
}

#[test]
fn no_hints_and_no_relay_is_a_clear_error() {
    let t = transport(
        &format!("iroh://{}", endpoint_id_of(&[7u8; 32])),
        loopback(),
    );
    let e = match rustgit_protocol::Transport::connect(&t, Duration::from_secs(2)) {
        Err(e) => e,
        Ok(_) => panic!("connected with no way to find the host"),
    };
    assert!(e.to_string().contains("no relay or addr hint"), "{e}");
}

#[test]
fn direct_only_without_any_relay_is_refused_at_bind() {
    let cfg = IrohConfig {
        direct: false,
        ..IrohConfig::default()
    };
    let t = transport(&format!("iroh://{}", endpoint_id_of(&[7u8; 32])), cfg);
    let e = match rustgit_protocol::Transport::connect(&t, Duration::from_secs(2)) {
        Err(e) => e,
        Ok(_) => panic!("connected"),
    };
    assert!(e.to_string().contains("nothing could connect"), "{e}");
}

#[test]
fn allowlist_rejects_unknown_devices() {
    use rustgit_transport_iroh::IrohListener;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hosted");
    rustgit_host::Host::init(&dir).unwrap();
    let host = rustgit_host::Host::open(
        &dir,
        Default::default(),
        std::sync::Arc::new(rustgit_host::OpenAccess),
    )
    .unwrap();
    let allowed_key = [3u8; 32];
    let l = IrohListener::bind(&loopback(), Some(vec![endpoint_id_of(&allowed_key)])).unwrap();
    let url = l.join_url(true);
    let running = rustgit_host::spawn(host, Box::new(l));

    // A device with an allowed key gets in.
    let ok = IrohConfig {
        secret_key: Some(allowed_key),
        ..loopback()
    };
    let t = transport(&url, ok);
    let c = rustgit_protocol::Client::connect(
        &t,
        &client_cfg("good"),
        rustgit_protocol::CancelToken::new(),
    )
    .expect("allowed device connects");
    c.close();

    // Any other key is refused (the connection is closed before the protocol starts).
    let other = IrohConfig {
        secret_key: Some([4u8; 32]),
        ..loopback()
    };
    let t2 = transport(&url, other);
    let mut cfg = client_cfg("bad");
    cfg.io_timeout = Duration::from_secs(5);
    cfg.max_retries = 0;
    assert!(
        rustgit_protocol::Client::connect(&t2, &cfg, rustgit_protocol::CancelToken::new()).is_err()
    );
    running.stop();
}

#[test]
fn host_keeps_its_identity_when_its_address_changes() {
    // The host restarts on a different UDP port (a new address, as after a DHCP lease or
    // moving networks). The key file keeps the endpoint id; the URL's relay hint finds it.
    let relay = TestRelay::https(0);
    let tmp = tempfile::tempdir().unwrap();
    let key =
        rustgit_transport_iroh::load_or_create_secret_key(&tmp.path().join("host.key")).unwrap();
    let mut cfg = relay_cfg(&relay);
    cfg.secret_key = Some(key);
    cfg.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    let hosted = tmp.path().join("hosted");

    let h1 = start_host(&hosted, &cfg, true);
    let url1 = h1.join_url.clone();
    let mut a = sample_repo(tmp.path(), "a", 1);
    let t = transport(&url1, relay_cfg(&relay));
    push(&t, &mut a, "dev-a");
    let first_ports = h1.sockets.clone();
    h1.running.stop();

    let h2 = start_host(&hosted, &cfg, true);
    assert_eq!(h2.id, h1.id, "endpoint id is stable across restarts");
    assert_ne!(
        h2.sockets, first_ports,
        "the host really moved to another address"
    );
    // The *old* URL (stale direct address) still works: same transport object, new connection.
    write(&a, "more.txt", b"after the move\n");
    a.commit("second", &[]).unwrap();
    let head = push(&t, &mut a, "dev-a");
    let dest = tmp.path().join("b");
    let b = clone_to(&t, &url1, &dest);
    assert_eq!(b.head_commit().unwrap(), Some(head));
    assert_same_tree(&a.root, &dest);
    h2.running.stop();
}

#[test]
fn network_change_notifications_do_not_disturb_a_transfer() {
    // `network_changed()` asks Iroh's network monitor to re-examine the interfaces; Iroh only
    // rebinds and re-probes if something really changed. On this machine nothing does (the
    // socket keeps its port, see the printed before/after), so this shows that the call an app
    // makes on an OS network-change event is safe in the middle of a transfer. It does NOT
    // simulate an address change: `nat_rebinding_...` below does.
    let relay = TestRelay::https(0);
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = relay_cfg(&relay);
    cfg.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    let host = start_host(&tmp.path().join("hosted"), &cfg, true);
    let mut a = sample_repo(tmp.path(), "a", 24);
    let t = std::sync::Arc::new(transport(&host.join_url, cfg.clone()));

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (t2, stop2) = (t.clone(), stop.clone());
    let roamer = std::thread::spawn(move || {
        let mut n = 0;
        while !stop2.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(150));
            t2.network_changed();
            n += 1;
        }
        n
    });
    // Establish the connection first so the endpoint (and its socket) exists.
    {
        let c = rustgit_protocol::Client::connect(
            &*t,
            &client_cfg("probe"),
            rustgit_protocol::CancelToken::new(),
        )
        .unwrap();
        c.close();
    }
    let sockets_before = t.bound_sockets();
    let head = push(&*t, &mut a, "dev-a");
    stop.store(true, Ordering::SeqCst);
    let changes = roamer.join().unwrap();
    eprintln!(
        "network changes injected during the transfer: {changes}; status {:?}",
        t.status()
    );
    assert!(changes >= 1);
    let sockets_after = t.bound_sockets();
    eprintln!("client sockets before {sockets_before:?}, after {sockets_after:?}");
    let dest = tmp.path().join("b");
    let b = clone_to(&*t, &host.join_url, &dest);
    assert_eq!(b.head_commit().unwrap(), Some(head));
    assert_same_tree(&a.root, &dest);
    host.running.stop();
}

/// Forced relay. The relay is killed while a large push is running and restarted on the same
/// port `outage` later. Returns how many connection attempts the push needed.
fn push_across_relay_outage(outage: Duration, io_timeout: Duration) -> u32 {
    let mut relay = TestRelay::http(0);
    let port = relay.port;
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = relay_cfg(&relay);
    cfg.direct = false;
    cfg.connect_timeout = Some(Duration::from_secs(4));
    let host = start_host(&tmp.path().join("hosted"), &cfg, false);
    let mut a = sample_repo(tmp.path(), "a", 60);
    let t = transport(&host.join_url, cfg.clone());

    let (connected_tx, connected_rx) = std::sync::mpsc::channel::<()>();
    let ((out, took, attempts), _relay2) = std::thread::scope(|s| {
        let worker = s.spawn(|| {
            let mut c = client_cfg("dev-a");
            c.max_retries = 12;
            c.io_timeout = io_timeout;
            let started = Instant::now();
            let mut attempts = 0u32;
            let out =
                rustgit_protocol::with_retries(&c, &rustgit_protocol::CancelToken::new(), || {
                    attempts += 1;
                    let mut cl = rustgit_protocol::Client::connect(
                        &t,
                        &c,
                        rustgit_protocol::CancelToken::new(),
                    )?;
                    let _ = connected_tx.send(());
                    let o = cl.push(&mut a, &push_opts());
                    cl.close();
                    o
                });
            (out, started.elapsed(), attempts)
        });
        connected_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(400));
        relay.stop(); // the outage begins while the upload is in flight
        std::thread::sleep(outage);
        let relay2 = TestRelay::http(port); // the relay is back on the same address
        (worker.join().unwrap(), relay2)
    });
    eprintln!(
        "push across a {outage:?} relay outage (io timeout {io_timeout:?}): {} after {attempts} attempt(s) in {took:?}",
        if out.is_ok() { "ok" } else { "FAILED" }
    );
    assert!(took > outage, "the push must have overlapped the outage");
    assert!(matches!(
        out.expect("push survives the outage"),
        rustgit_protocol::PushOutcome::Pushed { .. }
    ));
    let dest = tmp.path().join("b");
    let b = clone_to(&t, &host.join_url, &dest);
    assert!(b.head_commit().unwrap().is_some());
    assert_same_tree(&a.root, &dest);
    host.running.stop();
    attempts
}

#[test]
fn short_relay_outage_is_absorbed_by_the_open_connection() {
    // 1.5 s: shorter than every timeout, so QUIC retransmits and the relay client reconnects
    // underneath the protocol (observed: still one connection attempt).
    push_across_relay_outage(Duration::from_millis(1500), Duration::from_secs(60));
}

#[test]
fn long_relay_outage_reconnects_and_the_push_resumes() {
    // 8 s with a 3 s protocol stall timeout: the attempt fails, retries fail while the relay is
    // down, then a fresh connection resumes the upload.
    let attempts = push_across_relay_outage(Duration::from_secs(8), Duration::from_secs(3));
    assert!(
        attempts >= 2,
        "expected a reconnect, got {attempts} attempt(s)"
    );
}

/// Pushes a large repo through a UDP proxy, changing the proxy's source address `changes`
/// times mid-transfer. Returns (attempts, duration, final status).
fn push_through_proxy(changes: u32) -> (u32, Duration, rustgit_transport_iroh::Status) {
    // No relay at all: the client reaches the host only through the proxy, so the host sees
    // the proxy's socket as the client's address. Changing that source port is what a NAT
    // mapping change looks like to the host. QUIC must either migrate the path or the protocol
    // must reconnect and resume; either way the repo must arrive intact.
    let tmp = tempfile::tempdir().unwrap();
    let host = start_host(&tmp.path().join("hosted"), &loopback(), true);
    let proxy = UdpProxy::new(host.sockets[0]);
    let url = format!("iroh://{}?addr=127.0.0.1:{}", host.id, proxy.port);
    let mut a = sample_repo(tmp.path(), "a", 40);
    let t = transport(&url, loopback());

    let (connected_tx, connected_rx) = std::sync::mpsc::channel::<()>();
    let (out, attempts, took) = std::thread::scope(|s| {
        let worker = s.spawn(|| {
            let mut c = client_cfg("dev-a");
            c.max_retries = 10;
            c.io_timeout = Duration::from_secs(10);
            let started = Instant::now();
            let mut attempts = 0u32;
            let out =
                rustgit_protocol::with_retries(&c, &rustgit_protocol::CancelToken::new(), || {
                    attempts += 1;
                    let mut cl = rustgit_protocol::Client::connect(
                        &t,
                        &c,
                        rustgit_protocol::CancelToken::new(),
                    )?;
                    let _ = connected_tx.send(());
                    let o = cl.push(&mut a, &push_opts());
                    cl.close();
                    o
                });
            (out, attempts, started.elapsed())
        });
        connected_rx.recv().unwrap();
        for _ in 0..changes {
            std::thread::sleep(Duration::from_millis(400));
            proxy.rebind();
        }
        worker.join().unwrap()
    });
    assert_eq!(proxy.rebinds.load(Ordering::SeqCst), changes);
    assert!(matches!(
        out.expect("push survives"),
        rustgit_protocol::PushOutcome::Pushed { .. }
    ));
    let dest = tmp.path().join("b");
    let b = clone_to(&t, &url, &dest);
    assert!(b.head_commit().unwrap().is_some());
    assert_same_tree(&a.root, &dest);
    let st = t.status();
    host.running.stop();
    (attempts, took, st)
}

#[test]
fn nat_rebinding_mid_transfer_is_survived() {
    let (a0, t0, _) = push_through_proxy(0);
    let (a2, t2, st) = push_through_proxy(2);
    eprintln!(
        "through the UDP proxy: baseline {a0} attempt(s) {t0:?}; with 2 source-address changes {a2} attempt(s) {t2:?}; final {st:?}"
    );
    assert!(
        t2 > Duration::from_millis(800),
        "the transfer must have overlapped the changes"
    );
}

#[test]
fn relay_access_token_is_enforced_and_presented() {
    let relay = TestRelay::https_with_token("correct-horse");
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = relay_cfg(&relay);
    cfg.direct = false;
    cfg.connect_timeout = Some(Duration::from_secs(5));

    // Without the token the host cannot even register with the relay.
    let l = rustgit_transport_iroh::IrohListener::bind(&cfg, None).unwrap();
    assert!(
        !l.wait_online(Duration::from_secs(3)),
        "relay admitted a client without a token"
    );
    drop(l);
    // A wrong token is refused as well.
    let mut wrong = cfg.clone();
    wrong.relay_token = Some("nope".into());
    let l = rustgit_transport_iroh::IrohListener::bind(&wrong, None).unwrap();
    assert!(
        !l.wait_online(Duration::from_secs(3)),
        "relay admitted a wrong token"
    );
    drop(l);

    // With the token a full transfer works through it.
    cfg.relay_token = Some("correct-horse".into());
    let host = start_host(&tmp.path().join("hosted"), &cfg, false);
    let mut a = sample_repo(tmp.path(), "a", 1);
    let t = transport(&host.join_url, cfg.clone());
    push(&t, &mut a, "dev-a");
    assert_eq!(t.status().state, PathState::Relayed);
    let dest = tmp.path().join("b");
    clone_to(&t, &host.join_url, &dest);
    assert_same_tree(&a.root, &dest);
    host.running.stop();
}
