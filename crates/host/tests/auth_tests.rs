//! Device authorization against a real host, in process but over real loopback TCP and the
//! real secure channel: enrollment, per-request role enforcement, revocation of *open*
//! sessions, host-side invitation checks, tampering, and handshake flooding.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustgit_host::{
    ensure_host_identity, spawn, AuthStore, DeviceAuthorizer, Host, HostConfig, KeyStorage,
    RunningHost,
};
use rustgit_protocol::secure::connect_secure;
use rustgit_protocol::transport::TcpDuplex;
use rustgit_protocol::*;

struct Env {
    _tmp: tempfile::TempDir,
    store: Arc<AuthStore>,
    host_key: Fingerprint,
}

fn start(cfg: HostConfig) -> (Env, RunningHost) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hosted");
    Host::init(&dir).unwrap();
    let store = Arc::new(AuthStore::open(&dir).unwrap());
    let ks = KeyStorage::File(tmp.path().join("keys"));
    let (key, _) = ensure_host_identity("test-project", &ks, &store).unwrap();
    let host_key = key.fingerprint();
    let auth = DeviceAuthorizer::new(store.clone(), key, vec![]);
    let host = Host::open(&dir, cfg, Arc::new(auth)).unwrap();
    let running = spawn(host, Box::new(TcpListenerAdapter::bind("127.0.0.1:0").unwrap()));
    (Env { _tmp: tmp, store, host_key }, running)
}

fn now() -> u64 {
    rustgit_host::authz::unix_now()
}

fn cfg(env: &Env, me: &Arc<DeviceKey>) -> ClientConfig {
    ClientConfig::new("test-device").secure(me.clone(), env.host_key)
}

fn enroll(env: &Env, addr: &str, role: Role) -> Arc<DeviceKey> {
    let me = Arc::new(DeviceKey::generate());
    let (id, secret) = env.store.create_invitation(role, 600, 1, "", now()).unwrap();
    let c = cfg(env, &me).enrolling(id, *secret.expose());
    let client = Client::connect(&TcpTransport::new(addr), &c, CancelToken::new()).unwrap();
    assert_eq!(client.hello.role, Some(role));
    client.close();
    me
}

fn connect(env: &Env, addr: &str, me: &Arc<DeviceKey>) -> Result<Client> {
    Client::connect(&TcpTransport::new(addr), &cfg(env, me), CancelToken::new())
}

fn code_of(e: &Error) -> ErrorCode {
    e.code().unwrap_or_else(|| panic!("expected a structured error, got {e:?}"))
}

#[test]
fn revoking_a_device_ends_its_open_sessions() {
    let (env, host) = start(HostConfig::default());
    let me = enroll(&env, &host.addr, Role::Read);

    // an idle connection and a blocked long-poll, both open when the revocation happens
    let mut idle = connect(&env, &host.addr, &me).unwrap();
    idle.list_refs("heads/").unwrap();
    let mut watching = connect(&env, &host.addr, &me).unwrap();
    let seq = watching.hello.ref_seq;
    let t0 = Instant::now();
    let h = std::thread::spawn(move || {
        let r = watching.watch(seq, Duration::from_secs(30));
        (r, t0.elapsed())
    });
    std::thread::sleep(Duration::from_millis(400));
    env.store.revoke_device("test-device", now()).ok(); // by name (set at enrollment)
    let fp = me.fingerprint().to_hex();
    env.store.revoke_device(&fp[..12], now()).unwrap();

    // the blocked long-poll fails within a couple of seconds, not after 30
    let (r, took) = h.join().unwrap();
    let e = r.unwrap_err();
    assert_eq!(code_of(&e), ErrorCode::DeviceRevoked, "{e:?}");
    assert!(took < Duration::from_secs(5), "watch outlived the revocation by {took:?}");
    // the idle connection's next request is refused, and the connection is closed
    let e = idle.list_refs("heads/").unwrap_err();
    assert_eq!(code_of(&e), ErrorCode::DeviceRevoked, "{e:?}");
    assert!(idle.list_refs("heads/").is_err(), "the host must have closed the session");
    // new connections are refused at the door
    let e = connect(&env, &host.addr, &me).err().unwrap();
    assert_eq!(code_of(&e), ErrorCode::DeviceRevoked);
}

