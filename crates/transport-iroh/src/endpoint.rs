//! Endpoint construction and the sync <-> async bridge.

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::presets;
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, RelayMap, RelayMode, RelayUrl, SecretKey};
use rustls_pki_types::CertificateDer;

use crate::config::{IrohConfig, RelaySetting};

/// The application protocol name negotiated inside QUIC/TLS. Only this ALPN is accepted.
pub const ALPN: &[u8] = b"rustgit/sync/1";

/// The protocol is blocking and threaded, Iroh is async: every transport and listener owns a
/// small runtime and the blocking `Duplex` methods run on it with `block_on` (always called
/// from plain threads, never from inside the runtime).
pub(crate) struct Rt(pub(crate) tokio::runtime::Runtime);

impl Rt {
    pub(crate) fn new() -> io::Result<Arc<Rt>> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rgit-iroh")
            .enable_all()
            .build()?;
        Ok(Arc::new(Rt(rt)))
    }

    pub(crate) fn block_on<F: Future>(&self, f: F) -> F::Output {
        self.0.block_on(f)
    }

    pub(crate) fn spawn<F>(&self, f: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.0.spawn(f);
    }
}

pub(crate) fn io_err(kind: io::ErrorKind, e: impl std::fmt::Display) -> io::Error {
    io::Error::new(kind, e.to_string())
}

/// `Display` of an error including its `source()` chain.
pub(crate) fn chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        let t = c.to_string();
        if !s.contains(&t) {
            s.push_str(": ");
            s.push_str(&t);
        }
        cur = c.source();
    }
    s
}

pub(crate) fn relay_urls(cfg: &IrohConfig) -> io::Result<Vec<RelayUrl>> {
    match &cfg.relay {
        RelaySetting::Custom(v) => v
            .iter()
            .map(|u| {
                u.parse::<RelayUrl>().map_err(|e| {
                    io_err(io::ErrorKind::InvalidInput, format!("relay URL '{u}': {e}"))
                })
            })
            .collect(),
        _ => Ok(Vec::new()),
    }
}

pub(crate) fn bind(rt: &Rt, cfg: &IrohConfig, alpns: Vec<Vec<u8>>) -> io::Result<Endpoint> {
    let mode = match &cfg.relay {
        RelaySetting::None => RelayMode::Disabled,
        RelaySetting::Public => RelayMode::Default,
        RelaySetting::Custom(_) => {
            let urls = relay_urls(cfg)?;
            if urls.is_empty() {
                return Err(io_err(
                    io::ErrorKind::InvalidInput,
                    "no relay URL configured",
                ));
            }
            let mut map = RelayMap::from_iter(urls);
            if let Some(t) = &cfg.relay_token {
                map = map.with_auth_token(t.clone());
            }
            RelayMode::Custom(map)
        }
    };
    // `Minimal` sets only the TLS provider: no n0 DNS/pkarr address lookup, so the endpoint
    // never publishes or queries anything on third-party infrastructure. Peers are found
    // through the relay URL / addresses carried by the `iroh://` URL.
    let mut b = Endpoint::builder(presets::Minimal)
        .alpns(alpns)
        .relay_mode(mode);
    if let Some(k) = cfg.secret_key {
        b = b.secret_key(SecretKey::from_bytes(&k));
    }
    if !cfg.relay_ca.is_empty() {
        let roots: Vec<CertificateDer<'static>> = cfg
            .relay_ca
            .iter()
            .map(|d| CertificateDer::from(d.clone()))
            .collect();
        b = b.ca_tls_config(CaTlsConfig::custom_roots(roots));
    }
    if cfg.direct {
        if let Some(a) = cfg.bind_addr {
            // `bind_addr` alone only replaces the default socket of that address family (an
            // IPv6 `[::]` socket would stay), so clear the defaults to make it exclusive.
            b = b
                .clear_ip_transports()
                .bind_addr(a)
                .map_err(|e| io_err(io::ErrorKind::InvalidInput, format!("bind address: {e}")))?;
        }
    } else {
        if matches!(cfg.relay, RelaySetting::None) {
            return Err(io_err(
                io::ErrorKind::InvalidInput,
                "direct paths are disabled and no relay is configured: nothing could connect",
            ));
        }
        b = b.clear_ip_transports();
    }
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), b.bind())
            .await
            .map_err(|_| {
                io_err(
                    io::ErrorKind::TimedOut,
                    "binding the iroh endpoint timed out",
                )
            })?
            .map_err(|e| {
                io_err(
                    io::ErrorKind::Other,
                    format!("cannot start the iroh endpoint: {}", chain(&e)),
                )
            })
    })
}
