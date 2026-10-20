//! Argument construction (injection safety) and the client against a **test-only stand-in**
//! for `ssh` (tests/fixtures/fake-ssh.sh: NOT SSH, see its header), plus tests that run the
//! real system `ssh` client where that is possible without a server.

#![cfg(unix)]
#![allow(clippy::field_reassign_with_default)]

use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustgit_protocol::{Channel, Transport};
use rustgit_transport_ssh::{
    build_args, remote_command, HostKeyPolicy, SshOptions, SshTarget, SshTransport,
};

const FAKE: &str = include_str!("fixtures/fake-ssh.sh");

/// Installs the stand-in as `<dir>/ssh` and returns its path.
fn install_fake(dir: &Path) -> PathBuf {
    let p = dir.join("ssh");
    std::fs::write(&p, FAKE).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn fake_transport(dir: &Path, url: &str) -> SshTransport {
    let mut o = SshOptions::default();
    o.program = install_fake(dir).into_os_string();
    o.login_timeout = Duration::from_secs(3);
    SshTransport::from_url(url, o).unwrap()
}

fn argv_of(args: &[OsString]) -> Vec<String> {
    args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
}

fn args_for(url: &str, opts: &SshOptions) -> Vec<String> {
    argv_of(&build_args(&SshTarget::parse(url).unwrap(), opts, Duration::from_secs(10)).unwrap())
}

// ---------------------------------------------------------------------------------------
// argument construction
// ---------------------------------------------------------------------------------------

#[test]
fn untrusted_values_only_ever_follow_dashdash_or_their_option() {
    let t = SshTarget::parse("ssh://ann@srv.example:2222/srv/my%20proj?jump=bob@jump.example:2200").unwrap();
    let a = argv_of(&build_args(&t, &SshOptions::default(), Duration::from_secs(10)).unwrap());
    let dd = a.iter().position(|x| x == "--").expect("-- present");
    // exactly: -- host remote-command, and nothing after
    assert_eq!(a.len(), dd + 3, "{a:?}");
    assert_eq!(a[dd + 1], "srv.example");
    assert_eq!(a[dd + 2], "rgit serve --stdio -- '/srv/my proj'");
    // values attached to their own options
    let i = a.iter().position(|x| x == "-l").unwrap();
    assert_eq!(a[i + 1], "ann");
    let i = a.iter().position(|x| x == "-p").unwrap();
    assert_eq!(a[i + 1], "2222");
    let i = a.iter().position(|x| x == "-J").unwrap();
    assert_eq!(a[i + 1], "bob@jump.example:2200");
    // nothing that could run a local command or reconfigure ssh from a URL
    for x in &a {
        assert!(!x.to_lowercase().contains("proxycommand"), "{x}");
        assert!(!x.to_lowercase().contains("localcommand=y"), "{x}");
    }
    assert!(a.iter().any(|x| x == "ClearAllForwardings=yes"));
    assert!(a.iter().any(|x| x == "ForwardAgent=no"));
}

#[test]
fn host_key_checking_is_never_disabled() {
    for (policy, batch, expect) in [
        (HostKeyPolicy::Ask, false, "StrictHostKeyChecking=ask"),
        (HostKeyPolicy::Strict, true, "StrictHostKeyChecking=yes"),
        (HostKeyPolicy::AcceptNew, false, "StrictHostKeyChecking=accept-new"),
    ] {
        let mut o = SshOptions::default();
        o.host_key_policy = policy;
        o.batch_mode = batch;
        let a = args_for("ssh://h/x", &o);
        assert!(a.iter().any(|x| x == expect), "{a:?}");
        for x in &a {
            let l = x.to_lowercase();
            assert!(!l.contains("stricthostkeychecking=no"), "{x}");
            assert!(!l.contains("stricthostkeychecking=off"), "{x}");
            assert!(!l.contains("userknownhostsfile"), "{x}");
        }
        assert!(a.iter().any(|x| x == if batch { "BatchMode=yes" } else { "BatchMode=no" }));
    }
}

#[test]
fn env_cannot_select_a_weaker_host_key_policy() {
    // from_env only ever yields Ask / Strict / AcceptNew; this documents it by construction
    let o = SshOptions::from_env(false).unwrap();
    assert!(matches!(o.host_key_policy, HostKeyPolicy::Strict | HostKeyPolicy::AcceptNew));
    let o = SshOptions::from_env(true).unwrap();
    assert!(matches!(o.host_key_policy, HostKeyPolicy::Ask | HostKeyPolicy::AcceptNew | HostKeyPolicy::Strict));
}

#[test]
fn hostile_remote_program_names_are_rejected() {
    let t = SshTarget::parse("ssh://h/x").unwrap();
    for p in ["rgit; touch pwned", "$(id)", "`id`", "rgit\nid", "-oProxyCommand=id", "r g", "rgit|cat"] {
        let mut o = SshOptions::default();
        o.remote_program = p.into();
        assert!(remote_command(&t, &o).is_err(), "{p:?}");
        assert!(SshTransport::new(t.clone(), o).is_err(), "{p:?}");
    }
}

#[test]
fn the_user_supplied_files_are_option_values() {
    let mut o = SshOptions::default();
    o.config_file = Some("-oProxyCommand=evil".into()); // odd file name, still just a value
    o.identity_file = Some("/tmp/id".into());
    let a = args_for("ssh://h/x", &o);
    let i = a.iter().position(|x| x == "-F").unwrap();
    assert_eq!(a[i + 1], "-oProxyCommand=evil");
    assert!(a.iter().position(|x| x == "--").unwrap() > i + 1);
}

// ---------------------------------------------------------------------------------------
// the real `ssh` client interpreting our argv (no connection is made: `ssh -G`)
// ---------------------------------------------------------------------------------------

fn real_ssh_available() -> bool {
    std::process::Command::new("ssh")
        .arg("-V")
        .output()
        .map(|o| o.status.success() || !o.stderr.is_empty())
        .unwrap_or(false)
}

#[test]
fn real_ssh_resolves_our_arguments_as_intended_and_overrides_a_hostile_config() {
    if !real_ssh_available() {
        eprintln!("SKIPPED: no `ssh` on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config");
    // a user config that would weaken everything for this host
    std::fs::write(
        &cfg,
        "Host *\n  StrictHostKeyChecking no\n  ForwardAgent yes\n  ForwardX11 yes\n  PermitLocalCommand yes\n  LocalCommand touch /tmp/rgit-should-never-run\n  LocalForward 9999 127.0.0.1:9\n",
    )
    .unwrap();
    let mut o = SshOptions::default();
    o.config_file = Some(cfg);
    let t = SshTarget::parse("ssh://ann@srv.example:2222/srv/proj?jump=bob@jump.example:2200").unwrap();
    let mut args = vec![OsString::from("-G")];
    args.extend(build_args(&t, &o, Duration::from_secs(10)).unwrap());
    let out = std::process::Command::new("ssh").args(&args).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(out.status.success(), "ssh -G failed: {}", String::from_utf8_lossy(&out.stderr));
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k} ")))
            .unwrap_or_else(|| panic!("no {k} in:\n{text}"))
            .to_string()
    };
    assert_eq!(get("hostname"), "srv.example");
    assert_eq!(get("user"), "ann");
    assert_eq!(get("port"), "2222");
    assert_eq!(get("proxyjump"), "bob@jump.example:2200");
    assert_eq!(get("batchmode"), "no");
    assert_ne!(get("stricthostkeychecking"), "false", "host key checking was disabled by the config");
    assert_eq!(get("stricthostkeychecking"), "ask");
    assert_eq!(get("forwardagent"), "no");
    assert_eq!(get("forwardx11"), "no");
    assert_eq!(get("permitlocalcommand"), "no");
    assert_eq!(get("clearallforwardings"), "yes");
    assert_eq!(get("requesttty"), "false");
}