#[test]
fn an_idle_connection_of_a_revoked_device_is_closed_by_the_host() {
    let (env, host) = start(HostConfig::default());
    let me = enroll(&env, &host.addr, Role::Write);
    let mut c = connect(&env, &host.addr, &me).unwrap();
    c.list_refs("heads/").unwrap();
    env.store.revoke_device(&me.fingerprint().to_hex()[..10], now()).unwrap();
    // without sending anything, the host notices within its 1 s polling slice and closes
    std::thread::sleep(Duration::from_millis(2500));
    assert!(c.list_refs("heads/").is_err());
}

#[test]
fn the_host_enforces_roles_per_operation_even_against_a_hand_written_client() {
    let (env, host) = start(HostConfig::default());
    let me = enroll(&env, &host.addr, Role::Read);

    // speak the protocol directly over the secure channel
    let s = TcpStream::connect(&host.addr).unwrap();
    let (io, _) = connect_secure(
        Box::new(TcpDuplex::new(s).unwrap()),
        &me,
        &env.host_key,
        Duration::from_secs(5),
    )
    .unwrap();
    let mut ch = Channel::new(io, Some(Duration::from_secs(5)));
    let hello = Request::Hello(Hello {
        min_version: 1,
        max_version: 1,
        device: "raw".into(),
        project: None,
        credential: vec![],
    });
    ch.send(&hello.encode()).unwrap();
    let Response::HelloOk(ok) = Response::decode(&ch.recv(Some(Duration::from_secs(5))).unwrap()).unwrap() else {
        panic!("no HelloOk")
    };
    assert_eq!(ok.role, Some(Role::Read));
    ch.set_max_frame(ok.limits.max_frame);

    let mut ask = |req: Request| -> Response {
        ch.send(&req.encode()).unwrap();
        Response::decode(&ch.recv(Some(Duration::from_secs(5))).unwrap()).unwrap()
    };
    // reading is fine
    assert!(matches!(ask(Request::ListRefs { prefix: "heads/".into() }), Response::Refs { .. }));
    // every write-side request is refused with Forbidden
    let begin = BeginReq {
        session: [1; 16],
        ref_name: "heads/main".into(),
        expected: None,
        expected_gen: 0,
        new_head: rustgit_store::ObjectId([9; 32]),
        force: false,
        object_count: 1,
        byte_estimate: 1,
    };
    for req in [
        Request::Begin(begin.clone()),
        Request::Put { session: [1; 16], objects: vec![] },
        Request::UpdateRef(UpdateRefReq {
            session: [1; 16],
            ref_name: "heads/main".into(),
            expected: None,
            expected_gen: 0,
            new: Some(rustgit_store::ObjectId([9; 32])),
            force: false,
        }),
    ] {
        let name = req.name();
        match ask(req) {
            Response::Error(w) => assert_eq!(w.code, ErrorCode::Forbidden, "{name}: {w}"),
            r => panic!("{name} was not refused: {r:?}"),
        }
    }
    // a second Hello with an enrollment credential cannot upgrade the connection
    match ask(Request::Hello(Hello {
        min_version: 1,
        max_version: 1,
        device: "raw".into(),
        project: None,
        credential: Credential::Enroll { invitation: [0; 16], secret: [0; 32] }.encode(),
    })) {
        Response::Error(_) => {}
        r => panic!("{r:?}"),
    }
}

