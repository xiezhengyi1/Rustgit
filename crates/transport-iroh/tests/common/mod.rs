//! Shared helpers: a locally started iroh-relay, a byte-recording TCP tap, UDP proxy, repos.
#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use iroh_relay::server::{testing, CertConfig, RelayConfig, Server, ServerConfig, TlsConfig};
use rustgit_core::Repo;
use rustgit_host::{spawn, Host, HostConfig, OpenAccess, RunningHost};
use rustgit_protocol::{
    clone_into, CancelToken, Client, ClientConfig, Listener, PushOptions, PushOutcome, Transport,
};
use rustgit_transport_iroh::{IrohConfig, IrohListener, IrohTransport, IrohUrl, RelaySetting};

/// A real `iroh-relay` server (the same code as the `iroh-relay` binary) on loopback.
pub struct TestRelay {
    rt: tokio::runtime::Runtime,
    server: Option<Server>,
    pub url: String,
    /// DER of the self-signed certificate (empty for the plain-HTTP relay).
    pub certs: Vec<Vec<u8>>,
    pub port: u16,
}

impl TestRelay {
    /// HTTPS relay with a self-signed certificate valid for 127.0.0.1.
    pub fn https(port: u16) -> TestRelay {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (certs, server_config) = testing::self_signed_tls_certs_and_config();
        let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
        relay.tls = Some(TlsConfig::new(
            (Ipv4Addr::LOCALHOST, port),
            CertConfig::Manual { server_config },
        ));
        let mut cfg = ServerConfig::default();
        cfg.relay = Some(relay);
        let server = rt.block_on(Server::spawn(cfg)).expect("relay starts");
        let addr = server.https_addr().unwrap();
        TestRelay {
            url: format!("https://{addr}"),
            certs: certs.iter().map(|c| c.to_vec()).collect(),
            port: addr.port(),
            server: Some(server),
            rt,
        }
    }

    /// HTTPS relay that admits only clients presenting `token` (`access.shared_token`).
    pub fn https_with_token(token: &str) -> TestRelay {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (certs, server_config) = testing::self_signed_tls_certs_and_config();
        let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
        relay.tls = Some(TlsConfig::new(
            (Ipv4Addr::LOCALHOST, 0),
            CertConfig::Manual { server_config },
        ));
        relay.access = Arc::new(TokenAccess(token.to_string()));
        let mut cfg = ServerConfig::default();
        cfg.relay = Some(relay);
        let server = rt.block_on(Server::spawn(cfg)).expect("relay starts");
        let addr = server.https_addr().unwrap();
        TestRelay {
            url: format!("https://{addr}"),
            certs: certs.iter().map(|c| c.to_vec()).collect(),
            port: addr.port(),
            server: Some(server),
            rt,
        }
    }

