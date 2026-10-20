//! In-process protocol tests: real TCP on loopback, real host, real repositories.
//! Process-level scenarios (crash points, concurrent processes) live in
//! crates/cli/tests/sync_processes.rs.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use rustgit_core::Repo;
use rustgit_host::{spawn, Host, HostConfig, OpenAccess, RunningHost};
use rustgit_protocol::*;
use rustgit_store::{hash_object, ObjectId, ObjectType};

struct Env {
    _tmp: tempfile::TempDir,
    host_dir: std::path::PathBuf,
}

fn start(cfg: HostConfig) -> (Env, RunningHost) {
    let tmp = tempfile::tempdir().unwrap();
    let host_dir = tmp.path().join("hosted");
    Host::init(&host_dir).unwrap();
    let host = Host::open(&host_dir, cfg, Arc::new(OpenAccess)).unwrap();
    let l = TcpListenerAdapter::bind("127.0.0.1:0").unwrap();
    let running = spawn(host, Box::new(l));
    (Env { _tmp: tmp, host_dir }, running)
}

fn new_repo(parent: &Path, name: &str) -> Repo {
    let p = parent.join(name);
    fs::create_dir_all(&p).unwrap();
    Repo::init(&p, "main").unwrap()
}

fn write(repo: &Repo, rel: &str, data: &[u8]) {
    let p = repo.root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn connect(addr: &str, device: &str) -> Client {
    let t = TcpTransport::new(addr);
    Client::connect(&t, &ClientConfig::new(device), CancelToken::new()).unwrap()
}

fn push_opts() -> PushOptions {
    PushOptions {
        branch: "main".into(),
        remote_branch: "main".into(),
        remote: "origin".into(),
        force: false,
    }
}

#[test]
fn push_clone_fetch_roundtrip() {
    let (env, host) = start(HostConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    write(&a, "src/main.rs", b"fn main() {}\n");
    write(&a, "big.bin", &vec![7u8; 3 * 1024 * 1024]);
    let c1 = a.commit("first", &[]).unwrap();

    let mut cl = connect(&host.addr, "dev-a");
    match cl.push(&mut a, &push_opts()).unwrap() {
        PushOutcome::Pushed { new_head, generation, .. } => {
            assert_eq!(new_head, c1);
            assert_eq!(generation, 1);
        }
        o => panic!("{o:?}"),
    }
    assert!(cl.stats.objects_transferred >= 4);
    // Pushing again is a no-op.
    assert!(matches!(cl.push(&mut a, &push_opts()).unwrap(), PushOutcome::UpToDate));
    cl.close();

    // The hosted repository holds the commit and no working files.
    {
        let h = host.host.repo();
        assert_eq!(h.get_ref("heads/main").unwrap().unwrap().commit, Some(c1));
    }
    assert!(!env.host_dir.join("src").exists());

    // Clone it.
    let t = TcpTransport::new(&host.addr);
    let dest = tmp.path().join("b");
    let mut b = clone_into(
        &t,
        &format!("tcp://{}", host.addr),
        &ClientConfig::new("dev-b"),
        &dest,
        "origin",
        CancelToken::new(),
    )
    .unwrap();
    assert_eq!(fs::read(dest.join("src/main.rs")).unwrap(), b"fn main() {}\n");
    assert_eq!(fs::read(dest.join("big.bin")).unwrap().len(), 3 * 1024 * 1024);
    assert_eq!(b.head_commit().unwrap(), Some(c1));

    // Second commit on b, pushed; a fetches and fast-forwards.
    write(&b, "src/main.rs", b"fn main() { println!(); }\n");
    let c2 = b.commit("second", &[]).unwrap();
    let mut cl = connect(&host.addr, "dev-b");
    let before = cl.wire_bytes();
    assert!(matches!(cl.push(&mut b, &push_opts()).unwrap(), PushOutcome::Pushed { .. }));
    let sent = cl.wire_bytes().0 - before.0;
    // The 3 MiB file is unchanged and must not travel again.
    assert!(sent < 20_000, "incremental push sent {sent} bytes");
    cl.close();

    let mut cl = connect(&host.addr, "dev-a");
    let out = cl.fetch(&mut a, "origin").unwrap();
    assert_eq!(out.updated.len(), 1);
    assert_eq!(a.get_ref("remotes/origin/main").unwrap().unwrap().commit, Some(c2));
    let r = a.merge("origin/main").unwrap();
    assert!(r.fast_forward);
    assert_eq!(fs::read(a.root.join("src/main.rs")).unwrap(), b"fn main() { println!(); }\n");
    cl.close();
    host.stop();
}

#[test]
fn stale_push_is_rejected_and_keeps_local_commits() {
    let (_env, host) = start(HostConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    write(&a, "f.txt", b"base\n");
    a.commit("base", &[]).unwrap();
    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();
    cl.close();

    let t = TcpTransport::new(&host.addr);
    let mut b = clone_into(
        &t,
        &format!("tcp://{}", host.addr),
        &ClientConfig::new("b"),
        &tmp.path().join("b"),
        "origin",
        CancelToken::new(),
    )
    .unwrap();

    write(&a, "f.txt", b"from a\n");
    let ca = a.commit("a edit", &[]).unwrap();
    write(&b, "g.txt", b"from b\n");
    let cb = b.commit("b edit", &[]).unwrap();

    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();
    cl.close();

    let mut cl = connect(&host.addr, "b");
    let err = cl.push(&mut b, &push_opts()).unwrap_err();
    assert!(matches!(err.code(), Some(ErrorCode::NonFastForward) | Some(ErrorCode::StaleRef)), "{err}");
    assert!(err.to_string().contains("fetch and merge"), "{err}");
    // Local commit survives, working file untouched, host still at a's commit.
    assert_eq!(b.head_commit().unwrap(), Some(cb));
    assert_eq!(fs::read(b.root.join("g.txt")).unwrap(), b"from b\n");
    assert_eq!(host.host.repo().get_ref("heads/main").unwrap().unwrap().commit, Some(ca));

    // Fetch, merge, push succeeds and contains both histories.
    cl.fetch(&mut b, "origin").unwrap();
    let m = b.merge("origin/main").unwrap();
    assert!(m.conflicts.is_empty());
    let merged = b.head_commit().unwrap().unwrap();
    assert!(matches!(cl.push(&mut b, &push_opts()).unwrap(), PushOutcome::Pushed { .. }));
    let h = host.host.repo();
    assert_eq!(h.get_ref("heads/main").unwrap().unwrap().commit, Some(merged));
    assert!(h.is_ancestor(&ca, &merged).unwrap());
    assert!(h.is_ancestor(&cb, &merged).unwrap());
    drop(h);
    host.stop();
}

fn raw(addr: &str) -> (Channel, HelloOk) {
    let io = TcpTransport::new(addr).connect(std::time::Duration::from_secs(5)).unwrap();
    let mut ch = Channel::new(io, Some(std::time::Duration::from_secs(10)));
    ch.send(
        &Request::Hello(Hello {
            min_version: 1,
            max_version: 1,
            device: "raw".into(),
            project: None,
            credential: vec![],
        })
        .encode(),
    )
    .unwrap();
    let Response::HelloOk(ok) = Response::decode(&ch.recv(Some(std::time::Duration::from_secs(5))).unwrap()).unwrap() else {
        panic!("no hello")
    };
    ch.set_max_frame(ok.limits.max_frame);
    (ch, ok)
}

fn rpc(ch: &mut Channel, r: Request) -> Response {
    ch.send(&r.encode()).unwrap();
    Response::decode(&ch.recv(Some(std::time::Duration::from_secs(10))).unwrap()).unwrap()
}

fn expect_err(r: Response, code: ErrorCode) -> WireError {
    match r {
        Response::Error(w) => {
            assert_eq!(w.code, code, "{w}");
            w
        }
        o => panic!("expected {code:?}, got {}", o.name()),
    }
}

#[test]
fn version_negotiation_and_malformed_hello() {
    let (_e, host) = start(HostConfig::default());
    let io = TcpTransport::new(&host.addr).connect(std::time::Duration::from_secs(5)).unwrap();
    let mut ch = Channel::new(io, Some(std::time::Duration::from_secs(10)));
    ch.send(
        &Request::Hello(Hello {
            min_version: 7,
            max_version: 9,
            device: "future".into(),
            project: None,
            credential: vec![],
        })
        .encode(),
    )
    .unwrap();
    let w = expect_err(
        Response::decode(&ch.recv(Some(std::time::Duration::from_secs(5))).unwrap()).unwrap(),
        ErrorCode::VersionMismatch,
    );
    assert_eq!(w.detail, Detail::Versions { min: 1, max: 1 });

    // A request before Hello is refused with a structured error, not a hang or a crash.
    let io = TcpTransport::new(&host.addr).connect(std::time::Duration::from_secs(5)).unwrap();
    let mut ch = Channel::new(io, Some(std::time::Duration::from_secs(10)));
    ch.send(&Request::ListRefs { prefix: "".into() }.encode()).unwrap();
    expect_err(
        Response::decode(&ch.recv(Some(std::time::Duration::from_secs(5))).unwrap()).unwrap(),
        ErrorCode::BadRequest,
    );

    // Wrong project.
    let io = TcpTransport::new(&host.addr).connect(std::time::Duration::from_secs(5)).unwrap();
    let mut ch = Channel::new(io, Some(std::time::Duration::from_secs(10)));
    ch.send(
        &Request::Hello(Hello {
            min_version: 1,
            max_version: 1,
            device: "x".into(),
            project: Some("not-this-one".into()),
            credential: vec![],
        })
        .encode(),
    )
    .unwrap();
    expect_err(
        Response::decode(&ch.recv(Some(std::time::Duration::from_secs(5))).unwrap()).unwrap(),
        ErrorCode::ProjectMismatch,
    );
    host.stop();
}

#[test]
fn corrupt_object_is_rejected_and_nothing_is_staged() {
    let (env, host) = start(HostConfig::default());
    let (mut ch, _) = raw(&host.addr);
    let session = [5u8; 16];
    let data = b"hello world".to_vec();
    let good = hash_object(ObjectType::Chunk, &data);
    let r = rpc(
        &mut ch,
        Request::Begin(BeginReq {
            session,
            ref_name: "heads/main".into(),
            expected: None,
            expected_gen: 0,
            new_head: good,
            force: false,
            object_count: 2,
            byte_estimate: 100,
        }),
    );
    assert!(matches!(r, Response::BeginOk { .. }));
    // Flipped payload byte: id no longer matches.
    let mut bad = data.clone();
    bad[0] ^= 1;
    let w = expect_err(
        rpc(
            &mut ch,
            Request::Put {
                session,
                objects: vec![
                    WireObject { ty: ObjectType::Chunk, id: good, data: data.clone(), base: None },
                    WireObject { ty: ObjectType::Chunk, id: hash_object(ObjectType::Chunk, b"other"), data: bad, base: None },
                ],
            },
        ),
        ErrorCode::CorruptObject,
    );
    assert!(matches!(w.detail, Detail::Ids(ref v) if v.len() == 1));
    // The valid object in the same batch was not kept either.
    let staged = host.host.staging.stat(&session).unwrap();
    assert_eq!(staged, (0, 0));
    // Wrong type claim for a valid hash is also corrupt.
    expect_err(
        rpc(
            &mut ch,
            Request::Put {
                session,
                objects: vec![WireObject { ty: ObjectType::Tree, id: good, data: data.clone(), base: None }],
            },
        ),
        ErrorCode::CorruptObject,
    );
    // A tree-typed object that hashes right but does not decode is refused as invalid.
    let junk = vec![9u8; 10];
    let jid = hash_object(ObjectType::Tree, &junk);
    expect_err(
        rpc(
            &mut ch,
            Request::Put {
                session,
                objects: vec![WireObject { ty: ObjectType::Tree, id: jid, data: junk, base: None }],
            },
        ),
        ErrorCode::InvalidObject,
    );
    // The repository gained nothing.
    assert!(!host.host.repo().store.has(&good).unwrap());
    let _ = env;
    host.stop();
}

#[test]
fn missing_dependencies_block_the_ref_update() {
    let (_e, host) = start(HostConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    write(&a, "f.txt", b"content\n");
    let c = a.commit("c", &[]).unwrap();
    let commit_bytes = a.store.get(&c).unwrap().1;

    let (mut ch, _) = raw(&host.addr);
    let session = [6u8; 16];
    assert!(matches!(
        rpc(&mut ch, Request::Begin(BeginReq {
            session, ref_name: "heads/main".into(), expected: None, expected_gen: 0,
            new_head: c, force: false, object_count: 1, byte_estimate: 10,
        })),
        Response::BeginOk { .. }
    ));
    // Send only the commit; its tree and chunk are missing.
    assert!(matches!(
        rpc(&mut ch, Request::Put { session, objects: vec![WireObject { ty: ObjectType::Commit, id: c, data: commit_bytes, base: None }] }),
        Response::PutAck { .. }
    ));
    let w = expect_err(
        rpc(&mut ch, Request::UpdateRef(UpdateRefReq {
            session, ref_name: "heads/main".into(), expected: None, expected_gen: 0, new: Some(c), force: false,
        })),
        ErrorCode::MissingObjects,
    );
    assert!(matches!(w.detail, Detail::Ids(ref v) if !v.is_empty()));
    // The ref did not move and the half-uploaded commit was not published.
    assert!(host.host.repo().get_ref("heads/main").unwrap().is_none());
    assert!(!host.host.repo().store.has(&c).unwrap());
    host.stop();
}

#[test]
fn update_ref_is_idempotent_and_checks_generation() {
    let (_e, host) = start(HostConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    write(&a, "f.txt", b"1\n");
    a.commit("one", &[]).unwrap();
    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();
    cl.close();

    // Replay an UpdateRef that is already applied: success, flagged, no second generation.
    write(&a, "f.txt", b"2\n");
    let c2 = a.commit("two", &[]).unwrap();
    let c1 = a.get_ref("remotes/origin/main").unwrap().unwrap().commit.unwrap();
    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();
    cl.close();
    let (mut ch, _) = raw(&host.addr);
    let sess = session_id("a", "heads/main", &c2);
    let req = Request::UpdateRef(UpdateRefReq {
        session: sess, ref_name: "heads/main".into(), expected: Some(c1), expected_gen: 1, new: Some(c2), force: false,
    });
    match rpc(&mut ch, req.clone()) {
        Response::RefUpdated { generation: 2, already_applied: true } => {}
        o => panic!("{o:?}"),
    }
    assert_eq!(host.host.repo().get_ref("heads/main").unwrap().unwrap().generation, 2);

    // Same head but a stale generation is a conflict even though the commit matches.
    let r = rpc(&mut ch, Request::UpdateRef(UpdateRefReq {
        session: [1; 16], ref_name: "heads/main".into(), expected: Some(c2), expected_gen: 1, new: Some(c1), force: false,
    }));
    let w = expect_err(r, ErrorCode::StaleRef);
    assert_eq!(w.detail, Detail::Ref { name: "heads/main".into(), current: Some(c2), generation: 2 });
    host.stop();
}

#[test]
fn history_replacement_needs_force_and_force_is_off() {
    let (_e, host) = start(HostConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    write(&a, "f.txt", b"1\n");
    a.commit("one", &[]).unwrap();
    write(&a, "f.txt", b"2\n");
    let c2 = a.commit("two", &[]).unwrap();
    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();

    // An unrelated history built in another repo.
    let mut z = new_repo(tmp.path(), "z");
    write(&z, "other.txt", b"x\n");
    let cz = z.commit("unrelated", &[]).unwrap();
    // Even with the right expected head+generation the host refuses to replace history.
    let (records, _) = cl.list_refs("heads/").unwrap();
    let rec = &records[0];
    assert_eq!(rec.commit, Some(c2));
    let (mut ch, _) = raw(&host.addr);
    let sess = [3u8; 16];
    let mut objs = Vec::new();
    for (ty, id) in rustgit_protocol::plan::enumerate_objects(&z, &[cz]).unwrap() {
        let (_, data) = z.store.get(&id).unwrap();
        objs.push(WireObject { ty, id, data, base: None });
    }
    assert!(matches!(
        rpc(&mut ch, Request::Begin(BeginReq {
            session: sess, ref_name: "heads/main".into(), expected: Some(c2), expected_gen: rec.generation,
            new_head: cz, force: false, object_count: objs.len() as u64, byte_estimate: 100,
        })),
        Response::BeginOk { .. }
    ));
    assert!(matches!(rpc(&mut ch, Request::Put { session: sess, objects: objs }), Response::PutAck { .. }));
    let req = |force| Request::UpdateRef(UpdateRefReq {
        session: sess, ref_name: "heads/main".into(), expected: Some(c2), expected_gen: rec.generation, new: Some(cz), force,
    });
    expect_err(rpc(&mut ch, req(false)), ErrorCode::NonFastForward);
    expect_err(rpc(&mut ch, req(true)), ErrorCode::ForcePushDisabled);
    // Deleting is off by default too.
    expect_err(
        rpc(&mut ch, Request::UpdateRef(UpdateRefReq {
            session: [0; 16], ref_name: "heads/main".into(), expected: Some(c2), expected_gen: rec.generation, new: None, force: false,
        })),
        ErrorCode::DeleteDisabled,
    );
    assert_eq!(host.host.repo().get_ref("heads/main").unwrap().unwrap().commit, Some(c2));
    cl.close();
    host.stop();
}

#[test]
fn force_and_delete_work_when_the_host_allows_them() {
    let cfg = HostConfig { allow_force: true, allow_delete: true, ..HostConfig::default() };
    let (_e, host) = start(cfg);
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    write(&a, "f.txt", b"1\n");
    a.commit("one", &[]).unwrap();
    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();

    let mut z = new_repo(tmp.path(), "z");
    write(&z, "o.txt", b"x\n");
    let cz = z.commit("unrelated", &[]).unwrap();
    let mut cl2 = connect(&host.addr, "z");
    // Without force the client refuses locally (it does not even have the host's commit).
    assert!(cl2.push(&mut z, &push_opts()).is_err());
    let forced = PushOptions { force: true, ..push_opts() };
    cl2.push(&mut z, &forced).unwrap();
    assert_eq!(host.host.repo().get_ref("heads/main").unwrap().unwrap().commit, Some(cz));
    // Tombstone on delete keeps the generation.
    cl2.delete_remote_branch(&mut z, "origin", "main").unwrap();
    let rec = host.host.list_refs("heads/").unwrap();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].commit, None);
    assert_eq!(rec[0].generation, 3);
    cl.close();
    cl2.close();
    host.stop();
}

#[test]
fn quotas_limit_uploads() {
    let cfg = HostConfig { max_session_bytes: 1000, ..HostConfig::default() };
    let (_e, host) = start(cfg);
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    write(&a, "big.bin", &vec![1u8; 100_000]);
    a.commit("big", &[]).unwrap();
    let mut cl = connect(&host.addr, "a");
    let err = cl.push(&mut a, &push_opts()).unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::QuotaExceeded), "{err}");
    assert!(host.host.repo().get_ref("heads/main").unwrap().is_none());
    host.stop();
}

#[test]
fn oversized_and_garbage_frames_are_refused() {
    use std::io::{Read, Write};
    let (_e, host) = start(HostConfig::default());
    // Garbage that claims a 2 GiB frame: the host closes without allocating it.
    let mut s = std::net::TcpStream::connect(&host.addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    s.write_all(&[0x7f, 0xff, 0xff, 0xff, 0, 0, 0, 0]).unwrap();
    let mut buf = [0u8; 16];
    let n = s.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "host should just close the connection");
    // A frame with a bad checksum is rejected.
    let mut s = std::net::TcpStream::connect(&host.addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let payload = Request::Bye.encode();
    let mut f = Vec::new();
    f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    f.extend_from_slice(&0u32.to_be_bytes());
    f.extend_from_slice(&payload);
    s.write_all(&f).unwrap();
    assert_eq!(s.read(&mut buf).unwrap_or(0), 0);
    // The host is still serving.
    let (_ch, ok) = raw(&host.addr);
    assert_eq!(ok.version, 1);
    host.stop();
}

#[test]
fn second_host_process_cannot_open_the_same_repository() {
    let (env, host) = start(HostConfig::default());
    let e = Host::open(&env.host_dir, HostConfig::default(), Arc::new(OpenAccess));
    assert!(matches!(e, Err(rustgit_host::Error::Locked(_))));
    host.stop();
    // After the first host is gone the lock is released.
    let again = Host::open(&env.host_dir, HostConfig::default(), Arc::new(OpenAccess));
    assert!(again.is_ok());
}

#[test]
fn idle_connections_time_out() {
    let cfg = HostConfig { idle_timeout: std::time::Duration::from_secs(2), ..HostConfig::default() };
    let (_e, host) = start(cfg);
    let (mut ch, _) = raw(&host.addr);
    // Say nothing; the host should send a Timeout error and close.
    let r = Response::decode(&ch.recv(Some(std::time::Duration::from_secs(8))).unwrap()).unwrap();
    expect_err(r, ErrorCode::Timeout);
    host.stop();
}

#[test]
fn watch_wakes_on_ref_change() {
    let (_e, host) = start(HostConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    write(&a, "f.txt", b"1\n");
    a.commit("one", &[]).unwrap();
    let mut watcher = connect(&host.addr, "watcher");
    let seq = watcher.hello.ref_seq;
    let addr = host.addr.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut c = connect(&addr, "pusher");
        c.push(&mut a, &push_opts()).unwrap();
        c.close();
    });
    let t0 = std::time::Instant::now();
    let (new_seq, changed) = watcher.watch(seq, std::time::Duration::from_secs(20)).unwrap();
    assert!(changed && new_seq > seq);
    assert!(t0.elapsed() < std::time::Duration::from_secs(10));
    // And a quiet watch returns unchanged after the timeout.
    let (_, changed) = watcher.watch(new_seq, std::time::Duration::from_millis(300)).unwrap();
    assert!(!changed);
    h.join().unwrap();
    host.stop();
}

#[allow(dead_code)]
fn _unused(_: ObjectId) {}

// ---- cancellation, timeouts, backpressure, limits, hostile peers --------------------------

fn big_repo(parent: &Path, files: usize) -> Repo {
    let mut a = new_repo(parent, "big");
    let mut x = 0x1234_5678_9abc_def1u64;
    for i in 0..files {
        let mut v = Vec::new();
        while v.len() < 1_500_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.extend_from_slice(&x.to_le_bytes());
        }
        write(&a, &format!("f{i}.bin"), &v);
    }
    a.commit("big", &[]).unwrap();
    a
}

#[test]
fn cancelled_upload_stops_between_batches_and_resumes_later() {
    let (_e, host) = start(HostConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut a = big_repo(tmp.path(), 8);
    let mut cl = connect(&host.addr, "a");
    let token = cl.cancel.clone();
    cl.on_progress(move |p| {
        if p.phase == "uploading" && p.done > 0 {
            token.cancel();
        }
    });
    let err = cl.push(&mut a, &push_opts()).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert!(host.host.repo().get_ref("heads/main").unwrap().is_none());
    let sent_first = cl.stats.objects_transferred;
    assert!(sent_first > 0);
    cl.close();

    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();
    assert_eq!(cl.stats.objects_already_staged, sent_first, "confirmed objects are not resent");
    assert!(host.host.repo().get_ref("heads/main").unwrap().is_some());
    cl.close();
    host.stop();
}

#[test]
fn unresponsive_host_times_out() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let h = std::thread::spawn(move || {
        let (_s, _) = l.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(4));
    });
    let mut cfg = ClientConfig::new("x");
    cfg.io_timeout = std::time::Duration::from_millis(500);
    let t0 = std::time::Instant::now();
    let r = Client::connect(&TcpTransport::new(&addr), &cfg, CancelToken::new());
    assert!(matches!(r, Err(Error::Timeout)), "{:?}", r.err());
    assert!(t0.elapsed() < std::time::Duration::from_secs(3));
    h.join().unwrap();
}

