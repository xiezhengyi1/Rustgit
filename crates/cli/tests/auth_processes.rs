//! Device authentication, invitations, roles and revocation, end to end: a real `rgit serve`
//! process, real `rgit` client processes (each "device" has its own key directory), real TCP on
//! loopback. Nothing about the crypto is mocked. Keys live in 0600 files (the explicit
//! `--insecure-file-keys` mode) so the tests never touch the real Keychain.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_rgit");
static N: AtomicUsize = AtomicUsize::new(0);

struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        Env { root: tempfile::tempdir().unwrap() }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    /// A device = its own key directory.
    fn keys(&self, who: &str) -> PathBuf {
        self.root.path().join(format!("keys-{who}"))
    }
}

fn cmd(keys: &Path, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.arg("--json").arg("-C").arg(dir).args(args);
    c.env("USER", "tester").env("HOSTNAME", "testbox");
    c.env("RGIT_INSECURE_FILE_KEYS", "1").env("RGIT_KEY_DIR", keys);
    c
}

/// Runs a command with `--json`; returns (exit ok, parsed last stdout line, stderr).
fn run(keys: &Path, dir: &Path, args: &[&str]) -> (bool, Value, String) {
    let o = cmd(keys, dir, args).output().unwrap();
    parse(&o)
}

fn parse(o: &Output) -> (bool, Value, String) {
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    let last = text.lines().last().unwrap_or("{}");
    let v: Value = serde_json::from_str(last).unwrap_or_else(|e| panic!("not JSON ({e}): {text:?}"));
    (o.status.success(), v, String::from_utf8_lossy(&o.stderr).to_string())
}

fn ok(keys: &Path, dir: &Path, args: &[&str]) -> Value {
    let (good, v, e) = run(keys, dir, args);
    assert!(good && v["ok"] == true, "{args:?} failed: {v}\n{e}");
    v["data"].clone()
}

fn fail(keys: &Path, dir: &Path, args: &[&str]) -> Value {
    let (good, v, e) = run(keys, dir, args);
    assert!(!good && v["ok"] == false, "{args:?} should have failed: {v}\n{e}");
    v["error"].clone()
}

struct Server {
    child: Child,
    addr: String,
    dir: PathBuf,
    keys: PathBuf,
    stdout_path: PathBuf,
}

