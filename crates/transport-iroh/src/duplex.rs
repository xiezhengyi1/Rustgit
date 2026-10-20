//! One protocol connection = one QUIC connection with one bidirectional stream.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use iroh::endpoint::{Connection, RecvStream, SendStream};

use crate::endpoint::{chain, io_err, Rt};
use crate::status::StatusHandle;

pub struct IrohDuplex {
    rt: Arc<Rt>,
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    peer: String,
    status: Option<(StatusHandle, u64)>,
    closed: bool,
}

impl IrohDuplex {
    pub(crate) fn new(
        rt: Arc<Rt>,
        conn: Connection,
        send: SendStream,
        recv: RecvStream,
        status: Option<(StatusHandle, u64)>,
    ) -> Self {
        let peer = format!("iroh:{}", conn.remote_id());
        if let Some((s, gen)) = &status {
            let (s, gen, c) = (s.clone(), *gen, conn.clone());
            rt.spawn(async move { watch_paths(c, s, gen).await });
        }
        IrohDuplex {
            rt,
            conn,
            send,
            recv,
            read_timeout: None,
            write_timeout: None,
            peer,
            status,
            closed: false,
        }
    }

    /// The path QUIC is using right now: `Some(true)` relay, `Some(false)` direct.
    pub fn selected_path_is_relay(&self) -> Option<bool> {
        self.conn
            .paths()
            .iter()
            .find(|p| p.is_selected())
            .map(|p| p.is_relay())
    }
}

/// Follows the connection's path changes and mirrors them into the status handle.
async fn watch_paths(conn: Connection, status: StatusHandle, gen: u64) {
    {
        let mut stream = conn.paths_stream();
        while let Some(list) = stream.next().await {
            let sel = list.iter().find(|p| p.is_selected()).map(|p| {
                let desc = match p.remote_addr() {
                    iroh::TransportAddr::Relay(u) => format!("relay {u}"),
                    iroh::TransportAddr::Ip(a) => a.to_string(),
                    other => format!("{other:?}"),
                };
                (p.is_relay(), desc)
            });
            status.path(gen, sel);
        }
    }
    // A close we or the peer asked for (code 0, sent by `shutdown`) is the normal end of a
    // session, not a loss of the host.
    match conn.closed().await {
        iroh::endpoint::ConnectionError::LocallyClosed => {}
        iroh::endpoint::ConnectionError::ApplicationClosed(c) if c.error_code.into_inner() == 0 => {
        }
        e => status.ended(gen, chain(&e)),
    }
}

fn read_err(e: iroh::endpoint::ReadError) -> io::Error {
    io_err(
        io::ErrorKind::ConnectionReset,
        format!("iroh connection lost: {}", chain(&e)),
    )
}

fn write_err(e: iroh::endpoint::WriteError) -> io::Error {
    io_err(
        io::ErrorKind::BrokenPipe,
        format!("iroh connection lost: {}", chain(&e)),
    )
}

impl Read for IrohDuplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let to = self.read_timeout;
        let recv = &mut self.recv;
        self.rt.block_on(async {
            let fut = recv.read(buf);
            let r = match to {
                // `RecvStream::read` is cancel-safe, so a timeout loses no data.
                Some(d) => tokio::time::timeout(d, fut)
                    .await
                    .map_err(|_| io_err(io::ErrorKind::TimedOut, "iroh read timed out"))?,
                None => fut.await,
            };
            match r {
                Ok(Some(n)) => Ok(n),
                Ok(None) => Ok(0),
                Err(e) => Err(read_err(e)),
            }
        })
    }
}

impl Write for IrohDuplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let to = self.write_timeout;
        let send = &mut self.send;
        self.rt.block_on(async {
            let fut = send.write(buf);
            let r = match to {
                Some(d) => tokio::time::timeout(d, fut)
                    .await
                    .map_err(|_| io_err(io::ErrorKind::TimedOut, "iroh write timed out"))?,
                None => fut.await,
            };
            r.map_err(write_err)
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        // QUIC sends written data on its own schedule; there is nothing to flush.
        Ok(())
    }
}

impl rustgit_protocol::Duplex for IrohDuplex {
    fn set_timeouts(&mut self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()> {
        if read.is_some() {
            self.read_timeout = read;
        }
        if write.is_some() {
            self.write_timeout = write;
        }
        Ok(())
    }

    /// Finishes our side, waits briefly until the peer has acknowledged everything we sent
    /// (closing a QUIC connection immediately would discard unsent data, e.g. the final `Bye`),
    /// then closes the connection.
    fn shutdown(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if let Some((s, gen)) = &self.status {
            s.closing(*gen);
        }
        let send = &mut self.send;
        let conn = &self.conn;
        self.rt.block_on(async {
            let _ = send.finish();
            let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
            conn.close(0u32.into(), b"done");
        });
        Ok(())
    }

    fn peer(&self) -> String {
        self.peer.clone()
    }
}

impl Drop for IrohDuplex {
    fn drop(&mut self) {
        let _ = rustgit_protocol::Duplex::shutdown(self);
    }
}
