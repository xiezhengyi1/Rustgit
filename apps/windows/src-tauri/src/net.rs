//! This computer's IPv4 addresses, so the host can show an address other devices can type.
//! Uses the `if-addrs` crate (GetAdaptersAddresses on Windows) rather than parsing `ipconfig`
//! output, which is localized and changes between Windows versions.

use std::net::{IpAddr, Ipv4Addr};

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct LanAddress {
    pub ip: String,
    /// adapter name as the OS reports it (e.g. "Wi-Fi", "vEthernet (WSL)")
    pub name: String,
    /// RFC 1918 private range (192.168/16, 10/8, 172.16/12): the usual home or office LAN
    pub private: bool,
}

fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

/// Keep addresses a peer could plausibly use: IPv4, not loopback, unspecified, link-local
/// (169.254/16, "no DHCP") or multicast. Private LAN addresses are listed first.
pub fn filter(list: impl IntoIterator<Item = (String, IpAddr)>) -> Vec<LanAddress> {
    let mut out: Vec<LanAddress> = Vec::new();
    for (name, ip) in list {
        let IpAddr::V4(v4) = ip else { continue };
        if v4.is_loopback() || v4.is_unspecified() || v4.is_link_local() || v4.is_multicast() || v4.is_broadcast() {
            continue;
        }
        let ip = v4.to_string();
        if out.iter().any(|a| a.ip == ip) {
            continue;
        }
        out.push(LanAddress { ip, name, private: is_private(v4) });
    }
    out.sort_by(|a, b| b.private.cmp(&a.private).then_with(|| a.name.cmp(&b.name)));
    out
}

pub fn lan_addresses() -> Vec<LanAddress> {
    match if_addrs::get_if_addrs() {
        Ok(list) => filter(list.into_iter().map(|i| (i.name.clone(), i.ip()))),
        Err(_) => Vec::new(),
    }
}

#[tauri::command]
pub async fn lan_addresses_cmd() -> Vec<LanAddress> {
    lan_addresses()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn keeps_usable_ipv4_and_sorts_private_lan_first() {
        let got = filter([
            ("Loopback".to_string(), ip("127.0.0.1")),
            ("Ethernet".to_string(), ip("169.254.10.2")), // no DHCP: unusable
            ("vEthernet (WSL)".to_string(), ip("172.28.80.1")),
            ("Tailscale".to_string(), ip("100.101.102.103")),
            ("Wi-Fi".to_string(), ip("192.168.1.20")),
            ("Wi-Fi".to_string(), ip("fe80::1")), // IPv6 ignored (rgit listens on IPv4 here)
            ("Wi-Fi again".to_string(), ip("192.168.1.20")), // duplicate address
            ("Any".to_string(), ip("0.0.0.0")),
            ("VPN".to_string(), ip("10.8.0.6")),
        ]);
        let ips: Vec<_> = got.iter().map(|a| a.ip.as_str()).collect();
        assert_eq!(ips, ["10.8.0.6", "192.168.1.20", "172.28.80.1", "100.101.102.103"]); // private first, then by adapter name
        assert!(got[..3].iter().all(|a| a.private));
        assert!(!got[3].private);
    }

    #[test]
    fn real_enumeration_does_not_fail_or_return_loopback() {
        for a in lan_addresses() {
            assert!(!a.ip.starts_with("127."), "{a:?}");
        }
    }
}
