//! Two real processes: one advertises, this one browses. mDNS needs working multicast on the
//! chosen interface; if the OS or network blocks it these tests SKIP with a message instead of
//! failing (and the skip must be reported, since it means the path was not exercised).
//!
//! The advertiser is this same test binary re-executed with `RGIT_DISCOVERY_ROLE=advertise`.

use std::io::{BufRead, BufReader, Write};
use std::net::IpAddr;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustgit_discovery::{
    browse, project_hint, Addresses, AdvertiseConfig, Advertiser, BrowseOptions, Event,
};

const PROJECT: &str = "11111111-2222-3333-4444-555555555555";

/// Entry point of the child process. Not a real test: it returns at once unless re-executed
/// with the role variable set.
#[test]
fn advertiser_child_process_entry() {
    if std::env::var("RGIT_DISCOVERY_ROLE").as_deref() != Ok("advertise") {
        return;
    }
    let ip: IpAddr = std::env::var("RGIT_DISCOVERY_IP").unwrap().parse().unwrap();
    let port: u16 = std::env::var("RGIT_DISCOVERY_PORT").unwrap().parse().unwrap();
    let adv = Advertiser::start(&AdvertiseConfig {
        project_uuid: PROJECT.into(),
        port,
        protocol_version: 1,
        addresses: Addresses::Only(vec![ip]),
    })
    .unwrap();
    println!("READY {}", adv.instance());
    std::io::stdout().flush().unwrap();
    // run until the parent closes our stdin, then drop the advertiser (mDNS goodbye)
    let mut s = String::new();
    while std::io::stdin().read_line(&mut s).map(|n| n > 0).unwrap_or(false) {
        s.clear();
    }
    drop(adv);
}

fn spawn_advertiser(ip: IpAddr, port: u16) -> (Child, String) {
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["--exact", "advertiser_child_process_entry", "--nocapture", "--test-threads=1"])
        .env("RGIT_DISCOVERY_ROLE", "advertise")
        .env("RGIT_DISCOVERY_IP", ip.to_string())
        .env("RGIT_DISCOVERY_PORT", port.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut r = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    let start = Instant::now();
    loop {
        line.clear();
        if r.read_line(&mut line).unwrap() == 0 {
            let _ = child.wait();
            panic!("advertiser exited early");
        }
        // libtest prints "test <name> ... " before the child's own output on the same line
        if let Some(i) = line.trim().split("READY ").nth(1) {
            let inst = i.trim().to_string();
            // keep draining so the child never blocks on a full pipe
            std::thread::spawn(move || {
                let mut s = String::new();
                while r.read_line(&mut s).map(|n| n > 0).unwrap_or(false) {
                    s.clear();
                }
            });
            return (child, inst);
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("advertiser never became ready");
        }
    }
}

