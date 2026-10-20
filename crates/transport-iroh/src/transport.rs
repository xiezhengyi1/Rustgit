//! Client side: dial an endpoint id with hints.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};
use rustgit_protocol::{Duplex, Transport};

use crate::config::IrohConfig;
use crate::duplex::IrohDuplex;
use crate::endpoint::{self, chain, io_err, Rt, ALPN};
use crate::status::{Status, StatusHandle};
use crate::url::IrohUrl;

pub struct IrohTransport {
    rt: Arc<Rt>,
    cfg: IrohConfig,
    url: IrohUrl,
    target: EndpointId,
    endpoint: Mutex<Option<Endpoint>>,
    status: StatusHandle,
}

impl IrohTransport {
    pub fn new(url: IrohUrl, cfg: IrohConfig) -> io::Result<Self> {
        let target: EndpointId = url.id.parse().map_err(|e| {
            io_err(
                io::ErrorKind::InvalidInput,
                format!("'{}' is not a valid iroh endpoint id ({e})", url.id),
            )
        })?;
        // Validate everything that can be validated without touching the network.
        endpoint::relay_urls(&cfg)?;
        for r in &url.relays {
            r.parse::<RelayUrl>().map_err(|e| {
                io_err(io::ErrorKind::InvalidInput, format!("relay URL '{r}': {e}"))
            })?;
        }
        Ok(IrohTransport {
            rt: Rt::new()?,
            status: StatusHandle::new(url.id.clone()),
            cfg,
            url,
            target,
            endpoint: Mutex::new(None),
        })
    }

    /// The live connection state for the UI: `direct`, `relayed` or `unreachable`.
    pub fn status(&self) -> Status {
        self.status.snapshot()
    }

    pub fn status_handle(&self) -> StatusHandle {
        self.status.clone()
    }

    /// Our own endpoint id (the identity the host sees), once the endpoint exists.
    pub fn local_id(&self) -> Option<String> {
        self.endpoint
            .lock()
            .unwrap()
            .as_ref()
            .map(|e| e.id().to_string())
    }

    /// Local UDP sockets of the client endpoint (empty before the first connection, and in
    /// relay-only mode).
    pub fn bound_sockets(&self) -> Vec<std::net::SocketAddr> {
        self.endpoint
            .lock()
            .unwrap()
            .as_ref()
            .map(|e| e.bound_sockets())
            .unwrap_or_default()
    }

    /// Ask Iroh to re-examine the network interfaces now (Wi-Fi switch, VPN up/down, resume
    /// from sleep). If something really changed Iroh rebinds its sockets and re-probes its
    /// paths; if nothing changed this does nothing. Iroh also watches the OS itself; this is
    /// for hosts that learn of a change first (e.g. an app handling an OS notification).
    pub fn network_changed(&self) {
        let ep = self.endpoint.lock().unwrap().clone();
        if let Some(ep) = ep {
            self.rt.block_on(ep.network_change());
        }
    }

    fn endpoint(&self) -> io::Result<Endpoint> {
        let mut g = self.endpoint.lock().unwrap();
        if let Some(ep) = g.as_ref() {
            if !ep.is_closed() {
                return Ok(ep.clone());
            }
        }
        let ep = endpoint::bind(&self.rt, &self.cfg, Vec::new())?;
        self.status.set_local(ep.id().to_string());
        *g = Some(ep.clone());
        Ok(ep)
    }

    fn target_addr(&self) -> io::Result<EndpointAddr> {
        let mut addr = EndpointAddr::new(self.target);
        for r in &self.url.relays {
            addr = addr.with_relay_url(r.parse().map_err(|e| {
                io_err(io::ErrorKind::InvalidInput, format!("relay URL '{r}': {e}"))
            })?);
        }
        // With no relay hint in the URL, assume the host uses the relays we are configured with.
        if self.url.relays.is_empty() {
            for r in endpoint::relay_urls(&self.cfg)? {
                addr = addr.with_relay_url(r);
            }
        }
        for a in &self.url.addrs {
            addr = addr.with_ip_addr(*a);
        }
        if addr.is_empty() {
            return Err(io_err(
                io::ErrorKind::InvalidInput,
                "the iroh URL has no relay or addr hint and no relay is configured, so there is no way to find the host",
            ));
        }
        Ok(addr)
    }

    fn unreachable(&self, why: String) -> String {
        format!(
            "cannot reach iroh host {}: {why}. The host may be offline, its relay may be down or \
             unreachable from here, or both devices' networks may block direct connections and no \
             relay is available",
            &self.url.id[..16]
        )
    }
}

impl Transport for IrohTransport {
    fn connect(&self, timeout: Duration) -> io::Result<Box<dyn Duplex>> {
        let gen = self.status.begin();
        let fail = |kind: io::ErrorKind, why: String| {
            let msg = self.unreachable(why);
            self.status.failed(gen, msg.clone());
            io_err(kind, msg)
        };
        let ep = match self.endpoint() {
            Ok(e) => e,
            Err(e) => {
                self.status.failed(gen, e.to_string());
                return Err(e);
            }
        };
        let addr = match self.target_addr() {
            Ok(a) => a,
            Err(e) => {
                self.status.failed(gen, e.to_string());
                return Err(e);
            }
        };
        let limit = self.cfg.connect_timeout.unwrap_or(timeout);
        let res = self.rt.block_on(async {
            tokio::time::timeout(limit, async {
                let conn = ep.connect(addr, ALPN).await.map_err(|e| chain(&e))?;
                let (send, recv) = conn.open_bi().await.map_err(|e| chain(&e))?;
                Ok::<_, String>((conn, send, recv))
            })
            .await
        });
        match res {
            Err(_) => Err(fail(
                io::ErrorKind::TimedOut,
                format!("no answer within {}s", limit.as_secs().max(1)),
            )),
            Ok(Err(why)) => {
                // "timed out" from QUIC is transient and worth retrying; an explicit refusal
                // (wrong protocol, endpoint closed, no address) is not.
                let l = why.to_lowercase();
                let kind = if l.contains("timed out") || l.contains("timeout") {
                    io::ErrorKind::TimedOut
                } else if l.contains("no addressing") {
                    io::ErrorKind::NotFound
                } else {
                    io::ErrorKind::ConnectionRefused
                };
                Err(fail(kind, why))
            }
            Ok(Ok((conn, send, recv))) => Ok(Box::new(IrohDuplex::new(
                self.rt.clone(),
                conn,
                send,
                recv,
                Some((self.status.clone(), gen)),
            ))),
        }
    }

    fn describe(&self) -> String {
        self.url.to_string()
    }
}

impl Drop for IrohTransport {
    fn drop(&mut self) {
        if let Some(ep) = self.endpoint.lock().unwrap().take() {
            self.rt.block_on(async {
                let _ = tokio::time::timeout(Duration::from_secs(3), ep.close()).await;
            });
        }
    }
}
