//! `rgit serve --advertise` and `rgit discover` as two real processes on this machine's
//! multicast-capable interfaces. If mDNS does not work here (OS firewall, "local network"
//! permission, VPN, a network that filters multicast) the tests SKIP with a message rather than
//! fail; read the output of `--nocapture` to see whether they actually ran.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::net::IpAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_rgit");

fn cmd(keys: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("RGIT_INSECURE_FILE_KEYS", "true")
        .env("RGIT_KEY_DIR", keys)
        .env("USER", "tester")
        .env("HOSTNAME", "testbox")
        .stdin(Stdio::null());
    c
}

struct Serve {
    child: Child,
    project: String,
    port: u16,
    instance: String,
    hint: String,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Lines of a child's stdout through a channel (so waits can time out).
fn lines_of(child: &mut Child) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let out = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for l in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    rx
}

fn start_serve(tmp: &Path, listen: &str) -> Serve {
    let keys = tmp.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let dir = tmp.join("hosted.rgit");
    let mut child = cmd(&keys)
        .args(["--json", "serve", "--init"])
        .arg(&dir)
        .args(["--listen", listen, "--advertise"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let rx = lines_of(&mut child);
    let (mut project, mut port, mut instance, mut hint) = (String::new(), 0u16, String::new(), String::new());
    let t0 = Instant::now();
    while instance.is_empty() {
        let l = rx
            .recv_timeout(Duration::from_secs(30).saturating_sub(t0.elapsed()))
            .expect("serve did not report advertising");
        let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
        match v["event"].as_str() {
            Some("listening") => {
                project = v["project"].as_str().unwrap().to_string();
                port = v["addr"].as_str().unwrap().rsplit(':').next().unwrap().parse().unwrap();
            }
            Some("advertising") => {
                instance = v["instance"].as_str().unwrap().to_string();
                hint = v["project_hint"].as_str().unwrap().to_string();
            }
            _ => {}
        }
    }
    std::mem::forget(rx);
    Serve { child, project, port, instance, hint }
}

fn discover_json(keys: &Path, extra: &[&str]) -> (Vec<Value>, Value) {
    let o = cmd(keys).args(["--json", "discover"]).args(extra).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    let mut events = Vec::new();
    let mut last = Value::Null;
    for l in text.lines() {
        let v: Value = serde_json::from_str(l).unwrap_or_else(|_| panic!("not JSON: {l}"));
        if v.get("event").is_some() {
            events.push(v);
        } else {
            last = v;
        }
    }
    (events, last)
}

fn find_host(hosts: &[Value], port: u16) -> Option<&Value> {
    hosts.iter().find(|h| h["port"] == port)
}

fn private_lan_ip() -> Option<IpAddr> {
    // `ipconfig`-free: ask the OS through a UDP socket bound to each candidate is overkill;
    // use the same enumeration the library uses via a tiny shell-out.
    let out = Command::new("ifconfig").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut name = String::new();
    for l in text.lines() {
        if !l.starts_with(char::is_whitespace) {
            name = l.split(':').next().unwrap_or("").to_string();
        } else if let Some(rest) = l.trim().strip_prefix("inet ") {
            if name.starts_with("utun") || name.starts_with("awdl") || name.starts_with("lo") {
                continue;
            }
            let ip: IpAddr = rest.split_whitespace().next()?.parse().ok()?;
            if let IpAddr::V4(a) = ip {
                if a.is_private() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

#[test]
fn serve_advertises_minimal_metadata_and_discover_finds_it_on_loopback() {
    let tmp = tempfile::tempdir().unwrap();
    let serve = start_serve(tmp.path(), "127.0.0.1:0");
    let keys = tmp.path().join("clientkeys");
    std::fs::create_dir_all(&keys).unwrap();

    let (events, last) = discover_json(
        &keys,
        &["--timeout", "8", "--include-loopback", "--project", &serve.project],
    );
    assert_eq!(last["ok"], true, "{last}");
    let hosts = last["data"]["hosts"].as_array().unwrap();
    let Some(h) = find_host(hosts, serve.port) else {
        eprintln!("SKIPPED: `rgit discover` did not see the advertisement within 8 s on loopback. mDNS looks blocked or unavailable here, so this path was NOT exercised.");
        return;
    };
    // streaming: a host event arrived before the final result
    assert!(events.iter().any(|e| e["event"] == "host" && e["host"]["port"] == serve.port), "{events:?}");
    // content and shape (docs/gui-interface.md)
    assert_eq!(h["instance"], serve.instance);
    assert_eq!(h["project_hint"], serve.hint);
    assert_eq!(h["matches_project"], true);
    assert_eq!(h["verified"], false);
    assert_eq!(h["source"], "mdns");
    assert_eq!(h["reachable"], true);
    assert_eq!(h["addresses"][0], "127.0.0.1");
    assert_eq!(h["urls"][0], format!("tcp://127.0.0.1:{}", serve.port));
    assert!(h["protocol_version"].as_u64().unwrap() >= 1);
    // minimal metadata: nothing in the record identifies the project, the user or the machine
    let all = h.to_string();
    for secret in [serve.project.as_str(), &serve.project[..8], "hosted", "tester", "testbox"] {
        assert!(!all.contains(secret), "{secret:?} leaked into {all}");
    }
    // a different project is not matched
    let (_, other) = discover_json(
        &keys,
        &["--timeout", "4", "--include-loopback", "--project", "00000000-0000-0000-0000-000000000000"],
    );
    let h2 = find_host(other["data"]["hosts"].as_array().unwrap(), serve.port).expect("still listed");
    assert_eq!(h2["matches_project"], false);
    // loopback is hidden unless asked for
    let (_, hidden) = discover_json(&keys, &["--timeout", "4"]);
    assert!(find_host(hidden["data"]["hosts"].as_array().unwrap(), serve.port).is_none());
}

#[test]
fn a_wildcard_server_is_found_on_the_lan_interface() {
    let Some(ip) = private_lan_ip() else {
        eprintln!("SKIPPED: no private LAN IPv4 address on this machine");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let serve = start_serve(tmp.path(), "0.0.0.0:0");
    let keys = tmp.path().join("clientkeys");
    std::fs::create_dir_all(&keys).unwrap();
    let (_, last) = discover_json(&keys, &["--timeout", "8"]);
    let hosts = last["data"]["hosts"].as_array().unwrap();
    let Some(h) = find_host(hosts, serve.port) else {
        eprintln!("SKIPPED: the wildcard server was not seen on {ip} within 8 s (mDNS blocked?); NOT exercised.");
        return;
    };
    let addrs: Vec<&str> = h["addresses"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
    eprintln!("wildcard server advertised addresses: {addrs:?}");
    assert!(addrs.contains(&ip.to_string().as_str()), "{addrs:?}");
    assert!(addrs.iter().all(|a| !a.starts_with("127.")), "loopback published: {addrs:?}");
    assert!(addrs.iter().all(|a| !a.contains(':')), "IPv6 published for an IPv4-only listener: {addrs:?}");
    assert_eq!(h["reachable"], true);
}

#[test]
fn sigterm_sends_the_goodbye_and_watch_reports_the_host_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut serve = start_serve(tmp.path(), "127.0.0.1:0");
    let keys = tmp.path().join("clientkeys");
    std::fs::create_dir_all(&keys).unwrap();
    let mut watch = cmd(&keys)
        .args(["--json", "discover", "--watch", "--include-loopback", "--no-probe"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let rx = lines_of(&mut watch);
    let mut seen = false;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(10) {
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(250)) {
            let v: Value = serde_json::from_str(&l).unwrap();
            if v["event"] == "host" && v["host"]["port"] == serve.port {
                seen = true;
                break;
            }
        }
    }
    if !seen {
        eprintln!("SKIPPED: watch did not see the host within 10 s (mDNS blocked?); goodbye path NOT exercised.");
        let _ = watch.kill();
        let _ = watch.wait();
        return;
    }
    // polite termination, as an app or `kill` would do it
    let st = Command::new("kill").arg("-TERM").arg(serve.child.id().to_string()).status().unwrap();
    assert!(st.success());
    let mut gone = false;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(10) {
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(250)) {
            let v: Value = serde_json::from_str(&l).unwrap();
            if v["event"] == "lost" && v["instance"] == serve.instance {
                gone = true;
                break;
            }
        }
    }
    let _ = watch.kill();
    let _ = watch.wait();
    let _ = serve.child.wait();
    assert!(gone, "no goodbye after SIGTERM (the advertisement would linger for minutes)");
}

#[test]
fn discover_rejects_nonsense_timeouts_and_works_with_no_hosts() {
    let tmp = tempfile::tempdir().unwrap();
    let keys = tmp.path().join("k");
    std::fs::create_dir_all(&keys).unwrap();
    let o = cmd(&keys).args(["--json", "discover", "--timeout", "0"]).output().unwrap();
    assert!(!o.status.success());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["ok"], false);
    let (_, last) = discover_json(&keys, &["--timeout", "1"]);
    assert_eq!(last["ok"], true);
    assert!(last["data"]["hosts"].is_array());
}
