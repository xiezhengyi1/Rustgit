//! `rgit` as real processes over Iroh: `serve --iroh`, invitations carrying an `iroh://` hint,
//! `join`, `push`, `fetch`, with a locally started `iroh-relay` server in between and the
//! `connection` state read back from `--json` output. The `rgit` binary is built with the
//! `iroh` feature by this test (a nested `cargo build`, quick when already built).

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::*;
use serde_json::Value;

fn rgit_bin() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let out = Command::new(env!("CARGO"))
            .args([
                "build",
                "-q",
                "-p",
                "rustgit-cli",
                "--features",
                "iroh",
                "--bin",
                "rgit",
                "--message-format=json",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stderr(Stdio::inherit())
            .output()
            .expect("cargo build");
        assert!(out.status.success(), "building rgit --features iroh failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "rgit")
            .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
            .next_back()
            .expect("rgit executable path")
    })
}

fn b64(b: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn write_pem(path: &Path, der: &[u8]) {
    let body = b64(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    fs::write(path, pem).unwrap();
}

struct World {
    root: tempfile::TempDir,
}

impl World {
    fn new() -> World {
        World {
            root: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self, n: &str) -> PathBuf {
        self.root.path().join(n)
    }
}

/// One machine/user: its own key directory, config directory and Iroh settings.
#[derive(Clone)]
struct Who {
    keys: PathBuf,
    home: PathBuf,
    iroh_env: Vec<(String, String)>,
}

impl Who {
    fn new(w: &World, name: &str, iroh_env: &[(&str, String)]) -> Who {
        Who {
            keys: w.path(&format!("keys-{name}")),
            home: w.path(&format!("home-{name}")),
            iroh_env: iroh_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }
    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(rgit_bin());
        c.arg("--json").arg("-C").arg(dir).args(args);
        c.env("USER", "tester").env("HOSTNAME", "testbox");
        c.env("RGIT_INSECURE_FILE_KEYS", "1")
            .env("RGIT_KEY_DIR", &self.keys)
            .env("RGIT_HOME", &self.home);
        for (k, v) in &self.iroh_env {
            c.env(k, v);
        }
        c
    }
    fn run(&self, dir: &Path, args: &[&str]) -> (bool, Value, String) {
        let o = self.cmd(dir, args).output().unwrap();
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        let last = text.lines().last().unwrap_or("{}");
        let v: Value =
            serde_json::from_str(last).unwrap_or_else(|e| panic!("not JSON ({e}): {text:?}"));
        (
            o.status.success(),
            v,
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    }
    fn ok(&self, dir: &Path, args: &[&str]) -> Value {
        let (good, v, e) = self.run(dir, args);
        assert!(good && v["ok"] == true, "{args:?} failed: {v}\n{e}");
        v["data"].clone()
    }
    fn fail(&self, dir: &Path, args: &[&str]) -> Value {
        let (good, v, e) = self.run(dir, args);
        assert!(
            !good && v["ok"] == false,
            "{args:?} should have failed: {v}\n{e}"
        );
        v["error"].clone()
    }
}

struct Server {
    child: Child,
    dir: PathBuf,
    who: Who,
    iroh_url: String,
    stdout_path: PathBuf,
}

impl Server {
    fn start(w: &World, who: &Who, extra: &[&str]) -> Server {
        let dir = w.path("hosted");
        let pf = w.path("port");
        let stdout_path = w.path("host.stdout");
        let mut c = Command::new(rgit_bin());
        c.arg("--json").arg("serve").arg(&dir);
        c.args(["--init", "--listen", "127.0.0.1:0", "--port-file"])
            .arg(&pf)
            .args(extra);
        c.env("RGIT_INSECURE_FILE_KEYS", "1")
            .env("RGIT_KEY_DIR", &who.keys)
            .env("RGIT_HOME", &who.home);
        for (k, v) in &who.iroh_env {
            c.env(k, v);
        }
        c.stdout(fs::File::create(&stdout_path).unwrap())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let t0 = Instant::now();
        loop {
            let out = fs::read_to_string(&stdout_path).unwrap_or_default();
            if let Some(line) = out.lines().find(|l| l.contains("listening_iroh")) {
                let v: Value = serde_json::from_str(line).unwrap();
                return Server {
                    child,
                    dir,
                    who: who.clone(),
                    iroh_url: v["url"].as_str().unwrap().to_string(),
                    stdout_path,
                };
            }
            if let Some(st) = child.try_wait().unwrap() {
                let mut e = String::new();
                use std::io::Read;
                child.stderr.take().unwrap().read_to_string(&mut e).unwrap();
                panic!("server exited early: {st:?}\n{e}");
            }
            assert!(
                t0.elapsed() < Duration::from_secs(40),
                "server did not announce its iroh URL"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn invite(&self) -> String {
        self.who.ok(
            &self.dir,
            &[
                "invite",
                "create",
                "--role",
                "write",
                "--hint",
                &self.iroh_url,
            ],
        )["uri"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = &self.stdout_path;
    }
}

fn join(who: &Who, work_parent: &Path, uri: &str, dest: &Path) -> (bool, Value, String) {
    let mut c = who.cmd(work_parent, &["join", "-", dest.to_str().unwrap()]);
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(uri.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    let v: Value = serde_json::from_str(text.lines().last().unwrap_or("{}"))
        .unwrap_or_else(|e| panic!("not JSON ({e}): {text:?}"));
    (
        o.status.success(),
        v,
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

#[test]
fn forced_relay_end_to_end_with_the_cli() {
    let relay = TestRelay::https(0);
    let w = World::new();
    let pem = w.path("relay-ca.pem");
    write_pem(&pem, &relay.certs[0]);
    let env = [
        ("RGIT_IROH_RELAY", relay.url.clone()),
        ("RGIT_IROH_RELAY_CA", pem.to_string_lossy().to_string()),
        ("RGIT_IROH_DIRECT", "false".to_string()),
    ];
    let host = Who::new(&w, "host", &env);
    let srv = Server::start(&w, &host, &["--iroh"]);
    assert!(
        srv.iroh_url.starts_with("iroh://") && srv.iroh_url.contains("relay="),
        "{}",
        srv.iroh_url
    );
    assert!(
        !srv.iroh_url.contains("addr="),
        "relay-only URL must not leak addresses: {}",
        srv.iroh_url
    );

    let alice = Who::new(&w, "alice", &env);
    let work = w.path("work-alice");
    let (good, v, e) = join(&alice, w.root.path(), &srv.invite(), &work);
    assert!(good, "join over iroh failed: {v}\n{e}");
    assert_eq!(v["data"]["connection"]["state"], "relayed", "{v}");

    // A real commit with multi-MiB content, pushed over the relay.
    fs::write(work.join("blob.bin"), noise(5, 4 * 1024 * 1024)).unwrap();
    fs::write(work.join("hello.txt"), "hello over iroh\n").unwrap();
    alice.ok(&work, &["commit", "-m", "over the relay"]);
    let pushed = alice.ok(&work, &["push"]);
    assert_eq!(pushed["up_to_date"], false);
    assert_eq!(pushed["connection"]["transport"], "iroh");
    assert_eq!(pushed["connection"]["state"], "relayed", "{pushed}");
    assert_eq!(pushed["connection"]["used_direct"], false);

    // A second device joins and gets byte-identical files.
    let bob = Who::new(&w, "bob", &env);
    let work_b = w.path("work-bob");
    let (good, v, e) = join(&bob, w.root.path(), &srv.invite(), &work_b);
    assert!(good, "second join failed: {v}\n{e}");
    assert_same_tree(&work, &work_b);
    let fetched = bob.ok(&work_b, &["fetch"]);
    assert_eq!(fetched["connection"]["state"], "relayed");

    // `remote` lists the iroh remote like any other.
    let remotes = alice.ok(&work, &["remote"]);
    assert!(remotes["remotes"][0]["url"]
        .as_str()
        .unwrap()
        .starts_with("iroh://"));
}

#[test]
fn direct_loopback_end_to_end_with_the_cli() {
    let w = World::new();
    let env = [("RGIT_IROH_BIND", "127.0.0.1:0".to_string())];
    let host = Who::new(&w, "host", &env);
    let srv = Server::start(&w, &host, &["--iroh"]);
    // No relay configured: the URL carries direct addresses, otherwise it would be useless.
    assert!(
        srv.iroh_url.contains("addr=127.0.0.1:") && !srv.iroh_url.contains("relay="),
        "{}",
        srv.iroh_url
    );

    let alice = Who::new(&w, "alice", &env);
    let work = w.path("work-alice");
    let (good, v, e) = join(&alice, w.root.path(), &srv.invite(), &work);
    assert!(good, "{v}\n{e}");
    assert_eq!(v["data"]["connection"]["state"], "direct", "{v}");
    fs::write(work.join("a.txt"), "direct\n").unwrap();
    alice.ok(&work, &["commit", "-m", "x"]);
    let pushed = alice.ok(&work, &["push"]);
    assert_eq!(pushed["connection"]["state"], "direct", "{pushed}");
    assert_eq!(pushed["connection"]["used_relay"], false);
}

#[test]
fn unreachable_host_is_reported_as_unreachable_in_json() {
    let w = World::new();
    let env = [
        ("RGIT_IROH_RELAY", "https://127.0.0.1:9".to_string()),
        ("RGIT_IROH_DIRECT", "false".to_string()),
        ("RGIT_IROH_CONNECT_TIMEOUT", "3".to_string()),
    ];
    let me = Who::new(&w, "me", &env);
    let dir = w.path("proj");
    fs::create_dir_all(&dir).unwrap();
    me.ok(&dir, &["init"]);
    let url = format!(
        "iroh://{}",
        rustgit_transport_iroh::endpoint_id_of(&[5u8; 32])
    );
    me.ok(&dir, &["remote", "add", "origin", &url]);
    let started = Instant::now();
    let e = me.fail(&dir, &["fetch"]);
    assert!(started.elapsed() < Duration::from_secs(60));
    assert_eq!(e["code"], "host_unreachable", "{e}");
    assert_eq!(e["connection"]["state"], "unreachable", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("cannot reach iroh host"),
        "{e}"
    );
}

#[test]
fn serve_iroh_refuses_open_access() {
    let w = World::new();
    let o = Command::new(rgit_bin())
        .args([
            "serve",
            w.path("h").to_str().unwrap(),
            "--init",
            "--listen",
            "127.0.0.1:0",
            "--insecure-open-access",
            "--iroh",
        ])
        .env("RGIT_HOME", w.path("home"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr)
        .contains("cannot be combined with --insecure-open-access"));
}

#[test]
fn bad_iroh_urls_and_config_are_rejected_with_clear_messages() {
    let w = World::new();
    let me = Who::new(&w, "me", &[]);
    let dir = w.path("proj");
    fs::create_dir_all(&dir).unwrap();
    me.ok(&dir, &["init"]);
    let e = me.fail(&dir, &["remote", "add", "origin", "iroh://not-an-id"]);
    assert!(e["message"].as_str().unwrap().contains("64 hex"), "{e}");
    let e = me.fail(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            &format!("iroh://{}", "ab".repeat(32)),
        ],
    );
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("not a valid iroh endpoint id"),
        "{e}"
    );
    let bad = Who::new(
        &w,
        "bad",
        &[
            ("RGIT_IROH_PUBLIC_RELAY", "true".into()),
            ("RGIT_IROH_RELAY", "https://r.example".into()),
        ],
    );
    let e = bad.fail(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            &format!(
                "iroh://{}",
                rustgit_transport_iroh::endpoint_id_of(&[6u8; 32])
            ),
        ],
    );
    assert!(e["message"].as_str().unwrap().contains("both set"), "{e}");
}
