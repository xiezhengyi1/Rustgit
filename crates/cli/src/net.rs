//! Network-facing commands that sit beside the TCP defaults: `ssh://` remotes,
//! `serve --stdio`, mDNS advertising for `serve --advertise`, and `rgit discover`.
//!
//! This file is the single place where a remote URL scheme is turned into a transport
//! (`transport_for`); other transports register there.

use std::io::{IsTerminal, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustgit_core::{Error, Repo};
use rustgit_discovery::{Addresses, AdvertiseConfig, Advertiser, BrowseOptions, DiscoveredHost, Event};
use rustgit_host::Host;
use rustgit_protocol::{Transport, PROTOCOL_MAX_VERSION};
use rustgit_transport_ssh::{OneShotListener, SshOptions, SshTransport};
use serde_json::{json, Value};

use crate::{out, say, Cli, Cmd};

fn other(e: impl std::fmt::Display) -> Error {
    Error::Other(e.to_string())
}

/// Builds the transport for a remote URL: `ssh://` here, everything else from the protocol
/// crate (`tcp://`).
pub fn transport_for(url: &str) -> Result<Box<dyn Transport>, String> {
    if url.starts_with("iroh://") {
        return crate::iroh::transport(url);
    }
    if url.starts_with("ssh://") {
        let opts = SshOptions::from_env(std::io::stdin().is_terminal()).map_err(|e| e.to_string())?;
        let t = SshTransport::from_url(url, opts).map_err(|e| format!("invalid SSH remote: {e}"))?;
        return Ok(Box::new(t));
    }
    rustgit_protocol::transport::transport_from_url(url)
}

/// `rgit serve --stdio`: exactly one session on stdin/stdout, then exit. stdout carries
/// protocol frames only; everything for humans goes to stderr.
pub fn run_stdio(host: Arc<Host>) -> Result<u8, Error> {
    if out::is_json() {
        return Err(Error::Invalid(
            "--stdio uses stdout for the protocol and cannot be combined with --json".into(),
        ));
    }
    let duplex = rustgit_transport_ssh::session_over(std::io::stdin(), std::io::stdout(), "stdio")
        .map_err(other)?;
    let (listener, finished) = OneShotListener::new(Box::new(duplex), "stdio");
    let running = rustgit_host::spawn(host, Box::new(listener));
    while !finished.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(20));
    }
    running.stop();
    Ok(0)
}

static ADVERT: Mutex<Option<Advertiser>> = Mutex::new(None);

/// Starts mDNS advertising for the hosted project listening on `bound`.
/// Only the opaque project hint, protocol version and port are published (see
/// `rustgit_discovery`).
pub fn advertise(host: &Host, bound: SocketAddr) -> Result<(), Error> {
    let addresses = if bound.ip().is_unspecified() {
        if bound.is_ipv4() {
            Addresses::AllInterfacesV4
        } else {
            Addresses::AllInterfaces
        }
    } else {
        Addresses::Only(vec![bound.ip()])
    };
    let adv = Advertiser::start(&AdvertiseConfig {
        project_uuid: host.uuid().to_string(),
        port: bound.port(),
        protocol_version: u32::from(PROTOCOL_MAX_VERSION),
        addresses,
    })
    .map_err(other)?;
    say!(
        "Advertising on the local network as {} (hint {}); results are hints, not identities",
        adv.instance(),
        adv.project_hint()
    );
    out::event(json!({
        "event": "advertising",
        "instance": adv.instance(),
        "project_hint": adv.project_hint(),
        "port": bound.port(),
    }));
    *ADVERT.lock().unwrap() = Some(adv);
    // `rgit serve` is normally stopped by a signal; say goodbye so neighbours drop the entry
    // at once instead of when the record expires (up to 75 minutes).
    let _ = ctrlc::set_handler(|| {
        drop(ADVERT.lock().unwrap().take());
        std::process::exit(0);
    });
    Ok(())
}

/// Plain TCP connect to see whether anything still listens (stale mDNS records outlive
/// their host). Never sends a byte.
fn probe(addrs: &[SocketAddr]) -> bool {
    addrs
        .iter()
        .any(|a| TcpStream::connect_timeout(a, Duration::from_millis(800)).is_ok())
}

fn host_json(h: &DiscoveredHost, project: Option<&str>, reachable: Option<bool>) -> Value {
    json!({
        "instance": h.instance,
        "addresses": h.addresses.iter().map(IpAddr::to_string).collect::<Vec<_>>(),
        "port": h.port,
        "urls": h.tcp_urls(),
        "project_hint": h.project_hint,
        "protocol_version": h.protocol_version,
        "matches_project": project.map(|p| h.matches_project(p)),
        "reachable": reachable,
        "verified": false,
        "source": "mdns",
    })
}

pub fn discover(cli: Cli) -> Result<u8, Error> {
    let Cmd::Discover {
        timeout,
        watch,
        project,
        remote,
        include_loopback,
        no_probe,
    } = cli.cmd
    else {
        unreachable!()
    };
    let project: Option<String> = match (project, remote) {
        (Some(p), _) => Some(p),
        (None, Some(r)) => {
            let repo = Repo::discover(&cli.dir)?;
            Some(
                repo.get_config(&format!("remote.{r}.project"))?
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| {
                        Error::Other(format!(
                            "remote '{r}' has no pinned project yet (fetch from it once, or pass --project UUID)"
                        ))
                    })?,
            )
        }
        (None, None) => None,
    };
    if !timeout.is_finite() || !(0.1..=600.0).contains(&timeout) {
        return Err(Error::Invalid("--timeout must be between 0.1 and 600 seconds".into()));
    }
    let opts = BrowseOptions { include_loopback };
    let limit = if watch { None } else { Some(Duration::from_secs_f64(timeout)) };
    let mut hosts: Vec<(DiscoveredHost, Option<bool>)> = Vec::new();
    rustgit_discovery::browse(limit, &opts, |ev| {
        match ev {
            Event::Found(h) => {
                let reachable = (!no_probe).then(|| probe(&h.socket_addrs()));
                out::event(json!({"event": "host", "host": host_json(&h, project.as_deref(), reachable)}));
                if !out::is_json() {
                    print_host(&h, project.as_deref(), reachable);
                    let _ = std::io::stdout().flush();
                }
                hosts.retain(|(x, _)| x.instance != h.instance);
                hosts.push((h, reachable));
            }
            Event::Lost { instance } => {
                out::event(json!({"event": "lost", "instance": instance}));
                if !out::is_json() {
                    println!("gone: {instance}");
                }
                hosts.retain(|(x, _)| x.instance != instance);
            }
        }
        true
    })
    .map_err(other)?;
    let list: Vec<Value> = hosts
        .iter()
        .map(|(h, r)| host_json(h, project.as_deref(), *r))
        .collect();
    crate::result!(json!({"hosts": list, "timeout_ms": (timeout * 1000.0) as u64}));
    if hosts.is_empty() {
        say!("No rgit hosts found on the local network (multicast may be blocked by this network or firewall).");
    } else {
        say!("These are hints from the local network, not verified identities.");
    }
    Ok(0)
}

fn print_host(h: &DiscoveredHost, project: Option<&str>, reachable: Option<bool>) {
    let m = match project.map(|p| h.matches_project(p)) {
        Some(true) => "  [matches this project's hint]",
        _ => "",
    };
    let r = match reachable {
        Some(false) => "  [not reachable]",
        _ => "",
    };
    println!(
        "{}  {}  hint {}{}{}",
        h.instance,
        h.tcp_urls().join(" "),
        h.project_hint.as_deref().unwrap_or("-"),
        m,
        r
    );
}