#[test]
fn batches_are_bounded_and_get_returns_partial_results() {
    let mut cfg = HostConfig::default();
    cfg.limits.max_batch_objects = 2;
    let (_e, host) = start(cfg);
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    for i in 0..5 {
        write(&a, &format!("f{i}.txt"), format!("file {i}\n").as_bytes());
    }
    a.commit("c", &[]).unwrap();
    // Push works even though the host only takes two objects per batch.
    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();
    cl.close();

    let (mut ch, ok) = raw(&host.addr);
    assert_eq!(ok.limits.max_batch_objects, 2);
    // A batch over the limit is a protocol error, not a crash or a silent truncation.
    let objs: Vec<WireObject> = (0..3)
        .map(|i| {
            let d = format!("x{i}").into_bytes();
            WireObject { ty: ObjectType::Chunk, id: hash_object(ObjectType::Chunk, &d), data: d, base: None }
        })
        .collect();
    assert!(matches!(
        rpc(&mut ch, Request::Begin(BeginReq {
            session: [8; 16], ref_name: "heads/other".into(), expected: None, expected_gen: 0,
            new_head: objs[0].id, force: false, object_count: 3, byte_estimate: 6,
        })),
        Response::BeginOk { .. }
    ));
    expect_err(rpc(&mut ch, Request::Put { session: [8; 16], objects: objs }), ErrorCode::BadRequest);
    // Get of 5 existing ids returns at most 2 and says how many remain.
    let ids: Vec<ObjectId> = host.host.repo().store.ids().unwrap().into_iter().take(5).collect();
    assert_eq!(ids.len(), 5);
    match rpc(&mut ch, Request::Get { ids, allow_delta: false }) {
        Response::Objects { objects, remaining } => {
            assert_eq!(objects.len(), 2);
            assert_eq!(remaining, 3);
        }
        o => panic!("{}", o.name()),
    }
    host.stop();
}