impl Server {
    fn start(env: &Env, name: &str, extra: &[&str]) -> Server {
        let dir = env.path(name);
        let keys = env.keys(&format!("host-{name}"));
        let pf = env.path(&format!("port-{}", N.fetch_add(1, Ordering::SeqCst)));
        let stdout_path = env.path(&format!("{name}.stdout"));
        let mut c = Command::new(BIN);
        c.arg("--json").arg("serve").arg(&dir);
        c.args(["--init", "--listen", "127.0.0.1:0", "--log", "--port-file"])
            .arg(&pf)
            .args(extra);
        c.env("RGIT_INSECURE_FILE_KEYS", "1").env("RGIT_KEY_DIR", &keys);
        c.stdout(fs::File::create(&stdout_path).unwrap())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let t0 = Instant::now();
        loop {
            if let Ok(a) = fs::read_to_string(&pf) {
                if !a.is_empty() {
                    return Server { child, addr: a, dir, keys, stdout_path };
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

    /// Host-side admin command (same key directory as the server).
    fn admin(&self, args: &[&str]) -> Value {
        ok(&self.keys, &self.dir, args)
    }

    fn admin_fail(&self, args: &[&str]) -> Value {
        fail(&self.keys, &self.dir, args)
    }

    fn fingerprint(&self) -> String {
        self.admin(&["host", "id"])["fingerprint"].as_str().unwrap().to_string()
    }

    fn invite(&self, role: &str, extra: &[&str]) -> String {
        let hint = self.url();
        let mut a = vec!["invite", "create", "--role", role, "--hint", &hint];
        a.extend_from_slice(extra);
        self.admin(&a)["uri"].as_str().unwrap().to_string()
    }

    /// Stops the server and returns everything it printed (stdout, stderr).
    fn finish(mut self) -> (String, String) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut e = String::new();
        use std::io::Read;
        if let Some(mut s) = self.child.stderr.take() {
            let _ = s.read_to_string(&mut e);
        }
        (fs::read_to_string(&self.stdout_path).unwrap_or_default(), e)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn join(env: &Env, who: &str, uri: &str, dest: &str) -> (bool, Value, String) {
    let keys = env.keys(who);
    let mut c = cmd(&keys, env.root.path(), &["join", "-", dest]);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(uri.as_bytes()).unwrap();
    parse(&child.wait_with_output().unwrap())
}

fn joined(env: &Env, who: &str, uri: &str) -> PathBuf {
    let dest = env.path(&format!("work-{who}"));
    let (good, v, e) = join(env, who, uri, dest.to_str().unwrap());
    assert!(good, "join as {who} failed: {v}\n{e}");
    dest
}

fn commit_file(keys: &Path, dir: &Path, name: &str, data: &str) {
    fs::write(dir.join(name), data).unwrap();
    ok(keys, dir, &["commit", "-m", &format!("add {name}")]);
}

fn code(v: &Value) -> &str {
    v["code"].as_str().unwrap_or("")
}

// ---------------------------------------------------------------------------------------------

#[test]
fn join_clone_push_fetch_and_roles_over_the_encrypted_channel() {
    let env = Env::new();
    let srv = Server::start(&env, "host", &[]);
    // device A (write) joins and pushes
    let a = joined(&env, "alice", &srv.invite("write", &[]));
    let ka = env.keys("alice");
    commit_file(&ka, &a, "one.txt", "hello");
    let pushed = ok(&ka, &a, &["push"]);
    assert_eq!(pushed["up_to_date"], false);
    // the remote is recorded as encrypted and pinned
    let remotes = ok(&ka, &a, &["remote"]);
    assert_eq!(remotes["remotes"][0]["secure"], true);
    assert_eq!(remotes["remotes"][0]["host_key"], srv.fingerprint());

    // device B (read) joins: gets the content, cannot push
    let b = joined(&env, "bob", &srv.invite("read", &[]));
    let kb = env.keys("bob");
    assert_eq!(fs::read_to_string(b.join("one.txt")).unwrap(), "hello");
    commit_file(&kb, &b, "two.txt", "x");
    let e = fail(&kb, &b, &["push"]);
    assert_eq!(code(&e), "permission_denied", "{e}");
    // but reading still works
    ok(&kb, &b, &["fetch"]);
    // nothing of B's push reached the host
    let v = ok(&srv.keys, &srv.dir, &["log"]);
    assert_eq!(v["commits"].as_array().unwrap().len(), 1);

    // devices list: both, with roles and last-seen times
    let list = srv.admin(&["devices", "list"]);
    let devs = list["devices"].as_array().unwrap();
    assert_eq!(devs.len(), 2);
    for d in devs {
        assert_eq!(d["status"], "active");
        assert!(d["last_seen"].as_u64().is_some(), "last seen must be recorded: {d}");
    }
    let roles: Vec<_> = devs.iter().map(|d| d["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["write", "read"]);

    // promote B to write: takes effect on its next connection
    let bfp = devs[1]["fingerprint"].as_str().unwrap().to_string();
    srv.admin(&["devices", "role", &bfp[..12], "write"]);
    ok(&kb, &b, &["fetch"]);
    let _ = ok(&kb, &b, &["push"]);
}

#[test]
fn write_role_cannot_force_or_delete_but_admin_can() {
    let env = Env::new();
    let srv = Server::start(&env, "host", &["--allow-force", "--allow-delete"]);
    let w = joined(&env, "writer", &srv.invite("write", &[]));
    let kw = env.keys("writer");
    commit_file(&kw, &w, "a.txt", "1");
    ok(&kw, &w, &["push"]);
    ok(&kw, &w, &["branch", "side"]);
    ok(&kw, &w, &["push", "origin", "side"]);
    // deleting a branch is admin-only even though the host allows deletion
    let e = fail(&kw, &w, &["push", "--delete", "side"]);
    assert_eq!(code(&e), "permission_denied", "{e}");
    // an unrelated history pushed with --force is admin-only even though the host allows force
    let z = env.path("z");
    fs::create_dir_all(&z).unwrap();
    ok(&kw, &z, &["init"]);
    commit_file(&kw, &z, "z.txt", "unrelated");
    ok(&kw, &z, &["remote", "add", "origin", &srv.url(), "--host-key", &srv.fingerprint()]);
    let e = fail(&kw, &z, &["push", "--force"]);
    assert_eq!(code(&e), "permission_denied", "{e}");
    assert!(!ok(&srv.keys, &srv.dir, &["log"])["commits"].to_string().contains("add z.txt"));

    // promoted to admin, the same device may do both
    let fp = srv.admin(&["devices", "list"])["devices"][0]["fingerprint"].as_str().unwrap().to_string();
    srv.admin(&["devices", "role", &fp[..12], "admin"]);
    ok(&kw, &w, &["push", "--delete", "side"]);
    ok(&kw, &z, &["push", "--force"]);
    assert!(ok(&srv.keys, &srv.dir, &["log"])["commits"].to_string().contains("add z.txt"));
}

#[test]
fn wrong_pinned_host_key_and_man_in_the_middle_are_rejected() {
    let env = Env::new();
    let real = Server::start(&env, "real", &[]);
    let attacker = Server::start(&env, "attacker", &[]);
    let a = joined(&env, "alice", &real.invite("write", &[]));
    let ka = env.keys("alice");
    commit_file(&ka, &a, "f.txt", "x");
    ok(&ka, &a, &["push"]);

    // 1. the pin is wrong from the start
    let wrong = "ab".repeat(32);
    let tmp = env.path("probe");
    fs::create_dir_all(&tmp).unwrap();
    ok(&ka, &tmp, &["init"]);
    ok(&ka, &tmp, &["remote", "add", "origin", &real.url(), "--host-key", &wrong]);
    let e = fail(&ka, &tmp, &["fetch"]);
    assert_eq!(code(&e), "host_key_mismatch", "{e}");

    // 2. "key swap": the remote's address now answers with a different, perfectly valid host
    ok(&ka, &a, &["remote", "remove", "origin"]);
    ok(&ka, &a, &["remote", "add", "origin", &attacker.url(), "--host-key", &real.fingerprint()]);
    let e = fail(&ka, &a, &["fetch"]);
    assert_eq!(code(&e), "host_key_mismatch", "{e}");
    let e = fail(&ka, &a, &["push"]);
    assert_eq!(code(&e), "host_key_mismatch", "{e}");

    // 3. an invitation whose hint leads to the attacker fails the same way and is NOT consumed
    let uri = real.invite("read", &[]);
    let swapped = {
        // same invitation, but the hint is rewritten to the attacker's address
        let inv = uri.clone();
        let payload = inv.strip_prefix("projectapp://join/").unwrap();
        let mut raw = b64d(payload);
        let from = format!("tcp://{}", real.addr);
        let to = format!("tcp://{}", attacker.addr);
        assert_eq!(from.len(), to.len(), "loopback addresses have equal length");
        let pos = raw.windows(from.len()).position(|w| w == from.as_bytes()).unwrap();
        raw[pos..pos + from.len()].copy_from_slice(to.as_bytes());
        format!("projectapp://join/{}", b64e(&raw))
    };
    let (good, v, _) = join(&env, "carol", &swapped, env.path("carol-work").to_str().unwrap());
    assert!(!good);
    assert_eq!(code(&v["error"]), "host_key_mismatch", "{v}");
    // the real invitation still works afterwards: the failed attempt consumed nothing
    let (good, v, e) = join(&env, "carol", &uri, env.path("carol-real").to_str().unwrap());
    assert!(good, "{v}\n{e}");
}

fn b64d(s: &str) -> Vec<u8> {
    // minimal base64url (no padding) decoder for the test
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => panic!("bad char"),
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

fn b64e(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for ch in b.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..=ch.len() {
            s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

#[test]
fn invitation_failures_are_distinct_and_safe() {
    let env = Env::new();
    let srv = Server::start(&env, "host", &[]);
    let dest = |n: &str| env.path(n).to_string_lossy().to_string();

    // malformed
    for (i, bad) in ["", "hello", "https://example.com/join/abc", "projectapp://join/%%%%", "projectapp://join/AAAA"]
        .iter()
        .enumerate()
    {
        let (good, v, _) = join(&env, "x", bad, &dest(&format!("m{i}")));
        assert!(!good);
        assert_eq!(code(&v["error"]), "invite_malformed", "{bad:?}: {v}");
    }

    // expired (checked locally from the link, and by the host)
    let uri = srv.invite("read", &["--expires", "1s"]);
    std::thread::sleep(Duration::from_millis(2200));
    let (good, v, _) = join(&env, "late", &uri, &dest("late"));
    assert!(!good);
    assert_eq!(code(&v["error"]), "invite_expired", "{v}");
    // ... and `invite list` agrees
    let list = srv.admin(&["invite", "list"]);
    assert_eq!(list["invitations"][0]["status"], "expired");

    // tampered secret: indistinguishable from an unknown invitation
    let good_uri = srv.invite("read", &[]);
    let payload = good_uri.strip_prefix("projectapp://join/").unwrap();
    let mut raw = b64d(payload);
    let n = raw.len();
    raw[n - 1] ^= 0x01; // last byte of the secret
    let (g, v, _) = join(&env, "tamper", &format!("projectapp://join/{}", b64e(&raw)), &dest("t"));
    assert!(!g);
    assert_eq!(code(&v["error"]), "invite_invalid", "{v}");
    // unknown invitation id
    let mut raw = b64d(payload);
    raw[n - 34] ^= 0x01; // last byte of the 16-byte id (it is followed by a length byte and the 32-byte secret)
    let (g, v, _) = join(&env, "tamper2", &format!("projectapp://join/{}", b64e(&raw)), &dest("t2"));
    assert!(!g);
    assert_eq!(code(&v["error"]), "invite_invalid", "{v}");

    // replay: single use
    let uri = srv.invite("read", &[]);
    joined(&env, "first", &uri);
    let (g, v, _) = join(&env, "second", &uri, &dest("second"));
    assert!(!g);
    assert_eq!(code(&v["error"]), "invite_used", "{v}");
    // the same device retrying its own redemption is idempotent (lost reply / crash)
    let (g, v, e) = join(&env, "first", &uri, &dest("first-again"));
    assert!(g, "{v}\n{e}");

    // revoked
    let uri = srv.invite("read", &[]);
    let id = srv.admin(&["invite", "list"])["invitations"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    srv.admin(&["invite", "revoke", &id]);
    let (g, v, _) = join(&env, "revoked", &uri, &dest("rv"));
    assert!(!g);
    assert_eq!(code(&v["error"]), "invite_revoked", "{v}");
    assert_eq!(srv.admin_fail(&["invite", "revoke", "ffffffffffff"])["code"], "no_such_invitation");

    // none of the failed attempts enrolled anything
    let devs = srv.admin(&["devices", "list"]);
    assert_eq!(devs["devices"].as_array().unwrap().len(), 1, "only 'first' may be enrolled");
}

#[test]
fn single_use_invitation_survives_a_race_between_processes() {
    let env = Env::new();
    let srv = Server::start(&env, "host", &[]);
    let uri = srv.invite("write", &[]);
    let children: Vec<_> = (0..8)
        .map(|i| {
            let keys = env.keys(&format!("racer{i}"));
            let dest = env.path(&format!("racer-work-{i}"));
            let mut c = cmd(&keys, env.root.path(), &["join", "-", dest.to_str().unwrap()]);
            c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
            let mut ch = c.spawn().unwrap();
            ch.stdin.take().unwrap().write_all(uri.as_bytes()).unwrap();
            ch
        })
        .collect();
    let mut wins = 0;
    for ch in children {
        let (good, v, e) = parse(&ch.wait_with_output().unwrap());
        if good {
            wins += 1;
        } else {
            assert_eq!(code(&v["error"]), "invite_used", "{v}\n{e}");
        }
    }
    assert_eq!(wins, 1, "a single-use invitation must enroll exactly one device");
    assert_eq!(srv.admin(&["devices", "list"])["devices"].as_array().unwrap().len(), 1);
}

#[test]
fn unenrolled_and_plaintext_clients_are_refused() {
    let env = Env::new();
    let srv = Server::start(&env, "host", &[]);
    let a = joined(&env, "alice", &srv.invite("write", &[]));
    let ka = env.keys("alice");
    commit_file(&ka, &a, "a.txt", "1");
    ok(&ka, &a, &["push"]);

    // a device with the right pin but no enrollment
    let stranger = env.keys("stranger");
    let work = env.path("stranger-work");
    fs::create_dir_all(&work).unwrap();
    ok(&stranger, &work, &["init"]);
    ok(&stranger, &work, &["remote", "add", "origin", &srv.url(), "--host-key", &srv.fingerprint()]);
    let e = fail(&stranger, &work, &["fetch"]);
    assert_eq!(code(&e), "not_enrolled", "{e}");

    // plaintext client (no pin) against the secure host
    let plain = env.path("plain-work");
    fs::create_dir_all(&plain).unwrap();
    ok(&stranger, &plain, &["init"]);
    ok(&stranger, &plain, &["remote", "add", "origin", &srv.url()]);
    let e = fail(&stranger, &plain, &["fetch"]);
    assert_eq!(code(&e), "security_required", "{e}");
    let e = fail(&stranger, &env.path("plain-clone"), &["clone", &srv.url()]);
    assert_eq!(code(&e), "security_required", "{e}");

    // and a secure client against an open-access (plaintext) host gets a helpful error
    let open = Server::start(&env, "open", &["--insecure-open-access"]);
    ok(&stranger, &work, &["remote", "remove", "origin"]);
    ok(&stranger, &work, &["remote", "add", "origin", &open.url(), "--host-key", &srv.fingerprint()]);
    let e = fail(&stranger, &work, &["fetch"]);
    assert_eq!(code(&e), "secure_channel_failed", "{e}");
    assert!(e["message"].as_str().unwrap().contains("insecure-open-access"), "{e}");
}

#[test]
fn revoked_device_loses_access_on_the_next_connection() {
    let env = Env::new();
    let srv = Server::start(&env, "host", &[]);
    let a = joined(&env, "alice", &srv.invite("write", &[]));
    let ka = env.keys("alice");
    commit_file(&ka, &a, "a.txt", "1");
    ok(&ka, &a, &["push"]);
    let fp = srv.admin(&["devices", "list"])["devices"][0]["fingerprint"].as_str().unwrap().to_string();
    let r = srv.admin(&["devices", "revoke", &fp[..10]]);
    assert_eq!(r["device"]["status"], "revoked");
    for args in [&["fetch"][..], &["push"][..]] {
        let e = fail(&ka, &a, args);
        assert_eq!(code(&e), "device_revoked", "{args:?}: {e}");
    }
    // a revoked key cannot enroll again, even with a fresh invitation
    let (g, v, _) = join(&env, "alice", &srv.invite("write", &[]), env.path("again").to_str().unwrap());
    assert!(!g);
    assert_eq!(code(&v["error"]), "device_revoked", "{v}");
    // the listing says so
    let l = srv.admin(&["devices", "list"]);
    assert_eq!(l["devices"][0]["status"], "revoked");
    assert!(l["devices"][0]["revoked_at"].as_u64().is_some());
}

#[test]
fn host_key_rotation_follows_succession_and_compromise_requires_repinning() {
    let env = Env::new();
    let mut srv = Server::start(&env, "host", &[]);
    let a = joined(&env, "alice", &srv.invite("write", &[]));
    let ka = env.keys("alice");
    commit_file(&ka, &a, "a.txt", "1");
    ok(&ka, &a, &["push"]);
    let old_fp = srv.fingerprint();

    // refused while serving
    assert_eq!(srv.admin_fail(&["host", "rotate"])["code"], "host_serving");
    let (dir, keys) = (srv.dir.clone(), srv.keys.clone());
    let url_port = srv.addr.clone();
    let _ = srv.child.kill();
    let _ = srv.child.wait();
    drop(srv);

    // rotate with a signed succession, restart on the same data
    let r = ok(&keys, &dir, &["host", "rotate"]);
    assert_eq!(r["succession_recorded"], true);
    let new_fp = r["fingerprint"].as_str().unwrap().to_string();
    assert_ne!(new_fp, old_fp);
    let (srv, _) = restart(&env, &dir, &keys, &url_port);
    // alice is pinned to the OLD fingerprint but follows the succession and re-pins
    ok(&ka, &a, &["fetch"]);
    let remotes = ok(&ka, &a, &["remote"]);
    assert_eq!(remotes["remotes"][0]["host_key"], new_fp);
    drop(srv);

    // compromised rotation: no succession; pinned clients refuse the new key
    let r = ok(&keys, &dir, &["host", "rotate", "--compromised"]);
    assert_eq!(r["succession_recorded"], false);
    let newest = r["fingerprint"].as_str().unwrap().to_string();
    let (srv, url) = restart(&env, &dir, &keys, &url_port);
    let e = fail(&ka, &a, &["fetch"]);
    assert_eq!(code(&e), "host_key_mismatch", "{e}");
    // after the owner confirms the new fingerprint out of band, the already enrolled device
    // re-pins without needing a new invitation
    ok(&ka, &a, &["remote", "remove", "origin"]);
    ok(&ka, &a, &["remote", "add", "origin", &url, "--host-key", &newest]);
    ok(&ka, &a, &["fetch"]);
    drop(srv);
}

/// Restarts `rgit serve` on existing data (same key store).
fn restart(env: &Env, dir: &Path, keys: &Path, listen: &str) -> (Server, String) {
    let pf = env.path(&format!("port-{}", N.fetch_add(1, Ordering::SeqCst)));
    let stdout_path = env.path(&format!("restart-{}.stdout", N.fetch_add(1, Ordering::SeqCst)));
    let mut c = Command::new(BIN);
    c.arg("--json").arg("serve").arg(dir).args(["--listen", listen, "--log", "--port-file"]).arg(&pf);
    c.env("RGIT_INSECURE_FILE_KEYS", "1").env("RGIT_KEY_DIR", keys);
    c.stdout(fs::File::create(&stdout_path).unwrap()).stderr(Stdio::piped());
    let child = c.spawn().unwrap();
    let t0 = Instant::now();
    let addr = loop {
        if let Ok(a) = fs::read_to_string(&pf) {
            if !a.is_empty() {
                break a;
            }
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "restart failed");
        std::thread::sleep(Duration::from_millis(20));
    };
    let url = format!("tcp://{addr}");
    (
        Server { child, addr, dir: dir.to_path_buf(), keys: keys.to_path_buf(), stdout_path },
        url,
    )
}

#[test]
fn secrets_never_appear_in_logs_lists_or_error_output() {
    let env = Env::new();
    let srv = Server::start(&env, "host", &[]);
    let created = srv.admin(&["invite", "create", "--role", "write", "--hint", &srv.url()]);
    let uri = created["uri"].as_str().unwrap().to_string();
    let payload = uri.strip_prefix("projectapp://join/").unwrap().to_string();
    let raw = b64d(&payload);
    let secret = raw[raw.len() - 32..].to_vec();
    let secret_hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
    let secret_b64 = b64e(&secret);

    let mut seen = String::new();
    let (g, v, e) = join(&env, "alice", &uri, env.path("alice-work").to_str().unwrap());
    assert!(g, "{v}\n{e}");
    seen.push_str(&format!("{v}{e}"));
    // failures: replay and tampering
    let (_, v, e) = join(&env, "bob", &uri, env.path("bob-work").to_str().unwrap());
    seen.push_str(&format!("{v}{e}"));
    let (_, v, e) = join(&env, "bob", &format!("{uri}x"), env.path("bob-work2").to_str().unwrap());
    seen.push_str(&format!("{v}{e}"));
    for args in [&["invite", "list"][..], &["devices", "list"][..], &["host", "id"][..]] {
        let (_, v, e) = run(&srv.keys, &srv.dir, args);
        seen.push_str(&format!("{v}{e}"));
    }
    let (_, v, e) = run(&env.keys("alice"), env.root.path(), &["device", "id"]);
    seen.push_str(&format!("{v}{e}"));
    let (out, err) = srv.finish();
    seen.push_str(&out);
    seen.push_str(&err);
    assert!(!err.is_empty(), "the host should have logged something (--log)");

    // the enrollment secret, in any of its renderings, is not in anything that was printed
    // (the link itself is printed once, by `invite create`, by design)
    for needle in [&secret_hex, &secret_b64, &payload] {
        assert!(!seen.contains(needle.as_str()), "secret material leaked: {needle:.8}…");
    }
    // private key seeds (device and host key files) never appear either
    for dir in [env.keys("alice"), env.keys("host-host")] {
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            let seed = fs::read_to_string(&p).unwrap().trim().to_string();
            assert_eq!(seed.len(), 64);
            assert!(!seen.contains(&seed), "private key leaked from {p:?}");
        }
    }
    // the authorization database holds no enrollment secret either
    let db = fs::read(env.path("host/.rgit/host/authz.sqlite")).unwrap();
    assert!(!db.windows(32).any(|w| w == &secret[..]));
}

#[test]
fn device_identity_is_stable_private_and_never_silently_unprotected() {
    let env = Env::new();
    let keys = env.keys("me");
    let first = ok(&keys, env.root.path(), &["device", "id"]);
    assert_eq!(first["created"], true);
    assert_eq!(first["key_storage"], "file");
    assert_eq!(first["key_storage_protected"], false);
    let second = ok(&keys, env.root.path(), &["device", "id"]);
    assert_eq!(second["created"], false);
    assert_eq!(first["fingerprint"], second["fingerprint"]);
    assert_eq!(first["fingerprint"].as_str().unwrap().len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let f = keys.join("device.key");
        assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        // a loosened file is refused rather than used
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        let e = fail(&keys, env.root.path(), &["device", "id"]);
        assert!(e["message"].as_str().unwrap().contains("readable by other users"), "{e}");
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn open_access_stays_loopback_only_and_authenticated_serving_may_listen_anywhere() {
    let env = Env::new();
    let o = Command::new(BIN)
        .args(["serve", "--init", "--insecure-open-access", "--listen", "0.0.0.0:0"])
        .arg(env.path("h"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("only allowed on loopback"));
    // authenticated mode needs no override to listen on all interfaces
    let mut srv = Command::new(BIN)
        .args(["serve", "--init", "--listen", "0.0.0.0:0", "--port-file"])
        .arg(env.path("pf"))
        .arg(env.path("h2"))
        .env("RGIT_INSECURE_FILE_KEYS", "1")
        .env("RGIT_KEY_DIR", env.keys("h2"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while fs::read_to_string(env.path("pf")).map(|s| s.is_empty()).unwrap_or(true) {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = srv.kill();
    let _ = srv.wait();
}

#[test]
fn invite_needs_a_hint_and_defaults_are_expiring_single_use_read_only() {
    let env = Env::new();
    let host = env.path("host");
    let keys = env.keys("h");
    let o = Command::new(BIN)
        .args(["serve", "--init", "--insecure-open-access", "--listen", "127.0.0.1:0", "--port-file"])
        .arg(env.path("pf0"))
        .arg(&host)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut o = o;
    let t0 = Instant::now();
    while fs::read_to_string(env.path("pf0")).map(|s| s.is_empty()).unwrap_or(true) {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = o.kill();
    let _ = o.wait();
    let e = fail(&keys, &host, &["invite", "create"]);
    assert_eq!(code(&e), "invalid", "{e}");
    let d = ok(&keys, &host, &["invite", "create", "--hint", "tcp://127.0.0.1:1"]);
    assert_eq!(d["role"], "read");
    assert_eq!(d["max_uses"], 1);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let exp = d["expires"].as_u64().unwrap();
    assert!(exp > now + 23 * 3600 && exp <= now + 24 * 3600 + 5, "default lifetime is 24h");
    assert!(d["uri"].as_str().unwrap().starts_with("projectapp://join/"));
    // bad inputs
    assert_eq!(code(&fail(&keys, &host, &["invite", "create", "--hint", "tcp://x:1", "--role", "root"])), "invalid");
    assert_eq!(code(&fail(&keys, &host, &["invite", "create", "--hint", "tcp://x:1", "--expires", "0s"])), "invalid");
    assert_eq!(code(&fail(&keys, &host, &["invite", "create", "--hint", "ftp://x"])), "invalid");
}

#[test]
fn a_join_that_cannot_work_does_not_burn_the_invitation_and_add_remote_pins_the_host() {
    let env = Env::new();
    let srv = Server::start(&env, "host", &[]);
    let uri = srv.invite("write", &[]);
    // a non-empty destination is refused before anything is redeemed
    let busy = env.path("busy");
    fs::create_dir_all(&busy).unwrap();
    fs::write(busy.join("x"), "x").unwrap();
    let (g, v, _) = join(&env, "dave", &uri, busy.to_str().unwrap());
    assert!(!g);
    assert_eq!(code(&v["error"]), "invalid", "{v}");
    assert_eq!(srv.admin(&["devices", "list"])["devices"].as_array().unwrap().len(), 0);

    // the same link still works, here into an existing project via --add-remote
    let proj = env.path("proj");
    fs::create_dir_all(&proj).unwrap();
    let kd = env.keys("dave");
    ok(&kd, &proj, &["init"]);
    let mut c = cmd(&kd, &proj, &["join", "-", "--add-remote", "--remote", "team"]);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(uri.as_bytes()).unwrap();
    let (g, v, e) = parse(&child.wait_with_output().unwrap());
    assert!(g, "{v}\n{e}");
    assert_eq!(v["data"]["role"], "write");
    let r = ok(&kd, &proj, &["remote"]);
    assert_eq!(r["remotes"][0]["name"], "team");
    assert_eq!(r["remotes"][0]["host_key"], srv.fingerprint());
    // and the remote is usable
    commit_file(&kd, &proj, "n.txt", "1");
    ok(&kd, &proj, &["push", "team"]);
}
