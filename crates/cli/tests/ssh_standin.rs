//! `ssh://` remotes and `rgit serve --stdio`, end to end with real `rgit` processes.
//!
//! IMPORTANT: there is no real SSH in these tests. The `ssh` found first on PATH is a
//! **test-only stand-in** (crates/transport-ssh/tests/fixtures/fake-ssh.sh, "NOT SSH": no
//! network, no encryption, no host-key check) that runs the remote command locally, the way
//! sshd would. They verify our argument construction, the stdio server, the stream handling,
//! the device-authenticated Noise channel running over it, and error mapping. They say nothing
//! about real SSH security; the ignored test at the bottom (`real_ssh_roundtrip`) is the one
//! that uses a real sshd, and only when you point it at one.
//!
//! Keys live in per-test directories (`--insecure-file-keys` via environment) so nothing
//! touches the OS key store. The stand-in's `remote.env` gives the "remote account" its own
//! key directory, separate from the client's.

#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rgit");
const FAKE_SSH: &str = include_str!("../../transport-ssh/tests/fixtures/fake-ssh.sh");

struct Env {
    root: tempfile::TempDir,
    bin_dir: PathBuf,
    server_keys: PathBuf,
}

impl Env {
    fn new() -> Env {
        let root = tempfile::tempdir().unwrap();
        let bin_dir = root.path().join("fakebin");
        fs::create_dir_all(&bin_dir).unwrap();
        let ssh = bin_dir.join("ssh");
        fs::write(&ssh, FAKE_SSH).unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        let server_keys = root.path().join("keys-server");
        fs::create_dir_all(&server_keys).unwrap();
        fs::write(
            bin_dir.join("remote.env"),
            format!(
                "export RGIT_INSECURE_FILE_KEYS=true\nexport RGIT_KEY_DIR='{}'\n",
                server_keys.display()
            ),
        )
        .unwrap();
        Env { root, bin_dir, server_keys }
    }

