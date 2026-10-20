//! Local-network discovery with mDNS / DNS-SD (`_rustgit._tcp`).
//!
//! **Results are hints, never identities.** Anyone on the network can publish a service of this
//! type with any address and any TXT data. A discovered host tells a client "something that
//! claims to be hosting a project with this hint is at this address"; the client still has to
//! connect and complete the protocol handshake (project UUID check now; device identity and
//! encryption once plan step 9 lands) before trusting anything.
//!
//! What a host publishes (and nothing else):
//! * service type `_rustgit._tcp`, SRV port, the addresses it listens on;
//! * instance name `rgit-<8 hex of the hint>-<4 random hex>`; host name `rgit-<same>.local.`
//!   (so neither the machine's name nor the user's name is broadcast);
//! * TXT `v=<highest protocol version>` and `p=<16 hex>`, a short opaque hash of the project
//!   UUID (see [`project_hint`]). No project name, branch names, user names or device names.
//!
//! The hint is stable, so a passive observer on the LAN can tell that the same project is being
//! hosted again, but cannot derive the UUID from it. Only someone who already knows the project
//! UUID can recognise "their" project.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};

pub const SERVICE_TYPE: &str = "_rustgit._tcp.local.";
const TXT_VERSION: &str = "v";
const TXT_PROJECT: &str = "p";
const MAX_HOSTS: usize = 256;
const MAX_ADDRS_PER_HOST: usize = 16;

#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

fn err(e: impl fmt::Display) -> Error {
    Error(e.to_string())
}