/// A private IPv4 address on a real interface (not a proxy/VPN tunnel such as 198.18.0.0/15).
/// `RGIT_TEST_LAN_IP` overrides the choice.
fn lan_ip() -> Option<IpAddr> {
    if let Ok(v) = std::env::var("RGIT_TEST_LAN_IP") {
        return v.parse().ok();
    }
    let ifs = if_addrs::get_if_addrs().ok()?;
    let private = |ip: &IpAddr| match ip {
        IpAddr::V4(a) => a.is_private(),
        _ => false,
    };
    ifs.iter()
        .filter(|i| !i.is_loopback() && !i.name.starts_with("utun") && !i.name.starts_with("awdl"))
        .map(|i| i.ip())
        .find(private)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn find(ip: IpAddr, include_loopback: bool, instance: &str, within: Duration) -> Option<rustgit_discovery::DiscoveredHost> {
    let opts = BrowseOptions { include_loopback };
    let mut found = None;
    browse(Some(within), &opts, |e| {
        if let Event::Found(h) = e {
            if h.instance == instance && h.addresses.contains(&ip) {
                found = Some(h);
                return false;
            }
        }
        true
    })
    .unwrap();
    found
}

fn run_case(name: &str, ip: IpAddr, include_loopback: bool) -> bool {
    let port = free_port();
    let (mut child, instance) = spawn_advertiser(ip, port);
    let host = find(ip, include_loopback, &instance, Duration::from_secs(12));
    let Some(h) = host else {
        eprintln!("SKIPPED ({name}): the advertiser at {ip} was not seen within 12 s. Multicast DNS seems to be blocked or unavailable on this interface (OS firewall, local-network permission, or the network).");
        drop(child.stdin.take());
        let _ = child.wait();
        return false;
    };
    // what arrived is exactly the minimal metadata
    assert_eq!(h.port, port);
    assert_eq!(h.protocol_version, Some(1));
    assert_eq!(h.project_hint.as_deref(), Some(project_hint(PROJECT).as_str()));
    assert!(h.matches_project(PROJECT));
    assert!(!h.matches_project("some-other-project"));
    assert!(h.instance.starts_with("rgit-"));
    assert!(!h.instance.contains(&PROJECT[..8]), "instance must not contain the project id");
    let hostname = std::env::var("HOSTNAME").unwrap_or_default();
    assert!(hostname.is_empty() || !h.instance.contains(&hostname), "machine name leaked");
    assert_eq!(h.tcp_urls().first().map(String::as_str), Some(format!("tcp://{ip}:{port}").as_str()));

    // A clean exit sends the mDNS goodbye. Keep one browse open: once it has seen the
    // instance, let the advertiser exit and expect the Lost event.
    let opts = BrowseOptions { include_loopback };
    let mut stdin = child.stdin.take();
    let mut lost = false;
    let t0 = Instant::now();
    browse(Some(Duration::from_secs(20)), &opts, |e| {
        match e {
            Event::Found(h) if h.instance == instance => {
                drop(stdin.take()); // ask the advertiser to exit (drops it: goodbye packet)
            }
            Event::Lost { instance: i } if i == instance => {
                lost = true;
                return false;
            }
            _ => {}
        }
        true
    })
    .unwrap();
    drop(stdin);
    let _ = child.wait();
    eprintln!("({name}) goodbye observed as a Lost event after {:?}: {lost}", t0.elapsed());
    assert!(lost, "no goodbye/lost event after the advertiser exited");
    true
}

#[test]
fn loopback_advertise_and_browse() {
    let ran = run_case("loopback", "127.0.0.1".parse().unwrap(), true);
    if !ran {
        eprintln!("loopback case NOT EXERCISED");
    }
}

#[test]
fn lan_interface_advertise_and_browse() {
    let Some(ip) = lan_ip() else {
        eprintln!("SKIPPED (lan): no non-loopback IPv4 route on this machine");
        return;
    };
    let ran = run_case("lan", ip, false);
    if !ran {
        eprintln!("lan case NOT EXERCISED");
    }
}

#[test]
fn loopback_is_hidden_from_browsers_unless_asked_for() {
    let ip: IpAddr = "127.0.0.1".parse().unwrap();
    let port = free_port();
    let (mut child, instance) = spawn_advertiser(ip, port);
    // With include_loopback off the entry is dropped at the browser (another machine's
    // 127.0.0.1 would be this machine). This holds whether or not multicast works.
    let seen = find(ip, false, &instance, Duration::from_secs(4));
    drop(child.stdin.take());
    let _ = child.wait();
    assert!(seen.is_none(), "loopback address leaked to a browser that did not ask for it");
}

/// A server bound to `0.0.0.0` must not publish IPv6 addresses (it would not accept connections
/// there). Records are answered per interface, so a browser on the *same machine* may also see
/// 127.0.0.1 (answered on lo0, never on the wire to other hosts) and browsers drop it by default;
/// the test checks the addresses a browser keeps by default.
#[test]
fn wildcard_v4_publishes_only_ipv4_addresses_and_browsers_drop_loopback() {
    let adv = Advertiser::start(&AdvertiseConfig {
        project_uuid: PROJECT.into(),
        port: 4242,
        protocol_version: 1,
        addresses: Addresses::AllInterfacesV4,
    })
    .unwrap();
    let inst = adv.instance().to_string();
    let mut raw: Vec<IpAddr> = Vec::new();
    browse(Some(Duration::from_secs(10)), &BrowseOptions { include_loopback: true }, |e| {
        if let Event::Found(h) = e {
            if h.instance == inst {
                raw = h.addresses.clone();
                return false;
            }
        }
        true
    })
    .unwrap();
    let mut default_view: Vec<IpAddr> = Vec::new();
    browse(Some(Duration::from_secs(10)), &BrowseOptions::default(), |e| {
        if let Event::Found(h) = e {
            if h.instance == inst {
                default_view = h.addresses.clone();
                return false;
            }
        }
        true
    })
    .unwrap();
    eprintln!("AllInterfacesV4: seen with loopback allowed: {raw:?}; default browser view: {default_view:?}");
    drop(adv);
    if raw.is_empty() {
        eprintln!("SKIPPED: wildcard advertisement not seen (multicast blocked?); NOT exercised");
        return;
    }
    assert!(raw.iter().all(|a| a.is_ipv4()), "IPv6 published for an IPv4 listener: {raw:?}");
    assert!(default_view.iter().all(|a| a.is_ipv4() && !a.is_loopback()), "{default_view:?}");
}
