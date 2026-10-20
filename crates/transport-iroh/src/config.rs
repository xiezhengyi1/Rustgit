//! Endpoint configuration: which relays to use, which certificates to trust for a private
//! relay, whether direct paths are allowed, and where the device's endpoint key lives.

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

/// Which relay servers an endpoint may use. There is deliberately no implicit default to
/// someone else's infrastructure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RelaySetting {
    /// No relay: only direct addresses work (LAN, port-forwarded hosts). The default, so that
    /// nothing is ever sent to infrastructure the user did not choose.
    #[default]
    None,
    /// n0's public relays (Iroh's built-in list). Rate limited and without any guarantee: for
    /// development and trials, opt-in only.
    Public,
    /// The operator's own relays (see docs/relay.md).
    Custom(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct IrohConfig {
    pub relay: RelaySetting,
    /// DER certificates to trust for relay TLS, in addition to nothing else: when non-empty
    /// they *replace* the built-in web roots (a private CA for a private relay).
    pub relay_ca: Vec<Vec<u8>>,
    /// Bearer token presented to the relay (`access.shared_token` on the relay side). A secret:
    /// never logged or printed by this crate.
    pub relay_token: Option<String>,
    /// 32-byte endpoint secret key. `None` generates a throw-away key (a new identity per run).
    pub secret_key: Option<[u8; 32]>,
    /// Allow direct UDP paths (hole punching, LAN). `false` binds no IP socket at all, so every
    /// byte goes through the relay: the forced-relay mode used for testing and for networks
    /// that must not make direct UDP connections.
    pub direct: bool,
    /// Bind the only UDP socket to this address instead of all interfaces (exclusive: no
    /// other address family is bound).
    pub bind_addr: Option<SocketAddr>,
    /// Overrides the connect timeout the caller passes to `connect`.
    pub connect_timeout: Option<Duration>,
}

impl Default for IrohConfig {
    fn default() -> Self {
        IrohConfig {
            relay: RelaySetting::None,
            relay_ca: Vec::new(),
            relay_token: None,
            secret_key: None,
            direct: true,
            bind_addr: None,
            connect_timeout: None,
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Reads the endpoint secret key from `path` (64 hex characters) or creates it (mode 0600 on
/// Unix). The public half is the endpoint id other devices dial; it stays the same across
/// restarts and IP address changes as long as this file is kept.
pub fn load_or_create_secret_key(path: &Path) -> io::Result<[u8; 32]> {
    match std::fs::read_to_string(path) {
        Ok(t) => {
            return unhex(&t).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a 64-hex-character endpoint key", path.display()),
                )
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let key = iroh::SecretKey::generate().to_bytes();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => {
            writeln!(f, "{}", hex(&key))?;
            f.sync_all()?;
            Ok(key)
        }
        // Lost a race with another process creating the same file: use theirs.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => load_or_create_secret_key(path),
        Err(e) => Err(e),
    }
}

/// The endpoint id (public key, 64 hex characters) belonging to a secret key.
pub fn endpoint_id_of(key: &[u8; 32]) -> String {
    iroh::SecretKey::from_bytes(key).public().to_string()
}

/// Reads PEM certificates (one or more) as DER blobs, for [`IrohConfig::relay_ca`].
pub fn load_ca_pem(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    use rustls_pki_types::{pem::PemObject, CertificateDer};
    let certs: Vec<Vec<u8>> = CertificateDer::pem_file_iter(path)
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?
        .map(|c| c.map(|c| c.to_vec()))
        .collect::<Result<_, _>>()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?;
    if certs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} contains no certificate", path.display()),
        ));
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_is_created_once_and_reused() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/k");
        let a = load_or_create_secret_key(&p).unwrap();
        let b = load_or_create_secret_key(&p).unwrap();
        assert_eq!(a, b);
        assert_eq!(endpoint_id_of(&a).len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&p, "nonsense").unwrap();
        assert!(load_or_create_secret_key(&p).is_err());
    }
}