#[test]
fn per_device_connection_limit_is_enforced() {
    let cfg = HostConfig { max_connections_per_device: 2, ..HostConfig::default() };
    let (_e, host) = start(cfg);
    let c1 = connect(&host.addr, "same");
    let c2 = connect(&host.addr, "same");
    let t = TcpTransport::new(&host.addr);
    let r = Client::connect(&t, &ClientConfig::new("same"), CancelToken::new());
    match r {
        Err(Error::Remote(w)) => {
            assert_eq!(w.code, ErrorCode::Busy);
            assert!(w.retryable);
        }
        _ => panic!("third connection from the same device should be refused as busy"),
    }
    // Another device is unaffected.
    let _other = connect(&host.addr, "different");
    drop((c1, c2));
    host.stop();
}

/// A host that lies: serves a real commit's objects but flips a byte in one of them.
#[test]
fn corrupt_objects_from_the_host_are_rejected_by_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut src = new_repo(tmp.path(), "src");
    write(&src, "f.txt", b"honest content\n");
    let head = src.commit("c", &[]).unwrap();
    let objs = rustgit_protocol::plan::enumerate_objects(&src, &[head]).unwrap();
    let ids: Vec<ObjectId> = objs.iter().map(|(_, i)| *i).collect();

    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let objs2 = objs.clone();
    let src_objects: Vec<WireObject> = objs
        .iter()
        .map(|(ty, id)| {
            let (_, mut data) = src.store.get(id).unwrap();
            if *ty == ObjectType::Chunk {
                data[0] ^= 0xff; // the lie
            }
            WireObject { ty: *ty, id: *id, data, base: None }
        })
        .collect();
    let ids2 = ids.clone();
    let server = std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let io: Box<dyn Duplex> = Box::new(rustgit_protocol::transport::TcpDuplex::new(s).unwrap());
        let mut ch = Channel::new(io, Some(std::time::Duration::from_secs(10)));
        loop {
            let Ok(p) = ch.recv(Some(std::time::Duration::from_secs(10))) else { return };
            let resp = match Request::decode(&p).unwrap() {
                Request::Hello(_) => Response::HelloOk(HelloOk {
                    version: 1,
                    repo_uuid: "evil".into(),
                    limits: Limits::default(),
                    policy: Policy { allow_force: false, allow_delete: false, default_branch: "main".into() },
                    ref_seq: 1,
                    role: None,
                }),
                Request::ListRefs { .. } => Response::Refs {
                    records: vec![RefRecord { name: "heads/main".into(), commit: Some(head), generation: 1 }],
                    seq: 1,
                },
                Request::Plan { .. } => Response::PlanOk { objects: ids2.len() as u64, commits: 1 },
                Request::PlanPage { .. } => Response::PlanPage { ids: ids2.clone(), end: true },
                Request::Get { .. } => Response::Objects { objects: src_objects.clone(), remaining: 0 },
                Request::Bye => return,
                _ => return,
            };
            ch.send(&resp.encode()).unwrap();
            let _ = &objs2;
        }
    });
    let mut dst = new_repo(tmp.path(), "dst");
    let mut cl = Client::connect(&TcpTransport::new(&addr), &ClientConfig::new("v"), CancelToken::new()).unwrap();
    let err = cl.fetch(&mut dst, "origin").unwrap_err();
    assert!(matches!(err, Error::Verification(_)), "{err}");
    // Nothing from the lying host entered the store and no tracking ref moved.
    assert!(dst.get_ref("remotes/origin/main").unwrap().is_none());
    for id in &ids {
        assert!(!dst.store.has(id).unwrap() || dst.store.object_type(id).unwrap() != Some(ObjectType::Chunk));
    }
    let _ = server.join();
}