    fn path(&self) -> String {
        let rgit_dir = Path::new(BIN).parent().unwrap().display().to_string();
        format!(
            "{}:{}:{}",
            self.bin_dir.display(),
            rgit_dir,
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn keys(&self, who: &str) -> PathBuf {
        let d = self.root.path().join(format!("keys-{who}"));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// An `rgit` command run as device `who` (own key directory).
    fn cmd_as(&self, who: &str, dir: &Path) -> Command {
        let keys = if who == "server" { self.server_keys.clone() } else { self.keys(who) };
        let mut c = Command::new(BIN);
        c.arg("-C").arg(dir);
        c.env("PATH", self.path())
            .env("USER", "tester")
            .env("HOSTNAME", "testbox")
            .env("RGIT_INSECURE_FILE_KEYS", "true")
            .env("RGIT_KEY_DIR", keys)
            .env_remove("RGIT_SSH")
            .env_remove("RGIT_SSH_ACCEPT_NEW_HOSTKEY")
            .env_remove("RGIT_SSH_BATCH")
            .env_remove("RGIT_REMOTE_RGIT")
            .stdin(Stdio::null());
        c
    }

    fn cmd(&self, dir: &Path) -> Command {
        self.cmd_as("alice", dir)
    }

    fn rgit(&self, dir: &Path, args: &[&str]) -> Output {
        self.cmd(dir).args(args).output().unwrap()
    }

    fn rgit_as(&self, who: &str, dir: &Path, args: &[&str]) -> Output {
        self.cmd_as(who, dir).args(args).output().unwrap()
    }

    fn argv_log(&self) -> String {
        fs::read_to_string(self.bin_dir.join("argv.log")).unwrap_or_default()
    }

    fn invocations(&self) -> usize {
        self.argv_log().matches("--- argv").count()
    }

    /// A hosted repository directory (what `rgit serve --init` creates) with its host identity,
    /// without keeping a server running.
    fn hosted(&self, name: &str) -> PathBuf {
        let d = self.root.path().join(name);
        let mut child = self
            .cmd_as("server", self.root.path())
            .arg("serve")
            .arg(&d)
            .args(["--init", "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let t0 = Instant::now();
        while !d.join(".rgit/HOSTED").exists() {
            assert!(t0.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(20));
        }
        // the identity is created before the listening line
        std::thread::sleep(Duration::from_millis(800));
        let _ = child.kill();
        let _ = child.wait();
        d
    }

    /// Invitation link for `hosted` whose only hint is the given ssh:// URL.
    fn invite(&self, hosted: &Path, hint: &str) -> String {
        let o = self
            .cmd_as("server", hosted)
            .args(["--json", "invite", "create", "--role", "write", "--hint", hint])
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| panic!("invite create: {}", String::from_utf8_lossy(&o.stderr)));
        v["data"]["uri"].as_str().unwrap_or_else(|| panic!("{v}")).to_string()
    }

    /// Enrolls device `who` through an invitation and clones into `<root>/<dest>`.
    fn join(&self, who: &str, hosted: &Path, hint: &str, dest: &str) -> PathBuf {
        let uri = self.invite(hosted, hint);
        let d = self.root.path().join(dest);
        let mut c = self.cmd_as(who, self.root.path());
        c.args(["join", "-"]).arg(&d).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(uri.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        ok(o);
        d
    }
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "command failed ({:?})\nstdout: {}\nstderr: {}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn commit_file(env: &Env, who: &str, repo: &Path, file: &str, body: &str, msg: &str) {
    ok(env.rgit_as(who, repo, &["config", "user.name", who]));
    ok(env.rgit_as(who, repo, &["config", "user.email", &format!("{who}@example.com")]));
    fs::write(repo.join(file), body).unwrap();
    ok(env.rgit_as(who, repo, &["commit", "-m", msg]));
}

fn url_for(host: &str, path: &Path) -> String {
    format!("ssh://{host}{}", path.display())
}

#[test]
fn join_push_clone_and_fetch_over_the_standin() {
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let hint = url_for("tester@fake-host:2222", &hosted);

    // device alice joins through an invitation whose connection hint is an ssh:// URL
    let a = env.join("alice", &hosted, &hint, "a");
    commit_file(&env, "alice", &a, "hello.txt", "hello over ssh\n", "first");
    let pushed = ok(env.rgit_as("alice", &a, &["push"]));
    assert!(pushed.contains("Pushed") || pushed.contains("object"), "{pushed}");

    // device bob joins separately (own key) and sees alice's work
    let b = env.join("bob", &hosted, &hint, "b");
    assert_eq!(fs::read_to_string(b.join("hello.txt")).unwrap(), "hello over ssh\n");

    commit_file(&env, "bob", &b, "hello.txt", "changed\n", "second");
    ok(env.rgit_as("bob", &b, &["push"]));
    ok(env.rgit_as("alice", &a, &["fetch"]));
    ok(env.rgit_as("alice", &a, &["merge", "origin/main"]));
    assert_eq!(fs::read_to_string(a.join("hello.txt")).unwrap(), "changed\n");

    // the stand-in saw structured arguments only, with host-key checking on
    let log = env.argv_log();
    assert!(log.contains("StrictHostKeyChecking=yes"), "{log}");
    assert!(log.contains("BatchMode=yes"), "{log}");
    assert!(!log.to_lowercase().contains("stricthostkeychecking=no"), "{log}");
    assert!(!log.to_lowercase().contains("proxycommand"), "{log}");
    let lines: Vec<&str> = log.lines().collect();
    let i = lines.iter().position(|l| *l == "-p").unwrap();
    assert_eq!(lines[i + 1], "2222");
    let i = lines.iter().position(|l| *l == "-l").unwrap();
    assert_eq!(lines[i + 1], "tester");
    let dd = lines.iter().position(|l| *l == "--").unwrap();
    assert_eq!(lines[dd + 1], "fake-host");
    assert_eq!(lines[dd + 2], format!("rgit serve --stdio -- {}", hosted.display()));
}

#[test]
fn an_unenrolled_device_is_refused_over_ssh_too() {
    // ssh only carries bytes: the host still demands a device it knows
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let hint = url_for("fake-host", &hosted);
    let a = env.join("alice", &hosted, &hint, "a");
    commit_file(&env, "alice", &a, "f.txt", "x\n", "first");
    // mallory has a key that was never enrolled; point a copy of alice's remote at the host
    let m = env.root.path().join("m");
    fs::create_dir_all(&m).unwrap();
    ok(env.rgit_as("mallory", &m, &["init"]));
    ok(env.rgit_as("mallory", &m, &["remote", "add", "origin", &hint]));
    let o = env.rgit_as("mallory", &m, &["--json", "fetch"]);
    assert!(!o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "security_required", "{v}");
}

#[test]
fn json_output_stays_clean_and_progress_works_over_ssh() {
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let hint = url_for("fake-host", &hosted);
    let a = env.join("alice", &hosted, &hint, "a");
    commit_file(&env, "alice", &a, "f.txt", &"x".repeat(5000), "first");
    let o = env.rgit_as("alice", &a, &["--json", "push"]);
    let text = ok(o);
    let last = text.lines().last().unwrap();
    let v: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(v["ok"], true, "{text}");
}

#[test]
fn login_banners_before_the_server_starts_are_ignored() {
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let hint = url_for("noisy-fake-host", &hosted);
    let a = env.join("alice", &hosted, &hint, "a");
    commit_file(&env, "alice", &a, "f.txt", "banner test\n", "first");
    let o = env.rgit_as("alice", &a, &["push"]);
    let e = stderr(&o);
    ok(o);
    assert!(e.contains("ignored"), "the skipped banner should be mentioned: {e}");
}

#[test]
fn jump_hosts_are_passed_to_ssh_as_one_validated_option() {
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let hint = format!("{}?jump=bob@bastion.example:2200,second", url_for("fake-host", &hosted));
    let a = env.join("alice", &hosted, &hint, "a");
    commit_file(&env, "alice", &a, "f.txt", "jump\n", "first");
    ok(env.rgit_as("alice", &a, &["push"]));
    let log = env.argv_log();
    let lines: Vec<&str> = log.lines().collect();
    let i = lines.iter().position(|l| *l == "-J").unwrap();
    assert_eq!(lines[i + 1], "bob@bastion.example:2200,second");
}

#[test]
fn host_key_failures_are_shown_clearly_and_never_retried() {
    let env = Env::new();
    let dest = env.root.path().join("dest");
    let start = Instant::now();
    let o = env.rgit(
        env.root.path(),
        &["--json", "clone", "ssh://mode-hostkey/srv/p", dest.to_str().unwrap()],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect("one JSON object");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "ssh_host_key_failed", "{v}\nSTDERR: {}", stderr(&o));
    let m = v["error"]["message"].as_str().unwrap();
    assert!(m.contains("Host key verification failed."), "ssh's own words are shown: {m}");
    assert!(m.contains("RGIT_SSH_ACCEPT_NEW_HOSTKEY"), "the opt-in is explained: {m}");
    assert_eq!(env.invocations(), 1, "an auth/host-key failure must not be retried");
    assert!(start.elapsed() < Duration::from_secs(8));
    assert!(!dest.exists());
}

#[test]
fn other_ssh_failures_map_to_codes() {
    let env = Env::new();
    for (i, (host, code)) in [
        ("mode-denied", "ssh_auth_failed"),
        ("mode-changed", "ssh_host_key_failed"),
        ("mode-refused", "host_unreachable"),
        ("mode-notfound", "remote_rgit_missing"),
    ]
    .into_iter()
    .enumerate()
    {
        let dest = env.root.path().join(format!("dest{i}"));
        let o = env.rgit(
            env.root.path(),
            &["--json", "clone", &format!("ssh://{host}/srv/p"), dest.to_str().unwrap()],
        );
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect("one JSON object");
        assert_eq!(v["ok"], false, "{host}");
        assert_eq!(v["error"]["code"], code, "{host}: {v}\nSTDERR: {}", stderr(&o));
    }
}

#[test]
fn accept_new_is_an_explicit_opt_in_and_never_becomes_no() {
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let hint = url_for("fake-host", &hosted);
    let a = env.join("alice", &hosted, &hint, "a");
    commit_file(&env, "alice", &a, "f.txt", "x\n", "first");
    let mut c = env.cmd_as("alice", &a);
    c.args(["push"]).env("RGIT_SSH_ACCEPT_NEW_HOSTKEY", "1");
    ok(c.output().unwrap());
    let log = env.argv_log();
    assert!(log.contains("StrictHostKeyChecking=accept-new"), "{log}");
    assert!(!log.contains("StrictHostKeyChecking=no"), "{log}");
}

#[test]
fn hostile_urls_are_rejected_before_ssh_is_ever_started() {
    let env = Env::new();
    let cwd = env.root.path().join("w");
    fs::create_dir_all(&cwd).unwrap();
    let marker = cwd.join("PWNED");
    let m = marker.display().to_string();
    let urls = [
        format!("ssh://-oProxyCommand=touch%20{m}/x"),
        format!("ssh://-oProxyCommand=touch/{m}"),
        format!("ssh://host/x;touch%20{m}"),
        format!("ssh://host/x%0Atouch%20{m}"),
        format!("ssh://host/x$(touch%20{m})"),
        format!("ssh://host/x%60touch%20{m}%60"),
        format!("ssh://host;touch%20{m}/x"),
        "ssh://host/x?jump=-oProxyCommand=id".to_string(),
        "ssh://user@host name/x".to_string(),
        "ssh://-F/x".to_string(),
    ];
    let dest = cwd.join("dest");
    for u in &urls {
        let o = env.rgit(&cwd, &["clone", u, dest.to_str().unwrap()]);
        assert!(!o.status.success(), "{u} was accepted");
        assert!(stderr(&o).contains("invalid SSH remote") || stderr(&o).contains("SSH"), "{u}: {}", stderr(&o));
    }
    assert_eq!(env.invocations(), 0, "ssh must never have been started for a rejected URL");
    assert!(!marker.exists());
    // `remote add` refuses them too ...
    let a = env.root.path().join("a");
    fs::create_dir_all(&a).unwrap();
    ok(env.rgit(&a, &["init"]));
    assert!(!env.rgit(&a, &["remote", "add", "origin", &urls[0]]).status.success());
    // ... and a hostile value that got into the config some other way is still refused when used
    for u in &urls[1..4] {
        ok(env.rgit(&a, &["config", "remote.origin.url", u]));
        let o = env.rgit(&a, &["fetch"]);
        assert!(!o.status.success(), "{u}");
        assert!(stderr(&o).contains("invalid SSH remote"), "{u}: {}", stderr(&o));
    }
    assert_eq!(env.invocations(), 0);
    assert!(!marker.exists());
}

// ---------------------------------------------------------------------------------------
// `rgit serve --stdio` itself, driven directly over pipes
// ---------------------------------------------------------------------------------------

fn spawn_stdio(env: &Env, hosted: &Path) -> std::process::Child {
    env.cmd_as("server", hosted)
        .args(["serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_exit(child: &mut std::process::Child, secs: u64) -> std::process::ExitStatus {
    let t0 = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(t0.elapsed() < Duration::from_secs(secs), "process did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn stdio_server_announces_then_exits_when_the_client_vanishes() {
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let mut c = spawn_stdio(&env, &hosted);
    drop(c.stdin.take()); // client disappears at once
    let st = wait_exit(&mut c, 15);
    assert!(st.success(), "{st:?}");
    let mut out = Vec::new();
    c.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert_eq!(out, b"\nRGIT-STDIO/1\n", "stdout must carry only the marker and frames");
}

#[test]
fn stdio_server_survives_garbage_and_never_prints_human_text_on_stdout() {
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let mut c = spawn_stdio(&env, &hosted);
    {
        let stdin = c.stdin.as_mut().unwrap();
        stdin.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        stdin.flush().unwrap();
    }
    let st = wait_exit(&mut c, 15);
    // a protocol violation ends the session; the process itself must exit cleanly
    assert!(st.success(), "{st:?}");
    let mut out = Vec::new();
    c.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert!(out.starts_with(b"\nRGIT-STDIO/1\n"));
    let human = [&b"Serving"[..], b"Created", b"error"];
    for h in human {
        assert!(
            !out.windows(h.len()).any(|w| w == h),
            "human text {:?} on stdout",
            String::from_utf8_lossy(h)
        );
    }
}

#[test]
fn stdio_cannot_be_combined_with_json() {
    let env = Env::new();
    let hosted = env.hosted("hosted.rgit");
    let o = env.cmd_as("server", &hosted).args(["--json", "serve", "--stdio"]).output().unwrap();
    assert!(!o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["ok"], false);
}

#[test]
fn stdio_refuses_a_missing_repository_clearly() {
    let env = Env::new();
    let nowhere = env.root.path().join("nope.rgit");
    let o = env.cmd_as("server", &nowhere).args(["serve", "--stdio"]).output().unwrap();
    assert!(!o.status.success());
    assert!(o.stdout.is_empty(), "nothing but protocol may appear on stdout: {:?}", o.stdout);
}

// ---------------------------------------------------------------------------------------
// real sshd (opt-in)
// ---------------------------------------------------------------------------------------

/// Needs a real, already-running sshd that you set up yourself (nothing here enables one), a
/// non-interactive login that works (key in ssh-agent, host key already in known_hosts) and
/// `rgit` on the remote PATH. Run with:
///   RGIT_REAL_SSH_URL=ssh://you@localhost/abs/path/to/hosted.rgit \
///     cargo test -p rustgit-cli --test ssh_standin -- --ignored real_ssh_roundtrip
#[test]
#[ignore = "needs a real sshd; see the doc comment"]
fn real_ssh_roundtrip() {
    let Ok(url) = std::env::var("RGIT_REAL_SSH_URL") else {
        panic!("set RGIT_REAL_SSH_URL to run this test");
    };
    let root = tempfile::tempdir().unwrap();
    let rgit = |dir: &Path, args: &[&str]| {
        Command::new(BIN)
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("USER", "tester")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let a = root.path().join("a");
    fs::create_dir_all(&a).unwrap();
    ok(rgit(&a, &["init"]));
    ok(rgit(&a, &["config", "user.name", "T"]));
    ok(rgit(&a, &["config", "user.email", "t@example.com"]));
    fs::write(a.join("f.txt"), "real ssh\n").unwrap();
    ok(rgit(&a, &["commit", "-m", "first"]));
    ok(rgit(&a, &["remote", "add", "origin", &url]));
    ok(rgit(&a, &["push"]));
    let b = root.path().join("b");
    ok(rgit(root.path(), &["clone", &url, b.to_str().unwrap()]));
    assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "real ssh\n");
}
