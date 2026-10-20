//! Host side: accept Iroh connections and hand them to the host as `Duplex` streams.

use std::collections::HashSet;
use std::io;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, EndpointId};
use rustgit_protocol::{Duplex, Listener};
use tokio::sync::Semaphore;

use crate::config::IrohConfig;
use crate::duplex::IrohDuplex;
use crate::endpoint::{self, io_err, Rt, ALPN};

/// Connections that completed the QUIC handshake but whose protocol stream has not shown up
/// yet. Bounded so a flood of half-open connections cannot exhaust memory.
const MAX_PENDING: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

pub struct IrohListener {
    rt: Arc<Rt>,
    ep: Endpoint,
    rx: Receiver<IrohDuplex>,
}

impl IrohListener {
    /// `allow`: if `Some`, only these endpoint ids (64 hex characters; authenticated by the
    /// TLS handshake) may connect. `None` accepts any endpoint that can reach this host, which
    /// is only appropriate when device authorization happens above the transport.
    pub fn bind(cfg: &IrohConfig, allow: Option<Vec<String>>) -> io::Result<Self> {
        let allow: Option<HashSet<EndpointId>> = match allow {
            None => None,
            Some(v) => Some(
                v.iter()
                    .map(|s| {
                        s.parse::<EndpointId>().map_err(|e| {
                            io_err(
                                io::ErrorKind::InvalidInput,
                                format!("endpoint id '{s}': {e}"),
                            )
                        })
                    })
                    .collect::<io::Result<_>>()?,
            ),
        };
        let rt = Rt::new()?;
        let ep = endpoint::bind(&rt, cfg, vec![ALPN.to_vec()])?;
        let (tx, rx) = sync_channel::<IrohDuplex>(MAX_PENDING);
        let permits = Arc::new(Semaphore::new(MAX_PENDING));
        let accept_ep = ep.clone();
        let rt2 = rt.clone();
        rt.spawn(async move {
            while let Some(incoming) = accept_ep.accept().await {
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    incoming.refuse();
                    continue;
                };
                let tx: SyncSender<IrohDuplex> = tx.clone();
                let allow = allow.clone();
                let rt3 = rt2.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let r = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
                        let conn = incoming.await.ok()?;
                        if let Some(a) = &allow {
                            if !a.contains(&conn.remote_id()) {
                                conn.close(403u32.into(), b"not authorized");
                                return None;
                            }
                        }
                        // The client's first write (Hello) makes its stream visible here.
                        let (send, recv) = conn.accept_bi().await.ok()?;
                        Some(IrohDuplex::new(rt3, conn, send, recv, None))
                    })
                    .await;
                    if let Ok(Some(d)) = r {
                        match tx.try_send(d) {
                            Ok(()) => {}
                            Err(TrySendError::Full(d)) | Err(TrySendError::Disconnected(d)) => {
                                drop(d)
                            }
                        }
                    }
                });
            }
        });
        Ok(IrohListener { rt, ep, rx })
    }

    pub fn endpoint_id(&self) -> String {
        self.ep.id().to_string()
    }

    /// Waits until the endpoint is registered with its home relay (when one is configured).
    /// Returns `false` on timeout, which usually means the relay is unreachable.
    pub fn wait_online(&self, timeout: Duration) -> bool {
        self.rt.block_on(async {
            tokio::time::timeout(timeout, self.ep.online())
                .await
                .is_ok()
        })
    }

    /// The `iroh://` URL clients use to reach this host. It always carries the home relay;
    /// `include_addrs` adds the local interface addresses as direct hints (faster first
    /// connection on a LAN, but it publishes private IP addresses to whoever gets the URL).
    pub fn join_url(&self, include_addrs: bool) -> String {
        let a = self.ep.addr();
        let mut u = crate::url::IrohUrl::new(self.ep.id().to_string());
        u.relays = a.relay_urls().map(|r| r.to_string()).collect();
        if include_addrs {
            u.addrs = a.ip_addrs().copied().collect();
        }
        u.to_string()
    }

    /// Addresses the UDP socket is bound to.
    pub fn bound_sockets(&self) -> Vec<std::net::SocketAddr> {
        self.ep.bound_sockets()
    }

    pub fn network_changed(&self) {
        self.rt.block_on(self.ep.network_change());
    }

    pub fn close(&self) {
        self.rt.block_on(async {
            let _ = tokio::time::timeout(Duration::from_secs(3), self.ep.close()).await;
        });
    }
}

impl Listener for IrohListener {
    fn accept(&self, wait: Duration) -> io::Result<Option<Box<dyn Duplex>>> {
        match self.rx.recv_timeout(wait) {
            Ok(d) => Ok(Some(Box::new(d))),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(io_err(io::ErrorKind::NotConnected, "iroh endpoint closed"))
            }
        }
    }

    fn local_addr(&self) -> String {
        format!("iroh://{}", self.ep.id())
    }
}

impl Drop for IrohListener {
    fn drop(&mut self) {
        self.close();
    }
}