#[test]
fn real_ssh_failures_are_classified_without_a_server() {
    if !real_ssh_available() {
        eprintln!("SKIPPED: no `ssh` on PATH");
        return;
    }
    let t = SshTransport::from_url(
        "ssh://nobody@127.0.0.1:1/x",
        SshOptions {
            batch_mode: true,
            host_key_policy: HostKeyPolicy::Strict,
            login_timeout: Duration::from_secs(10),
            ..SshOptions::default()
        },
    )
    .unwrap();
    let e = t.connect(Duration::from_secs(5)).err().expect("port 1 must refuse");
    let m = e.to_string();
    assert!(
        e.kind() == ErrorKind::ConnectionRefused,
        "kind {:?}: {m}",
        e.kind()
    );
    assert!(m.starts_with("ssh connection refused"), "{m}");
    assert!(!m.to_lowercase().contains("usage:") && !m.contains("Bad configuration"), "{m}");
}

#[test]
fn a_missing_ssh_client_is_reported_clearly() {
    let mut o = SshOptions::default();
    o.program = "/nonexistent/definitely-not-ssh".into();
    let t = SshTransport::from_url("ssh://h/x", o).unwrap();
    let e = t.connect(Duration::from_secs(1)).err().unwrap();
    assert_eq!(e.kind(), ErrorKind::NotFound);
    assert!(e.to_string().starts_with("ssh client not found"), "{e}");
}