// ---- delta encoding on the wire -----------------------------------------------------------

fn text_blob(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut s = String::new();
    while s.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.push_str(&format!("line {} value {}\n", x % 100_000, x % 977));
    }
    s.into_bytes()
}

#[test]
fn edits_travel_as_deltas_in_both_directions() {
    let (_e, host) = start(HostConfig::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut a = new_repo(tmp.path(), "a");
    let mut body = text_blob(300_000, 7);
    write(&a, "doc.txt", &body);
    a.commit("v0", &[]).unwrap();
    for v in 1..=5 {
        let at = 1000 * v;
        body[at] = b'#';
        write(&a, "doc.txt", &body);
        a.commit(&format!("v{v}"), &[]).unwrap();
    }
    let mut cl = connect(&host.addr, "a");
    cl.push(&mut a, &push_opts()).unwrap();
    let up = cl.stats.clone();
    cl.close();
    // Six versions of a 300 KB file: the five later ones are deltas of a few hundred bytes.
    assert!(up.objects_as_delta >= 5, "{up:?}");
    assert!(up.wire_bytes_sent < 150_000, "uploaded {} bytes", up.wire_bytes_sent);
    // The host holds exactly the committed content (deltas are only an encoding).
    assert!(host.host.repo().verify().unwrap().problems.is_empty());

    // Fetch direction.
    let mut b = new_repo(tmp.path(), "b");
    let mut cl = connect(&host.addr, "b");
    cl.fetch(&mut b, "origin").unwrap();
    let down = cl.stats.clone();
    assert!(down.objects_as_delta >= 5, "{down:?}");
    assert!(down.wire_bytes_received < 150_000, "downloaded {}", down.wire_bytes_received);
    b.merge("origin/main").unwrap();
    assert_eq!(fs::read(b.root.join("doc.txt")).unwrap(), body);
    cl.close();
    host.stop();
}

#[test]
fn delta_with_unknown_base_is_refused_and_wrong_delta_is_corrupt() {
    let (_e, host) = start(HostConfig::default());
    let (mut ch, _) = raw(&host.addr);
    let session = [4u8; 16];
    let base_bytes = text_blob(5000, 3);
    let mut new_bytes = base_bytes.clone();
    new_bytes[100] = b'!';
    let bid = hash_object(ObjectType::Chunk, &base_bytes);
    let nid = hash_object(ObjectType::Chunk, &new_bytes);
    assert!(matches!(
        rpc(&mut ch, Request::Begin(BeginReq {
            session, ref_name: "heads/main".into(), expected: None, expected_gen: 0,
            new_head: nid, force: false, object_count: 2, byte_estimate: 20_000,
        })),
        Response::BeginOk { .. }
    ));
    let delta = WireObject::delta(ObjectType::Chunk, nid, &new_bytes, bid, &base_bytes).unwrap();
    // Base not on the host yet.
    let w = expect_err(rpc(&mut ch, Request::Put { session, objects: vec![delta.clone()] }), ErrorCode::UnknownBase);
    assert_eq!(w.detail, Detail::Ids(vec![bid]));
    // Base then delta in one batch resolves.
    let full_base = WireObject::full(ObjectType::Chunk, bid, base_bytes.clone());
    assert!(matches!(
        rpc(&mut ch, Request::Put { session, objects: vec![full_base, delta.clone()] }),
        Response::PutAck { accepted: 2, .. }
    ));
    assert_eq!(host.host.staging.read(&session, &nid).unwrap().unwrap().1, new_bytes);
    // A delta that reconstructs to something other than the claimed id is corrupt.
    let mut lie = delta.clone();
    lie.id = hash_object(ObjectType::Chunk, b"a different object");
    expect_err(rpc(&mut ch, Request::Put { session, objects: vec![lie] }), ErrorCode::CorruptObject);
    host.stop();
}

// ---- completeness cache vs garbage collection ---------------------------------------------

/// Reproduces the hazard behind `maintenance.epoch`: gc works pack by pack, so an
/// unreachable tree can survive in a recent pack while the chunk it names is dropped from an
/// old one. If the host still believed that tree was "complete", a pusher would be told to skip
/// it and a ref could end up pointing at history with a hole in it.
#[test]
fn gc_does_not_leave_the_host_believing_a_holey_tree_is_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let host_dir = tmp.path().join("hosted");
    Host::init(&host_dir).unwrap();
    let cfg = HostConfig { allow_delete: true, ..HostConfig::default() };
    let host = Host::open(&host_dir, cfg.clone(), Arc::new(OpenAccess)).unwrap();
    let running = spawn(host, Box::new(TcpListenerAdapter::bind("127.0.0.1:0").unwrap()));

    let mut a = new_repo(tmp.path(), "a");
    let mut big = Vec::new();
    let mut x = 0xdead_beef_u64;
    while big.len() < 600_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        big.extend_from_slice(&x.to_le_bytes());
    }
    write(&a, "big.bin", &big);
    let x1 = a.commit("one", &[]).unwrap();
    let chunk = a.flatten(&a.read_commit(&x1).unwrap().tree).unwrap()["big.bin"].id;
    let mut cl = connect(&running.addr, "a");
    cl.push(&mut a, &PushOptions { remote_branch: "b1".into(), ..push_opts() }).unwrap();
    // Close the pack holding everything so far; later pushes go to a newer pack.
    running.host.repo().store.roll().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(4));
    write(&a, "small.txt", b"added later\n");
    let x2 = a.commit("two", &[]).unwrap();
    cl.push(&mut a, &PushOptions { remote_branch: "b2".into(), ..push_opts() }).unwrap();
    cl.delete_remote_branch(&mut a, "origin", "b1").unwrap();
    cl.delete_remote_branch(&mut a, "origin", "b2").unwrap();
    cl.close();
    let epoch_before = running.host.repo().get_config("maintenance.epoch").unwrap();
    running.stop();

    // Offline maintenance with a 3 s grace: the old pack is rewritten without the now
    // unreachable chunk, the recent pack (with commit two and its trees) is left alone.
    {
        let mut repo = Repo::open(&host_dir).unwrap();
        repo.gc(3, 0, false).unwrap();
        assert!(!repo.store.has(&chunk).unwrap(), "setup: the chunk should have been dropped");
        assert!(repo.store.has(&x2).unwrap(), "setup: the recent commit should have survived");
        assert_ne!(repo.get_config("maintenance.epoch").unwrap(), epoch_before);
    }

    let host = Host::open(&host_dir, cfg, Arc::new(OpenAccess)).unwrap();
    // Nothing reachable, so nothing may be considered complete.
    assert_eq!(host.closed_count(), 0);
    let running = spawn(host, Box::new(TcpListenerAdapter::bind("127.0.0.1:0").unwrap()));
    let mut cl = connect(&running.addr, "a");
    // Pushing the same commit again must resend what is missing, not skip it.
    cl.push(&mut a, &PushOptions { remote_branch: "b3".into(), ..push_opts() }).unwrap();
    assert!(cl.stats.objects_transferred > 0, "{:?}", cl.stats);
    cl.close();
    let repo = running.host.repo();
    assert!(repo.store.has(&chunk).unwrap());
    assert_eq!(repo.get_ref("heads/b3").unwrap().unwrap().commit, Some(x2));
    let report = repo.verify().unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    drop(repo);
    running.stop();
}
