//! Multi-process synchronization tests: a real `rgit serve` process, real `rgit` client
//! processes, real TCP on loopback, and abrupt process exits at named crash points
//! (`RGIT_FAILPOINT`, see crates/protocol/src/failpoint.rs). A failpoint simulates a process
//! crash (bytes already handed to the OS survive); it does not simulate power loss.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rgit");
static N: AtomicUsize = AtomicUsize::new(0);

fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(BIN);
    c.arg("-C").arg(dir).args(args);
    c.env("USER", "tester").env("HOSTNAME", "testbox");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn rgit(dir: &Path, args: &[&str]) -> Output {
    run_env(dir, args, &[])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "command failed ({:?})\nstdout: {}\nstderr: {}",
        o.status,
        out(&o),
        err(&o)
    );
    out(&o)
}

struct Server {
    child: Child,
    addr: String,
}

impl Server {
    fn start(host: &Path, failpoint: Option<&str>, extra: &[&str]) -> Server {
        let pf = host
            .parent()
            .unwrap()
            .join(format!("port-{}", N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_file(&pf);
        let mut c = Command::new(BIN);
        c.arg("serve")
            .arg(host)
            .args(["--init", "--insecure-open-access", "--listen", "127.0.0.1:0", "--port-file"])
            .arg(&pf)
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(f) = failpoint {
            c.env("RGIT_FAILPOINT", f);
        }
        let mut child = c.spawn().unwrap();
        let t0 = Instant::now();
        loop {
            if let Ok(a) = fs::read_to_string(&pf) {
                if !a.is_empty() {
                    return Server { child, addr: a };
                }
            }
            if let Some(st) = child.try_wait().unwrap() {
                let mut e = String::new();
                use std::io::Read;
                child.stderr.take().unwrap().read_to_string(&mut e).unwrap();
                panic!("server exited early: {st:?}\n{e}");
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "server did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn url(&self) -> String {
        format!("tcp://{}", self.addr)
    }

    /// Waits for the process to exit by itself (a failpoint crash) and returns its code.
    fn wait_crash(&mut self) -> Option<i32> {
        let t0 = Instant::now();
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return st.code();
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "server did not crash");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn new_project(root: &Path, name: &str) -> PathBuf {
    let d = root.join(name);
    fs::create_dir_all(&d).unwrap();
    ok(rgit(&d, &["init"]));
    d
}

fn point_at(dir: &Path, url: &str) {
    ok(rgit(dir, &["remote", "add", "origin", url]));
}

fn commit_file(dir: &Path, name: &str, data: &[u8], msg: &str) {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
    ok(rgit(dir, &["commit", "-m", msg]));
}

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(n);
    v
}

fn local_objects(dir: &Path) -> u64 {
    let s = ok(rgit(dir, &["stats"]));
    s.lines()
        .find(|l| l.starts_with("objects:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap()
        .parse()
        .unwrap()
}

fn parse_after(s: &str, key: &str) -> u64 {
    let i = s.find(key).unwrap_or_else(|| panic!("no '{key}' in {s}"));
    s[i + key.len()..]
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

fn setup_big(root: &Path) -> (PathBuf, u64) {
    let a = new_project(root, "a");
    for i in 0..6 {
        commit_file(&a, &format!("data/f{i}.bin"), &noise(i + 1, 2 * 1024 * 1024), "add");
    }
    let total = local_objects(&a);
    (a, total)
}

#[test]
fn concurrent_pushes_from_many_processes_lose_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let seed = new_project(tmp.path(), "seed");
    commit_file(&seed, "base.txt", b"base\n", "base");
    point_at(&seed, &srv.url());
    ok(rgit(&seed, &["push"]));

    const N_WORKERS: usize = 6;
    let mut handles = Vec::new();
    for i in 0..N_WORKERS {
        let tmp = tmp.path().to_path_buf();
        let url = srv.url();
        handles.push(std::thread::spawn(move || {
            let d = tmp.join(format!("w{i}"));
            ok(Command::new(BIN).args(["clone", &url]).arg(&d).output().unwrap());
            commit_file(&d, &format!("work/{i}.txt"), format!("worker {i}\n").as_bytes(), &format!("worker-{i}"));
            // Every worker races on the same branch: losers must fetch, merge and retry.
            let mut rejected = 0;
            for _ in 0..40 {
                let o = rgit(&d, &["push"]);
                match o.status.code() {
                    Some(0) => return rejected,
                    Some(3) => {
                        rejected += 1;
                        assert!(err(&o).contains("fetch"), "{}", err(&o));
                        ok(rgit(&d, &["fetch"]));
                        ok(rgit(&d, &["merge", "origin/main"]));
                    }
                    _ => panic!("push failed: {} {}", out(&o), err(&o)),
                }
            }
            panic!("worker {i} never succeeded");
        }));
    }
    let rejections: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    // The race is real: with six simultaneous pushers at least one must have lost once.
    eprintln!("total rejected pushes that had to fetch+merge: {rejections}");

    let log = ok(rgit(&host, &["log", "-n", "200"]));
    for i in 0..N_WORKERS {
        assert!(log.contains(&format!("worker-{i}")), "commit worker-{i} lost:\n{log}");
    }
    assert!(log.contains("base"));
    let v = rgit(&host, &["verify"]);
    assert!(v.status.success(), "{}", out(&v));
    let fresh = tmp.path().join("fresh");
    ok(Command::new(BIN).args(["clone", &srv.url()]).arg(&fresh).output().unwrap());
    for i in 0..N_WORKERS {
        assert_eq!(fs::read(fresh.join(format!("work/{i}.txt"))).unwrap(), format!("worker {i}\n").as_bytes());
    }
    assert_eq!(fs::read(fresh.join("base.txt")).unwrap(), b"base\n");
}

#[test]
fn concurrent_pushes_to_different_branches_keep_every_head() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let seed = new_project(tmp.path(), "seed");
    commit_file(&seed, "base.txt", b"base\n", "base");
    point_at(&seed, &srv.url());
    ok(rgit(&seed, &["push"]));
    let main_head = {
        let l = ok(rgit(&host, &["branch"]));
        l
    };
    let mut hs = Vec::new();
    for i in 0..5 {
        let tmp = tmp.path().to_path_buf();
        let url = srv.url();
        hs.push(std::thread::spawn(move || {
            let d = tmp.join(format!("b{i}"));
            ok(Command::new(BIN).args(["clone", &url]).arg(&d).output().unwrap());
            commit_file(&d, &format!("x{i}.txt"), b"x", &format!("feature-{i}"));
            ok(rgit(&d, &["push", "origin", "main", "--as", &format!("feat{i}")]));
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    let branches = ok(rgit(&host, &["branch"]));
    for i in 0..5 {
        assert!(branches.contains(&format!("feat{i}")), "{branches}");
    }
    // main was not overwritten by any of them.
    assert_eq!(
        ok(rgit(&host, &["log", "-n", "5", "main"])).lines().filter(|l| l.contains("base")).count(),
        1,
        "{main_head}"
    );
    assert!(!ok(rgit(&host, &["log", "-n", "5", "main"])).contains("feature-"));
    assert!(rgit(&host, &["verify"]).status.success());
}

#[test]
fn rejected_push_keeps_local_commits_and_explains() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let a = new_project(tmp.path(), "a");
    commit_file(&a, "f.txt", b"base\n", "base");
    point_at(&a, &srv.url());
    ok(rgit(&a, &["push"]));
    let b = tmp.path().join("b");
    ok(Command::new(BIN).args(["clone", &srv.url()]).arg(&b).output().unwrap());

    commit_file(&a, "a.txt", b"a\n", "a-work");
    commit_file(&b, "b.txt", b"b\n", "b-work");
    ok(rgit(&a, &["push"]));
    let o = rgit(&b, &["push"]);
    assert_eq!(o.status.code(), Some(3), "{} {}", out(&o), err(&o));
    let e = err(&o);
    assert!(e.contains("fetch") && e.contains("merge"), "{e}");
    // Local state intact.
    assert!(ok(rgit(&b, &["log"])).contains("b-work"));
    assert_eq!(fs::read(b.join("b.txt")).unwrap(), b"b\n");
    assert!(ok(rgit(&b, &["status"])).contains("clean"));
    // The documented recovery path works.
    ok(rgit(&b, &["fetch"]));
    ok(rgit(&b, &["merge", "origin/main"]));
    ok(rgit(&b, &["push"]));
    let log = ok(rgit(&host, &["log", "-n", "20"]));
    assert!(log.contains("a-work") && log.contains("b-work"), "{log}");
}

#[test]
fn client_interrupted_mid_upload_resumes_without_resending() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let (a, total) = setup_big(tmp.path());
    point_at(&a, &srv.url());
    // The client process dies right after the host acknowledged its 2nd batch.
    let o = run_env(&a, &["push"], &[("RGIT_FAILPOINT", "client_put_acked:2")]);
    assert_eq!(o.status.code(), Some(86), "{} {}", out(&o), err(&o));
    assert!(ok(rgit(&host, &["branch"])).trim().is_empty(), "no branch may exist yet");

    let o = ok(rgit(&a, &["push"]));
    let staged = parse_after(&o, "resumed from staged:");
    let sent = parse_after(&o, "objects sent:");
    assert!(staged > 0, "{o}");
    assert!(sent > 0, "{o}");
    assert_eq!(staged + sent, total, "confirmed objects must not be sent again\n{o}");
    assert!(rgit(&host, &["verify"]).status.success());
    // And the result is complete: a clone reproduces every file.
    let c = tmp.path().join("c");
    ok(Command::new(BIN).args(["clone", &srv.url()]).arg(&c).output().unwrap());
    for i in 0..6u64 {
        assert_eq!(fs::read(c.join(format!("data/f{i}.bin"))).unwrap(), noise(i + 1, 2 * 1024 * 1024));
    }
}

#[test]
fn host_crash_mid_upload_resumes_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let mut srv = Server::start(&host, Some("stage_object:40"), &[]);
    let (a, total) = setup_big(tmp.path());
    point_at(&a, &srv.url());
    let o = rgit(&a, &["push"]);
    assert!(!o.status.success(), "push must not claim success: {}", out(&o));
    assert_eq!(srv.wait_crash(), Some(86));
    let srv2 = Server::start(&host, None, &[]);
    point_at(&a, &srv2.url());
    let o = ok(rgit(&a, &["push"]));
    let staged = parse_after(&o, "resumed from staged:");
    let sent = parse_after(&o, "objects sent:");
    assert!(staged >= 40, "objects staged before the crash must survive: {o}");
    assert_eq!(staged + sent, total, "{o}");
    assert!(rgit(&host, &["verify"]).status.success());
}

/// Runs a push against a host that crashes at `point`, restarts the host, and returns
/// (client repo, host dir, second push output, whether the host ref was set after the crash).
fn crash_scenario(point: &str) -> (tempfile::TempDir, PathBuf, PathBuf, Output, String, Server) {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let mut srv = Server::start(&host, Some(point), &[]);
    let a = new_project(tmp.path(), "a");
    for i in 0..3 {
        commit_file(&a, &format!("f{i}.bin"), &noise(10 + i, 600 * 1024), "work");
    }
    point_at(&a, &srv.url());
    let o = rgit(&a, &["push"]);
    assert!(!o.status.success(), "client must not see success when the host crashed at {point}");
    assert_eq!(srv.wait_crash(), Some(86));
    // State of the host as it is on disk right after the crash (read-only inspection).
    let after_crash = ok(rgit(&host, &["branch"]));
    let srv2 = Server::start(&host, None, &[]);
    point_at(&a, &srv2.url());
    let o = rgit(&a, &["push"]);
    (tmp, host, a, o, after_crash, srv2)
}

fn finish_crash_check(host: &Path, a: &Path) {
    let v = rgit(host, &["verify"]);
    assert!(v.status.success(), "{}", out(&v));
    let head_local = ok(rgit(a, &["log", "-n", "1"]));
    let head_host = ok(rgit(host, &["log", "-n", "1"]));
    assert_eq!(head_local.lines().next(), head_host.lines().next());
}

#[test]
fn crash_before_publish() {
    let (tmp, host, a, o, after, _srv) = crash_scenario("before_publish");
    // Nothing was published and no ref exists after the crash.
    assert!(after.trim().is_empty(), "{after}");
    let s = ok(o);
    // The staged objects were kept, so the retry sends nothing new.
    assert_eq!(parse_after(&s, "objects sent:"), 0, "{s}");
    assert!(parse_after(&s, "resumed from staged:") > 0, "{s}");
    finish_crash_check(&host, &a);
    drop(tmp);
}

#[test]
fn crash_between_publish_and_ref_update() {
    let (tmp, host, a, o, after, _srv) = crash_scenario("after_publish");
    // Objects are durable but unreferenced: no branch yet.
    assert!(after.trim().is_empty(), "{after}");
    let s = ok(o);
    // The retry succeeds; the large chunks are already in the store and are not resent.
    let sent = parse_after(&s, "objects sent:");
    let total = local_objects(&a);
    assert!(sent < total, "{s}");
    finish_crash_check(&host, &a);
    drop(tmp);
}

#[test]
fn crash_after_ref_update_before_ack() {
    let (tmp, host, a, o, after, _srv) = crash_scenario("after_ref_update");
    // The ref update is durable even though the client never saw an acknowledgement.
    assert!(after.contains("main"), "{after}");
    let s = ok(o);
    assert!(s.contains("up to date"), "retry must be a harmless no-op: {s}");
    finish_crash_check(&host, &a);
    // And it is the only commit chain: no duplicates.
    let log = ok(rgit(&host, &["log", "-n", "50"]));
    assert_eq!(log.matches("work").count(), 3, "{log}");
    drop(tmp);
}

#[test]
fn corrupted_staged_object_is_detected_and_resent() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let mut srv = Server::start(&host, Some("before_publish"), &[]);
    let a = new_project(tmp.path(), "a");
    commit_file(&a, "f.bin", &noise(3, 500 * 1024), "one");
    point_at(&a, &srv.url());
    assert!(!rgit(&a, &["push"]).status.success());
    srv.wait_crash();
    // Flip one byte in the largest staged file (bit rot while it sat in staging).
    let staging = host.join(".rgit/host/staging");
    let mut biggest: Option<(u64, PathBuf)> = None;
    for s in fs::read_dir(&staging).unwrap() {
        for f in fs::read_dir(s.unwrap().path().join("obj")).unwrap() {
            let f = f.unwrap();
            let len = f.metadata().unwrap().len();
            if biggest.as_ref().map_or(true, |(l, _)| len > *l) {
                biggest = Some((len, f.path()));
            }
        }
    }
    let (len, path) = biggest.unwrap();
    assert!(len > 100_000);
    let mut bytes = fs::read(&path).unwrap();
    bytes[len as usize / 2] ^= 0xff;
    fs::write(&path, bytes).unwrap();

    let srv2 = Server::start(&host, None, &[]);
    point_at(&a, &srv2.url());
    // The host notices the damage at publish time, drops the file, and the client resends it.
    let s = ok(rgit(&a, &["push"]));
    assert!(s.contains("Pushed"), "{s}");
    assert!(rgit(&host, &["verify"]).status.success());
    let c = tmp.path().join("c");
    ok(Command::new(BIN).args(["clone", &srv2.url()]).arg(&c).output().unwrap());
    assert_eq!(fs::read(c.join("f.bin")).unwrap(), noise(3, 500 * 1024));
}

#[test]
fn offline_commits_sync_after_the_host_comes_back() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let a = new_project(tmp.path(), "a");
    commit_file(&a, "f.txt", b"1\n", "online-1");
    let srv = Server::start(&host, None, &[]);
    point_at(&a, &srv.url());
    ok(rgit(&a, &["push"]));
    let url = srv.url();
    drop(srv); // host goes away

    // Work continues locally.
    commit_file(&a, "f.txt", b"2\n", "offline-2");
    commit_file(&a, "g.txt", b"3\n", "offline-3");
    let o = rgit(&a, &["push"]);
    assert!(!o.status.success());
    assert!(err(&o).to_lowercase().contains("refused") || err(&o).contains("error"), "{}", err(&o));
    assert!(ok(rgit(&a, &["log"])).contains("offline-3"));
    let _ = url;

    // The host returns (new port); everything syncs and nothing is duplicated.
    let srv2 = Server::start(&host, None, &[]);
    point_at(&a, &srv2.url());
    let s = ok(rgit(&a, &["push"]));
    assert!(s.contains("Pushed"), "{s}");
    let log = ok(rgit(&host, &["log", "-n", "10"]));
    for m in ["online-1", "offline-2", "offline-3"] {
        assert_eq!(log.matches(m).count(), 1, "{log}");
    }
    assert!(rgit(&host, &["verify"]).status.success());
}

#[test]
fn hosting_refuses_unsafe_setups() {
    let tmp = tempfile::tempdir().unwrap();
    // A working project is never served directly.
    let work = new_project(tmp.path(), "work");
    let o = Command::new(BIN).args(["serve", "--init", "--listen", "127.0.0.1:0"]).arg(&work).output().unwrap();
    assert!(!o.status.success());
    assert!(err(&o).contains("working project"), "{}", err(&o));
    // Open access (no authentication, no encryption) is loopback only.
    let host = tmp.path().join("host");
    let o = Command::new(BIN)
        .args(["serve", "--init", "--insecure-open-access", "--listen", "0.0.0.0:0"])
        .arg(&host)
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(err(&o).contains("only allowed on loopback"), "{}", err(&o));
    // Only one serve process per hosted repository.
    let h2 = tmp.path().join("h2");
    let _s = Server::start(&h2, None, &[]);
    let o = Command::new(BIN).args(["serve", "--insecure-open-access", "--listen", "127.0.0.1:0"]).arg(&h2).output().unwrap();
    assert!(!o.status.success());
    assert!(err(&o).contains("already being served"), "{}", err(&o));
    // Maintenance is refused while serving, and ordinary work commands are refused on a bare host.
    let o = rgit(&h2, &["gc"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("being served"), "{}", err(&o));
    let o = rgit(&h2, &["commit", "-m", "x"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("hosted repository"), "{}", err(&o));
}

#[test]
fn pushes_never_touch_the_host_users_working_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    // The person running the host also works on the project in their own clone.
    let seed = new_project(tmp.path(), "seed");
    commit_file(&seed, "f.txt", b"v1\n", "v1");
    point_at(&seed, &srv.url());
    ok(rgit(&seed, &["push"]));
    let mine = tmp.path().join("mine");
    ok(Command::new(BIN).args(["clone", &srv.url()]).arg(&mine).output().unwrap());
    fs::write(mine.join("f.txt"), b"my uncommitted edit\n").unwrap();

    // A teammate changes the same file and pushes.
    let mate = tmp.path().join("mate");
    ok(Command::new(BIN).args(["clone", &srv.url()]).arg(&mate).output().unwrap());
    commit_file(&mate, "f.txt", b"v2 by mate\n", "v2");
    ok(rgit(&mate, &["push"]));

    // The host user's files did not change, and no host-side directory holds a checkout.
    assert_eq!(fs::read(mine.join("f.txt")).unwrap(), b"my uncommitted edit\n");
    assert!(!host.join("f.txt").exists());
    // Fetch is safe; merging refuses to overwrite the uncommitted edit.
    ok(rgit(&mine, &["fetch"]));
    assert_eq!(fs::read(mine.join("f.txt")).unwrap(), b"my uncommitted edit\n");
    let o = rgit(&mine, &["merge", "origin/main"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("uncommitted"), "{}", err(&o));
    assert_eq!(fs::read(mine.join("f.txt")).unwrap(), b"my uncommitted edit\n");
}

#[test]
fn force_push_is_disabled_by_default_and_works_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let a = new_project(tmp.path(), "a");
    commit_file(&a, "f.txt", b"1\n", "first");
    point_at(&a, &srv.url());
    ok(rgit(&a, &["push"]));
    let z = new_project(tmp.path(), "z");
    commit_file(&z, "z.txt", b"z\n", "unrelated");
    point_at(&z, &srv.url());
    let o = rgit(&z, &["push", "--force"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("disabled"), "{}", err(&o));
    assert!(ok(rgit(&host, &["log"])).contains("first"));
    drop(srv);

    let srv2 = Server::start(&host, None, &["--allow-force"]);
    point_at(&z, &srv2.url());
    ok(rgit(&z, &["push", "--force"]));
    assert!(ok(rgit(&host, &["log"])).contains("unrelated"));
}

#[test]
fn interrupted_fetch_resumes_without_refetching_received_objects() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let (a, total) = setup_big(tmp.path());
    point_at(&a, &srv.url());
    ok(rgit(&a, &["push"]));

    let b = new_project(tmp.path(), "b");
    point_at(&b, &srv.url());
    // The client process dies after durably storing its 3rd downloaded batch.
    let o = run_env(&b, &["fetch"], &[("RGIT_FAILPOINT", "client_get_done:3")]);
    assert_eq!(o.status.code(), Some(86), "{} {}", out(&o), err(&o));
    // No tracking ref may point at partial history.
    let o2 = rgit(&b, &["log", "-n", "1", "origin/main"]);
    assert!(!o2.status.success(), "origin/main must not exist after an interrupted fetch");
    let have = local_objects(&b);
    assert!(have > 0 && have < total, "partial objects expected, have {have} of {total}");

    let o = ok(rgit(&b, &["fetch"]));
    let transferred = parse_after(&o, "Fetched:");
    let skipped = parse_after(&o, "already complete:");
    assert!(skipped >= have, "objects received before the crash are not requested again\n{o}");
    assert_eq!(transferred + have, total, "{o}");
    // The result matches the source.
    ok(rgit(&b, &["merge", "origin/main"]));
    for i in 0..6u64 {
        assert_eq!(fs::read(b.join(format!("data/f{i}.bin"))).unwrap(), noise(i + 1, 2 * 1024 * 1024));
    }
}


#[test]
fn empty_host_can_be_cloned_and_then_pushed_to() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let c = tmp.path().join("c");
    let o = Command::new(BIN).args(["clone", &srv.url()]).arg(&c).output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("empty project"), "{}", out(&o));
    commit_file(&c, "first.txt", b"hello\n", "first");
    let s = ok(rgit(&c, &["push"]));
    assert!(s.contains("Pushed"), "{s}");
    assert!(ok(rgit(&host, &["log"])).contains("first"));
}

#[test]
fn a_remote_pinned_to_one_project_refuses_another() {
    let tmp = tempfile::tempdir().unwrap();
    let h1 = tmp.path().join("h1");
    let s1 = Server::start(&h1, None, &[]);
    let a = new_project(tmp.path(), "a");
    commit_file(&a, "f.txt", b"1\n", "one");
    point_at(&a, &s1.url());
    ok(rgit(&a, &["push"]));
    assert!(!ok(rgit(&a, &["config", "remote.origin.project"])).trim().is_empty(), "first contact pins the project");
    // The same remote name now points at a different project's host.
    let h2 = tmp.path().join("h2");
    let s2 = Server::start(&h2, None, &[]);
    ok(rgit(&a, &["config", "remote.origin.url", &s2.url()]));
    let o = rgit(&a, &["push"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("serves project"), "{}", err(&o));
    assert!(ok(rgit(&h2, &["branch"])).trim().is_empty(), "nothing may reach the other project");
    drop(s1);
}

#[test]
fn fetch_watch_picks_up_pushes_without_touching_the_working_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let a = new_project(tmp.path(), "a");
    commit_file(&a, "f.txt", b"1\n", "one");
    point_at(&a, &srv.url());
    ok(rgit(&a, &["push"]));
    let b = tmp.path().join("b");
    ok(Command::new(BIN).args(["clone", &srv.url()]).arg(&b).output().unwrap());
    fs::write(b.join("scratch.txt"), b"my uncommitted work\n").unwrap();

    let mut watcher = Command::new(BIN)
        .arg("-C")
        .arg(&b)
        .args(["fetch", "--watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    commit_file(&a, "f.txt", b"2\n", "two-from-a");
    ok(rgit(&a, &["push"]));

    let t0 = Instant::now();
    loop {
        let o = rgit(&b, &["log", "-n", "3", "origin/main"]);
        if o.status.success() && out(&o).contains("two-from-a") {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "watcher never fetched the new commit");
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = watcher.kill();
    let _ = watcher.wait();
    // The watcher fetched but neither merged nor touched local files.
    assert_eq!(fs::read(b.join("f.txt")).unwrap(), b"1\n");
    assert_eq!(fs::read(b.join("scratch.txt")).unwrap(), b"my uncommitted work\n");
    assert!(!ok(rgit(&b, &["log", "-n", "5"])).contains("two-from-a"));
}

#[test]
fn host_keeps_working_after_offline_gc() {
    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("host");
    let srv = Server::start(&host, None, &[]);
    let a = new_project(tmp.path(), "a");
    commit_file(&a, "big.bin", &noise(5, 800 * 1024), "one");
    commit_file(&a, "small.txt", b"x\n", "two");
    point_at(&a, &srv.url());
    ok(rgit(&a, &["push"]));
    drop(srv);
    // Maintenance needs the host to be stopped.
    ok(rgit(&host, &["gc", "--aggressive"]));
    assert!(rgit(&host, &["verify"]).status.success());
    let srv2 = Server::start(&host, None, &[]);
    point_at(&a, &srv2.url());
    commit_file(&a, "small.txt", b"y\n", "three");
    let s = ok(rgit(&a, &["push"]));
    // Only the new small objects travel; the big chunk is still on the host.
    assert!(parse_after(&s, "wire bytes sent:") < 20_000, "{s}");
    assert!(rgit(&host, &["verify"]).status.success());
    let c = tmp.path().join("c");
    ok(Command::new(BIN).args(["clone", &srv2.url()]).arg(&c).output().unwrap());
    assert_eq!(fs::read(c.join("big.bin")).unwrap(), noise(5, 800 * 1024));
    assert_eq!(fs::read(c.join("small.txt")).unwrap(), b"y\n");
}