/// 16 lowercase hex characters derived from the project UUID with BLAKE3 (domain-separated).
pub fn project_hint(project_uuid: &str) -> String {
    let h = blake3::derive_key("rustgit discovery v1 project hint", project_uuid.as_bytes());
    h[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn random_hex4() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = blake3::Hasher::new();
    h.update(&now.to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    let addr = &h as *const _ as usize;
    h.update(&addr.to_le_bytes());
    let out = h.finalize();
    out.as_bytes()[..2].iter().map(|b| format!("{b:02x}")).collect()
}

/// Which addresses to publish.
#[derive(Debug, Clone)]
pub enum Addresses {
    /// Every address of this machine's network interfaces (for a server listening on `[::]`).
    /// Answers are given per interface, so the loopback address is only ever sent on the
    /// loopback interface (visible to browsers on this machine, which drop it by default).
    AllInterfaces,
    /// Like [`Addresses::AllInterfaces`], IPv4 only (for a server listening on `0.0.0.0`: it
    /// would not accept connections on the machine's IPv6 addresses).
    AllInterfacesV4,
    /// Exactly these (for a server listening on one address). Loopback is only published if it
    /// is listed explicitly, which is useful for tests and single-machine setups.
    Only(Vec<IpAddr>),
}

#[derive(Debug, Clone)]
pub struct AdvertiseConfig {
    pub project_uuid: String,
    pub port: u16,
    /// Highest protocol version this host speaks (TXT `v`).
    pub protocol_version: u32,
    pub addresses: Addresses,
}

/// A running advertisement. Dropping it sends the mDNS goodbye and stops the daemon.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
    instance: String,
    hint: String,
}

impl Advertiser {
    pub fn start(cfg: &AdvertiseConfig) -> Result<Advertiser, Error> {
        if cfg.port == 0 {
            return Err(Error("cannot advertise port 0".into()));
        }
        let hint = project_hint(&cfg.project_uuid);
        let instance = format!("rgit-{}-{}", &hint[..8], random_hex4());
        let host = format!("{instance}.local.");
        let props = [
            (TXT_VERSION, cfg.protocol_version.to_string()),
            (TXT_PROJECT, hint.clone()),
        ];
        let daemon = ServiceDaemon::new().map_err(err)?;
        let mut info = match &cfg.addresses {
            Addresses::AllInterfaces | Addresses::AllInterfacesV4 => {
                ServiceInfo::new(SERVICE_TYPE, &instance, &host, "", cfg.port, &props[..])
                    .map_err(err)?
                    .enable_addr_auto()
            }
            Addresses::Only(ips) => {
                if ips.is_empty() {
                    return Err(Error("no address to advertise".into()));
                }
                ServiceInfo::new(SERVICE_TYPE, &instance, &host, &ips[..], cfg.port, &props[..])
                    .map_err(err)?
            }
        };
        match &cfg.addresses {
            Addresses::Only(ips) => {
                info.set_interfaces(ips.iter().map(|ip| IfKind::Addr(*ip)).collect())
            }
            Addresses::AllInterfacesV4 => info.set_interfaces(vec![IfKind::IPv4]),
            Addresses::AllInterfaces => {}
        }
        let fullname = info.get_fullname().to_string();
        daemon.register(info).map_err(err)?;
        Ok(Advertiser {
            daemon,
            fullname,
            instance,
            hint,
        })
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub fn project_hint(&self) -> &str {
        &self.hint
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        if let Ok(rx) = self.daemon.unregister(&self.fullname) {
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }
        if let Ok(rx) = self.daemon.shutdown() {
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }
    }
}

/// A host seen on the network. Everything in it is attacker-controlled data (see module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHost {
    /// mDNS instance name, sanitized for display. Not a name to trust.
    pub instance: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    /// TXT `p`, if present and well-formed (16 lowercase hex characters).
    pub project_hint: Option<String>,
    /// TXT `v`, if present and numeric.
    pub protocol_version: Option<u32>,
}

impl DiscoveredHost {
    pub fn socket_addrs(&self) -> Vec<SocketAddr> {
        self.addresses.iter().map(|ip| SocketAddr::new(*ip, self.port)).collect()
    }

    /// `tcp://ip:port` URLs, IPv4 first.
    pub fn tcp_urls(&self) -> Vec<String> {
        self.socket_addrs().iter().map(|a| format!("tcp://{a}")).collect()
    }

    /// True if the hint equals the hint of `project_uuid` (a hint match, not a verification).
    pub fn matches_project(&self, project_uuid: &str) -> bool {
        self.project_hint.as_deref() == Some(project_hint(project_uuid).as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Found(DiscoveredHost),
    /// The instance said goodbye or expired.
    Lost { instance: String },
}

#[derive(Debug, Clone, Default)]
pub struct BrowseOptions {
    /// Keep loopback addresses (127.0.0.0/8, ::1). Off by default: another machine's
    /// "127.0.0.1" is this machine.
    pub include_loopback: bool,
}

fn sanitize_instance(fullname: &str) -> String {
    let inst = fullname.strip_suffix(SERVICE_TYPE).unwrap_or(fullname).trim_end_matches('.');
    inst.chars()
        .filter(|c| !c.is_control())
        .take(63)
        .collect::<String>()
}

fn is_link_local_v6(a: &Ipv6Addr) -> bool {
    (a.segments()[0] & 0xffc0) == 0xfe80
}

fn usable(ip: &IpAddr, include_loopback: bool) -> bool {
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    if ip.is_loopback() && !include_loopback {
        return false;
    }
    match ip {
        // cannot be dialled without an interface scope, which a hint does not carry
        IpAddr::V6(a) if is_link_local_v6(a) => false,
        IpAddr::V4(a) if a.is_link_local() => true,
        _ => true,
    }
}

fn host_from(r: &mdns_sd::ResolvedService, opts: &BrowseOptions) -> Option<DiscoveredHost> {
    let port = r.get_port();
    if port == 0 {
        return None;
    }
    let mut addrs: Vec<IpAddr> = r
        .get_addresses()
        .iter()
        .map(|s| s.to_ip_addr())
        .filter(|ip| usable(ip, opts.include_loopback))
        .collect();
    addrs.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    addrs.dedup();
    addrs.truncate(MAX_ADDRS_PER_HOST);
    if addrs.is_empty() {
        return None;
    }
    let project_hint = r
        .get_property_val_str(TXT_PROJECT)
        .filter(|p| p.len() == 16 && p.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .map(str::to_string);
    let protocol_version = r
        .get_property_val_str(TXT_VERSION)
        .and_then(|v| if v.len() <= 5 { v.parse::<u32>().ok() } else { None });
    Some(DiscoveredHost {
        instance: sanitize_instance(r.get_fullname()),
        addresses: addrs,
        port,
        project_hint,
        protocol_version,
    })
}

/// Browses until `timeout` elapses (or forever when `None`), calling `on_event` for every host
/// found or lost. `on_event` returns `false` to stop early. A host whose data changes is
/// reported as `Found` again. At most 256 distinct instances are tracked.
pub fn browse(
    timeout: Option<Duration>,
    opts: &BrowseOptions,
    mut on_event: impl FnMut(Event) -> bool,
) -> Result<(), Error> {
    let daemon = ServiceDaemon::new().map_err(err)?;
    let rx = daemon.browse(SERVICE_TYPE).map_err(err)?;
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut known: HashMap<String, DiscoveredHost> = HashMap::new();
    let mut lost_names: HashSet<String> = HashSet::new();
    let result = loop {
        let wait = match deadline {
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break Ok(());
                }
                left.min(Duration::from_millis(250))
            }
            None => Duration::from_millis(250),
        };
        match rx.recv_timeout(wait) {
            Ok(ServiceEvent::ServiceResolved(r)) => {
                if !r.is_valid() || !r.ty_domain.eq_ignore_ascii_case(SERVICE_TYPE) {
                    continue;
                }
                let Some(h) = host_from(&r, opts) else { continue };
                if known.get(&r.fullname) == Some(&h) {
                    continue;
                }
                if known.len() >= MAX_HOSTS && !known.contains_key(&r.fullname) {
                    continue;
                }
                known.insert(r.fullname.clone(), h.clone());
                lost_names.remove(&r.fullname);
                if !on_event(Event::Found(h)) {
                    break Ok(());
                }
            }
            Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                if known.remove(&fullname).is_some()
                    && lost_names.insert(fullname.clone())
                    && !on_event(Event::Lost {
                        instance: sanitize_instance(&fullname),
                    })
                {
                    break Ok(());
                }
            }
            Ok(_) => {}
            Err(mdns_sd::RecvTimeoutError::Timeout) => {}
            Err(_) => break Err(Error("the mDNS daemon stopped unexpectedly".into())),
        }
    };
    let _ = daemon.stop_browse(SERVICE_TYPE);
    if let Ok(r) = daemon.shutdown() {
        let _ = r.recv_timeout(Duration::from_millis(500));
    }
    result
}

/// Collects the hosts seen within `timeout`.
pub fn discover(timeout: Duration, opts: &BrowseOptions) -> Result<Vec<DiscoveredHost>, Error> {
    let mut hosts: Vec<DiscoveredHost> = Vec::new();
    browse(Some(timeout), opts, |e| {
        match e {
            Event::Found(h) => {
                hosts.retain(|x| x.instance != h.instance);
                hosts.push(h);
            }
            Event::Lost { instance } => hosts.retain(|x| x.instance != instance),
        }
        true
    })?;
    Ok(hosts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_is_short_stable_and_opaque() {
        let a = project_hint("6f9619ff-8b86-d011-b42d-00c04fc964ff");
        assert_eq!(a.len(), 16);
        assert_eq!(a, project_hint("6f9619ff-8b86-d011-b42d-00c04fc964ff"));
        assert_ne!(a, project_hint("6f9619ff-8b86-d011-b42d-00c04fc964fe"));
        assert!(!a.contains("6f96"));
    }

    #[test]
    fn instance_names_are_sanitized() {
        assert_eq!(
            sanitize_instance("rgit-ab12cd34-ef56._rustgit._tcp.local."),
            "rgit-ab12cd34-ef56"
        );
        assert_eq!(sanitize_instance("a\u{0}b\nc._rustgit._tcp.local."), "abc");
        assert_eq!(sanitize_instance(&"x".repeat(500)).len(), 63);
    }

    #[test]
    fn unusable_addresses_are_dropped() {
        for ip in ["0.0.0.0", "224.0.0.1", "127.0.0.1", "::1", "fe80::1", "ff02::fb"] {
            assert!(!usable(&ip.parse().unwrap(), false), "{ip}");
        }
        assert!(usable(&"127.0.0.1".parse().unwrap(), true));
        assert!(usable(&"192.168.1.5".parse().unwrap(), false));
        assert!(usable(&"fd00::5".parse().unwrap(), false));
    }
}
