//! Byte-stream transports. The protocol only needs an ordered, reliable duplex stream with
//! read/write timeouts, so TCP, QUIC streams and an SSH channel can all sit behind
//! [`Duplex`]. Only plain TCP exists today. A transport does not authenticate or encrypt
//! anything itself: device authentication and encryption are applied on top of any `Duplex`
//! by [`crate::secure`], so adapters (SSH, Iroh) only have to move bytes.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

pub trait Duplex: Read + Write + Send {
    /// `None` leaves that direction's timeout unchanged.
    fn set_timeouts(&mut self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()>;
    fn shutdown(&mut self) -> io::Result<()>;
    fn peer(&self) -> String;
    /// Bytes `(sent, received)` on the underlying transport so far, when this layer can tell
    /// (the secure channel reports ciphertext bytes including its handshake).
    fn wire_counts(&self) -> Option<(u64, u64)> {
        None
    }
}

pub trait Transport: Send + Sync {
    fn connect(&self, timeout: Duration) -> io::Result<Box<dyn Duplex>>;
    fn describe(&self) -> String;
}

pub trait Listener: Send {
    /// Waits up to `wait` for a connection.
    fn accept(&self, wait: Duration) -> io::Result<Option<Box<dyn Duplex>>>;
    fn local_addr(&self) -> String;
}

pub struct TcpDuplex(TcpStream);

impl TcpDuplex {
    pub fn new(s: TcpStream) -> io::Result<Self> {
        s.set_nodelay(true)?;
        s.set_nonblocking(false)?;
        Ok(TcpDuplex(s))
    }
}

impl Read for TcpDuplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for TcpDuplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Duplex for TcpDuplex {
    fn set_timeouts(&mut self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()> {
        if read.is_some() {
            self.0.set_read_timeout(read)?;
        }
        if write.is_some() {
            self.0.set_write_timeout(write)?;
        }
        Ok(())
    }
    fn shutdown(&mut self) -> io::Result<()> {
        self.0.shutdown(std::net::Shutdown::Both)
    }
    fn peer(&self) -> String {
        self.0
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "unknown".into())
    }
}

pub struct TcpTransport {
    pub addr: String,
}

impl TcpTransport {
    pub fn new(addr: impl Into<String>) -> Self {
        TcpTransport { addr: addr.into() }
    }
}

impl Transport for TcpTransport {
    fn connect(&self, timeout: Duration) -> io::Result<Box<dyn Duplex>> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "address did not resolve");
        for a in self.addr.to_socket_addrs()? {
            match TcpStream::connect_timeout(&a, timeout) {
                Ok(s) => return Ok(Box::new(TcpDuplex::new(s)?)),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
    fn describe(&self) -> String {
        format!("tcp://{}", self.addr)
    }
}

pub struct TcpListenerAdapter {
    inner: TcpListener,
}

impl TcpListenerAdapter {
    pub fn bind(addr: &str) -> io::Result<Self> {
        let inner = TcpListener::bind(addr)?;
        inner.set_nonblocking(true)?;
        Ok(TcpListenerAdapter { inner })
    }
    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

impl Listener for TcpListenerAdapter {
    fn accept(&self, wait: Duration) -> io::Result<Option<Box<dyn Duplex>>> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            match self.inner.accept() {
                Ok((s, _)) => return Ok(Some(Box::new(TcpDuplex::new(s)?))),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
    fn local_addr(&self) -> String {
        self.inner
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default()
    }
}

/// Builds a transport from a remote URL. Only `tcp://host:port` exists today.
pub fn transport_from_url(url: &str) -> Result<Box<dyn Transport>, String> {
    match url.strip_prefix("tcp://") {
        Some(rest) if !rest.is_empty() && rest.contains(':') => {
            Ok(Box::new(TcpTransport::new(rest.trim_end_matches('/'))))
        }
        _ => Err(format!(
            "unsupported remote URL '{url}' (supported: tcp://host:port)"
        )),
    }
}
