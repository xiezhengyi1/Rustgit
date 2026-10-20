//! `iroh://<endpoint-id>[?relay=<url>][&relay=<url>][&addr=<ip:port>]`
//!
//! The endpoint id is the host's public key (64 hex characters). It is what authenticates the
//! host; `relay` and `addr` are only *hints* about where to look for it and may be stale.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

pub const SCHEME: &str = "iroh://";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrohUrl {
    /// 64 lowercase hex characters.
    pub id: String,
    /// Home relay URLs of the host (hints).
    pub relays: Vec<String>,
    /// Direct socket addresses of the host (hints).
    pub addrs: Vec<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlError(pub String);

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UrlError {}

fn err<T>(m: impl Into<String>) -> Result<T, UrlError> {
    Err(UrlError(m.into()))
}

fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn pct_decode(s: &str) -> Result<String, UrlError> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s
                .get(i + 1..i + 3)
                .ok_or_else(|| UrlError("truncated %-escape".into()))?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| UrlError("bad %-escape".into()))?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| UrlError("query value is not UTF-8".into()))
}

/// Encodes only what would break the query syntax; relay URLs stay readable.
fn pct_encode(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '%' | '&' | '#' | '+' | ' ' | '=' => o.push_str(&format!("%{:02X}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

impl IrohUrl {
    pub fn new(id: impl Into<String>) -> Self {
        IrohUrl {
            id: id.into(),
            relays: Vec::new(),
            addrs: Vec::new(),
        }
    }
}

impl FromStr for IrohUrl {
    type Err = UrlError;
    fn from_str(url: &str) -> Result<Self, UrlError> {
        let Some(rest) = url.strip_prefix(SCHEME) else {
            return err(format!(
                "not an iroh URL (expected {SCHEME}<endpoint-id>): {url}"
            ));
        };
        let (id, query) = match rest.split_once('?') {
            Some((i, q)) => (i, q),
            None => (rest, ""),
        };
        let id = id.trim_end_matches('/').to_ascii_lowercase();
        if !valid_id(&id) {
            return err("the iroh endpoint id must be 64 hex characters");
        }
        let mut u = IrohUrl::new(id);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = pct_decode(v)?;
            match k {
                "relay" => {
                    if !(v.starts_with("https://") || v.starts_with("http://")) {
                        return err(format!("relay must be an http(s) URL, got '{v}'"));
                    }
                    u.relays.push(v);
                }
                "addr" => u.addrs.push(
                    v.parse()
                        .map_err(|_| UrlError(format!("addr must be ip:port, got '{v}'")))?,
                ),
                other => return err(format!("unknown iroh URL parameter '{other}'")),
            }
        }
        Ok(u)
    }
}

impl fmt::Display for IrohUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEME}{}", self.id)?;
        let mut sep = '?';
        for r in &self.relays {
            write!(f, "{sep}relay={}", pct_encode(r))?;
            sep = '&';
        }
        for a in &self.addrs {
            write!(f, "{sep}addr={a}")?;
            sep = '&';
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "3745592aa0331656b6be955f0fb9ea2e604c08ad065ec5ba610ea6a8c82f188f";

    #[test]
    fn round_trip() {
        let u: IrohUrl = format!("iroh://{ID}?relay=https://relay.example.org:8443/&addr=192.0.2.1:7000&addr=[2001:db8::1]:7000")
            .parse()
            .unwrap();
        assert_eq!(u.relays, ["https://relay.example.org:8443/"]);
        assert_eq!(u.addrs.len(), 2);
        assert_eq!(u.to_string().parse::<IrohUrl>().unwrap(), u);
    }

    #[test]
    fn special_characters_survive() {
        let mut u = IrohUrl::new(ID);
        u.relays.push("https://r.example/?token=a&b=c d".into());
        assert_eq!(u.to_string().parse::<IrohUrl>().unwrap(), u);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "tcp://x",
            "iroh://",
            "iroh://abc",
            &format!("iroh://{}", "g".repeat(64)),
            &format!("iroh://{ID}?relay=ftp://x"),
            &format!("iroh://{ID}?addr=nonsense"),
            &format!("iroh://{ID}?surprise=1"),
            &format!("iroh://{ID}?relay=https://x%zz"),
        ] {
            assert!(bad.parse::<IrohUrl>().is_err(), "{bad}");
        }
    }

    #[test]
    fn id_is_normalised_and_trailing_slash_ignored() {
        let u: IrohUrl = format!("iroh://{}/", ID.to_uppercase()).parse().unwrap();
        assert_eq!(u.id, ID);
    }
}