// ---------------------------------------------------------------------------------------
// the transport against the stand-in (NOT real ssh)
// ---------------------------------------------------------------------------------------

#[test]
fn standin_echo_session_carries_frames_both_ways_and_skips_login_banners() {
    for host in ["mode-echo", "noisy-mode-echo"] {
        let tmp = tempfile::tempdir().unwrap();
        let t = fake_transport(tmp.path(), &format!("ssh://{host}/x"));
        let io = t.connect(Duration::from_secs(5)).unwrap();
        let mut ch = Channel::new(io, Some(Duration::from_secs(5)));
        ch.set_max_frame(1024 * 1024);
        for payload in [&b"one"[..], &[0u8; 100_000][..], &b""[..]] {
            ch.send(payload).unwrap();
            assert_eq!(ch.recv(Some(Duration::from_secs(5))).unwrap(), payload);
        }
        ch.shutdown();
    }
}

#[test]
fn standin_argv_log_matches_what_build_args_produced() {
    let tmp = tempfile::tempdir().unwrap();
    let t = fake_transport(tmp.path(), "ssh://ann@mode-echo:2201/srv/p?jump=j1,u@j2:22");
    let io = t.connect(Duration::from_secs(5)).unwrap();
    drop(io);
    let log = std::fs::read_to_string(tmp.path().join("argv.log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], "--- argv");
    let dd = lines.iter().position(|l| *l == "--").unwrap();
    assert_eq!(lines[dd + 1], "mode-echo");
    assert_eq!(lines[dd + 2], "rgit serve --stdio -- /srv/p");
    assert!(lines.contains(&"j1,u@j2:22"));
    assert!(!lines.iter().any(|l| l.to_lowercase().contains("stricthostkeychecking=no") && !l.contains("accept")));
}

fn connect_err(host_url: &str, tmp: &Path) -> std::io::Error {
    let t = fake_transport(tmp, host_url);
    match t.connect(Duration::from_secs(5)) {
        Ok(_) => panic!("{host_url} unexpectedly connected"),
        Err(e) => e,
    }
}

#[test]
fn standin_failures_become_clear_non_retryable_errors() {
    let tmp = tempfile::tempdir().unwrap();

    let e = connect_err("ssh://mode-hostkey/x", tmp.path());
    assert_eq!(e.kind(), ErrorKind::PermissionDenied);
    assert!(e.to_string().starts_with("ssh host key verification failed"), "{e}");
    assert!(e.to_string().contains("Host key verification failed."), "ssh's own words must be shown: {e}");

    let e = connect_err("ssh://mode-changed/x", tmp.path());
    assert_eq!(e.kind(), ErrorKind::PermissionDenied);
    assert!(e.to_string().contains("CHANGED"), "{e}");
    assert!(e.to_string().contains("ssh-keygen -R"), "{e}");

    let e = connect_err("ssh://mode-denied/x", tmp.path());
    assert_eq!(e.kind(), ErrorKind::PermissionDenied);
    assert!(e.to_string().starts_with("ssh authentication failed"), "{e}");

    let e = connect_err("ssh://mode-refused/x", tmp.path());
    assert_eq!(e.kind(), ErrorKind::ConnectionRefused);

    let e = connect_err("ssh://mode-resolve/x", tmp.path());
    assert_eq!(e.kind(), ErrorKind::NotFound);

    let e = connect_err("ssh://mode-notfound/x", tmp.path());
    assert_eq!(e.kind(), ErrorKind::NotFound);
    assert!(e.to_string().contains("RGIT_REMOTE_RGIT"), "{e}");
}

#[test]
fn standin_silence_times_out_and_the_child_is_killed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut o = SshOptions::default();
    o.program = install_fake(tmp.path()).into_os_string();
    o.login_timeout = Duration::from_millis(300);
    let t = SshTransport::from_url("ssh://mode-silent/x", o).unwrap();
    let start = Instant::now();
    let e = t.connect(Duration::from_millis(300)).err().unwrap();
    assert_eq!(e.kind(), ErrorKind::TimedOut, "{e}");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn standin_dying_mid_session_reports_why() {
    let tmp = tempfile::tempdir().unwrap();
    let t = fake_transport(tmp.path(), "ssh://mode-dies/x");
    let io = t.connect(Duration::from_secs(5)).unwrap();
    let mut ch = Channel::new(io, Some(Duration::from_secs(5)));
    let e = ch.recv(Some(Duration::from_secs(5))).unwrap_err();
    let m = e.to_string();
    assert!(m.contains("closed by the server") || m.contains("closed by remote host"), "{m}");
}