#[test]
fn the_host_rejects_expired_used_revoked_and_unknown_invitations_with_distinct_errors() {
    let (env, host) = start(HostConfig::default());
    let t = TcpTransport::new(host.addr.clone());
    let try_enroll = |id: [u8; 16], secret: [u8; 32]| {
        let me = Arc::new(DeviceKey::generate());
        Client::connect(&t, &cfg(&env, &me).enrolling(id, secret), CancelToken::new())
            .err()
            .map(|e| code_of(&e))
    };

    // expired: created in the past with a short lifetime
    let (id, s) = env.store.create_invitation(Role::Read, 10, 1, "", now() - 1000).unwrap();
    assert_eq!(try_enroll(id, *s.expose()), Some(ErrorCode::InvitationExpired));
    // malformed credential bytes
    let me = Arc::new(DeviceKey::generate());
    let mut c = cfg(&env, &me);
    c.credential = vec![1, 2, 3];
    let e = Client::connect(&t, &c, CancelToken::new()).err().unwrap();
    assert_eq!(code_of(&e), ErrorCode::BadRequest);
    // unknown id / wrong secret look the same
    assert_eq!(try_enroll([7; 16], [7; 32]), Some(ErrorCode::InvitationInvalid));
    let (id, s) = env.store.create_invitation(Role::Read, 600, 1, "", now()).unwrap();
    let mut wrong = *s.expose();
    wrong[0] ^= 1;
    assert_eq!(try_enroll(id, wrong), Some(ErrorCode::InvitationInvalid));
    // used, then replayed
    assert_eq!(try_enroll(id, *s.expose()), None);
    assert_eq!(try_enroll(id, *s.expose()), Some(ErrorCode::InvitationUsed));
    // revoked
    let (id, s) = env.store.create_invitation(Role::Read, 600, 1, "", now()).unwrap();
    env.store
        .revoke_invitation(&rustgit_protocol::identity::hex_of(&id), now())
        .unwrap();
    assert_eq!(try_enroll(id, *s.expose()), Some(ErrorCode::InvitationRevoked));
    // exactly one device got in
    assert_eq!(env.store.list_devices().unwrap().len(), 1);
}

#[test]
fn tampering_with_a_live_session_kills_it_and_the_host_keeps_serving() {
    let (env, host) = start(HostConfig::default());
    let me = enroll(&env, &host.addr, Role::Read);

    // a relay that flips one bit in the host->client stream well after the handshake
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let relay_addr = l.local_addr().unwrap().to_string();
    let target = host.addr.clone();
    std::thread::spawn(move || {
        let (c, _) = l.accept().unwrap();
        let s = TcpStream::connect(target).unwrap();
        let (mut cr, mut cw) = (c.try_clone().unwrap(), c);
        let (mut sr, mut sw) = (s.try_clone().unwrap(), s);
        std::thread::spawn(move || {
            let mut b = [0u8; 4096];
            while let Ok(n) = cr.read(&mut b) {
                if n == 0 || sw.write_all(&b[..n]).is_err() {
                    break;
                }
            }
        });
        let mut off = 0usize;
        let mut b = [0u8; 4096];
        while let Ok(n) = sr.read(&mut b) {
            if n == 0 {
                break;
            }
            for x in b[..n].iter_mut() {
                // the host -> client handshake message is 198 bytes; corrupt everything after it
                if off >= 200 {
                    *x ^= 0x40;
                }
                off += 1;
            }
            if cw.write_all(&b[..n]).is_err() {
                break;
            }
        }
    });
    let r = Client::connect(&TcpTransport::new(relay_addr), &cfg(&env, &me), CancelToken::new());
    let failed = match r {
        Err(_) => true,
        Ok(mut c) => (0..3).any(|_| c.list_refs("heads/").is_err()),
    };
    assert!(failed, "a modified ciphertext byte must break the session");
    // the host is unharmed
    let mut ok = connect(&env, &host.addr, &me).unwrap();
    ok.list_refs("heads/").unwrap();
}

#[test]
fn half_open_handshakes_cannot_exhaust_the_host() {
    let cfg = HostConfig {
        max_unauthenticated: 2,
        io_timeout: Duration::from_secs(3),
        ..HostConfig::default()
    };
    let (env, host) = start(cfg);
    let me = enroll(&env, &host.addr, Role::Read);
    // sockets that connect and then say nothing
    let idle: Vec<TcpStream> = (0..6).map(|_| TcpStream::connect(&host.addr).unwrap()).collect();
    std::thread::sleep(Duration::from_millis(400));
    // more than the allowance were dropped immediately
    let mut dropped = 0;
    for mut s in idle.iter().map(|s| s.try_clone().unwrap()) {
        s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut b = [0u8; 1];
        if matches!(s.read(&mut b), Ok(0)) {
            dropped += 1;
        }
    }
    assert!(dropped >= 3, "only {dropped} of 6 idle connections were refused");
    // the stalled ones time out by themselves, then a real device connects normally
    std::thread::sleep(Duration::from_secs(4));
    let mut c = connect(&env, &host.addr, &me).unwrap();
    c.list_refs("heads/").unwrap();
}