    /// Plain-HTTP relay (what `iroh-relay --dev` runs): lets a test tap the relay's wire.
    pub fn http(port: u16) -> TestRelay {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let relay = RelayConfig::new((Ipv4Addr::LOCALHOST, port));
        let mut cfg = ServerConfig::default();
        cfg.relay = Some(relay);
        let server = rt.block_on(Server::spawn(cfg)).expect("relay starts");
        let addr = server.http_addr().unwrap();
        TestRelay {
            url: format!("http://{addr}"),
            certs: Vec::new(),
            port: addr.port(),
            server: Some(server),
            rt,
        }
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.server.take() {
            let _ = self.rt.block_on(s.shutdown());
        }
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
struct TokenAccess(String);

impl iroh_relay::server::AccessControl for TokenAccess {
    async fn on_connect(
        &self,
        req: &iroh_relay::server::ClientRequest,
    ) -> iroh_relay::server::Access {
        match req.auth_token() {
            Some(t) if t == self.0 => iroh_relay::server::Access::Allow,
            _ => iroh_relay::server::Access::Deny { reason: None },
        }
    }
}

pub fn relay_cfg(relay: &TestRelay) -> IrohConfig {
    IrohConfig {
        relay: RelaySetting::Custom(vec![relay.url.clone()]),
        relay_ca: relay.certs.clone(),
        ..IrohConfig::default()
    }
}

/// Records every byte that passes through a TCP connection, in both directions.
pub struct TcpTap {
    pub port: u16,
    pub to_server: Arc<Mutex<Vec<u8>>>,
    pub to_client: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TcpTap {
    pub fn new(target: SocketAddr) -> TcpTap {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let port = l.local_addr().unwrap().port();
        let to_server = Arc::new(Mutex::new(Vec::new()));
        let to_client = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (ts, tc, st) = (to_server.clone(), to_client.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut pipes = Vec::new();
            while !st.load(Ordering::SeqCst) {
                match l.accept() {
                    Ok((c, _)) => {
                        c.set_nonblocking(false).unwrap();
                        let s = TcpStream::connect(target).unwrap();
                        for (mut from, mut to, log) in [
                            (c.try_clone().unwrap(), s.try_clone().unwrap(), ts.clone()),
                            (s, c, tc.clone()),
                        ] {
                            pipes.push(std::thread::spawn(move || {
                                let mut buf = vec![0u8; 64 * 1024];
                                while let Ok(n) = from.read(&mut buf) {
                                    if n == 0 {
                                        break;
                                    }
                                    log.lock().unwrap().extend_from_slice(&buf[..n]);
                                    if to.write_all(&buf[..n]).is_err() {
                                        break;
                                    }
                                }
                                let _ = to.shutdown(std::net::Shutdown::Both);
                            }));
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        TcpTap {
            port,
            to_server,
            to_client,
            stop,
            thread: Some(thread),
        }
    }

    pub fn all(&self) -> Vec<u8> {
        let mut v = self.to_server.lock().unwrap().clone();
        v.extend_from_slice(&self.to_client.lock().unwrap());
        v
    }
}

impl Drop for TcpTap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Deterministic incompressible bytes (xorshift), so compression cannot hide a marker.
pub fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len + 8);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

pub fn new_repo(parent: &Path, name: &str) -> Repo {
    let p = parent.join(name);
    fs::create_dir_all(&p).unwrap();
    Repo::init(&p, "main").unwrap()
}

pub fn write(repo: &Repo, rel: &str, data: &[u8]) {
    let p = repo.root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

pub fn push_opts() -> PushOptions {
    PushOptions {
        branch: "main".into(),
        remote_branch: "main".into(),
        remote: "origin".into(),
        force: false,
    }
}

/// A hosted repository served over Iroh.
pub struct IrohHost {
    pub running: RunningHost,
    pub id: String,
    pub dir: PathBuf,
    pub join_url: String,
    pub sockets: Vec<SocketAddr>,
}

pub fn start_host(dir: &Path, cfg: &IrohConfig, include_addrs: bool) -> IrohHost {
    if !Host::is_hosted(dir) {
        Host::init(dir).unwrap();
    }
    let host = Host::open(dir, HostConfig::default(), Arc::new(OpenAccess)).unwrap();
    let l = IrohListener::bind(cfg, None).expect("iroh listener");
    // With a relay configured, wait until the endpoint is registered so the URL is complete.
    if !matches!(cfg.relay, RelaySetting::None) {
        assert!(
            l.wait_online(Duration::from_secs(20)),
            "endpoint never got online"
        );
    }
    let join_url = l.join_url(include_addrs);
    let id = l.endpoint_id();
    let sockets = l.bound_sockets();
    let running = spawn(host, Box::new(l) as Box<dyn Listener>);
    IrohHost {
        running,
        id,
        dir: dir.to_path_buf(),
        join_url,
        sockets,
    }
}

pub fn client_cfg(device: &str) -> ClientConfig {
    let mut c = ClientConfig::new(device);
    c.connect_timeout = Duration::from_secs(20);
    c.io_timeout = Duration::from_secs(60);
    c
}

pub fn transport(url: &str, cfg: IrohConfig) -> IrohTransport {
    IrohTransport::new(url.parse::<IrohUrl>().unwrap(), cfg).unwrap()
}

/// Pushes `repo`'s main branch through `t`, returning the new head.
pub fn push(t: &dyn Transport, repo: &mut Repo, device: &str) -> rustgit_store::ObjectId {
    let mut c = Client::connect(t, &client_cfg(device), CancelToken::new()).expect("connect");
    let out = c.push(repo, &push_opts()).expect("push");
    c.close();
    match out {
        PushOutcome::Pushed { new_head, .. } => new_head,
        o => panic!("unexpected push outcome {o:?}"),
    }
}

pub fn clone_to(t: &dyn Transport, url: &str, dest: &Path) -> Repo {
    clone_into(
        t,
        url,
        &client_cfg("cloner"),
        dest,
        "origin",
        CancelToken::new(),
    )
    .expect("clone")
}

/// Every file (relative path -> bytes) under `root`, ignoring `.rgit`.
pub fn tree(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.file_name().is_some_and(|n| n == ".rgit") {
                continue;
            }
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    let mut m = std::collections::BTreeMap::new();
    walk(root, root, &mut m);
    m
}

pub fn assert_same_tree(a: &Path, b: &Path) {
    let (ta, tb) = (tree(a), tree(b));
    assert_eq!(ta.keys().collect::<Vec<_>>(), tb.keys().collect::<Vec<_>>());
    for (k, v) in &ta {
        assert!(v == &tb[k], "content of {k} differs");
    }
}

/// UDP forwarder between a client and a host. The host sees the proxy's "back" socket as the
/// client's address, so [`UdpProxy::rebind`] (a new back socket) is exactly what a NAT
/// rebinding looks like to the host: the same client suddenly arrives from another ip:port.
/// [`UdpProxy::blackhole`] drops everything (a dead path).
pub struct UdpProxy {
    pub port: u16,
    pub blackhole: Arc<AtomicBool>,
    rebind: Arc<AtomicBool>,
    pub rebinds: Arc<std::sync::atomic::AtomicU32>,
    stop: Arc<AtomicBool>,
}

impl UdpProxy {
    pub fn new(target: SocketAddr) -> UdpProxy {
        let front = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        front
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let port = front.local_addr().unwrap().port();
        let blackhole = Arc::new(AtomicBool::new(false));
        let rebind = Arc::new(AtomicBool::new(false));
        let rebinds = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let new_back = || {
            let b = UdpSocket::bind("127.0.0.1:0").unwrap();
            b.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            Arc::new(b)
        };
        let back = Arc::new(Mutex::new(new_back()));
        let client: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));

        // client -> host
        {
            let (front, back, client, bh, rb, rbs, st) = (
                front.clone(),
                back.clone(),
                client.clone(),
                blackhole.clone(),
                rebind.clone(),
                rebinds.clone(),
                stop.clone(),
            );
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                while !st.load(Ordering::SeqCst) {
                    if rb.swap(false, Ordering::SeqCst) {
                        *back.lock().unwrap() = new_back();
                        rbs.fetch_add(1, Ordering::SeqCst);
                    }
                    if let Ok((n, from)) = front.recv_from(&mut buf) {
                        *client.lock().unwrap() = Some(from);
                        if !bh.load(Ordering::SeqCst) {
                            let b = back.lock().unwrap().clone();
                            let _ = b.send_to(&buf[..n], target);
                        }
                    }
                }
            });
        }
        // host -> client (follows the current back socket)
        {
            let (front, back, client, bh, st) = (
                front.clone(),
                back.clone(),
                client.clone(),
                blackhole.clone(),
                stop.clone(),
            );
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                while !st.load(Ordering::SeqCst) {
                    let b = back.lock().unwrap().clone();
                    if let Ok((n, _)) = b.recv_from(&mut buf) {
                        let c = *client.lock().unwrap();
                        if let (Some(c), false) = (c, bh.load(Ordering::SeqCst)) {
                            let _ = front.send_to(&buf[..n], c);
                        }
                    }
                }
            });
        }
        UdpProxy {
            port,
            blackhole,
            rebind,
            rebinds,
            stop,
        }
    }

    /// The next packet towards the host leaves from a new source port.
    pub fn rebind(&self) {
        self.rebind.store(true, Ordering::SeqCst);
    }
}

impl Drop for UdpProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
