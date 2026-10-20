//! Framing: `u32 BE length | u32 BE crc32(payload) | payload`.
//!
//! The length is checked against the negotiated maximum *before* any allocation, and the
//! CRC catches corruption that a lossy or buggy transport adapter could introduce. (TCP
//! and QUIC have their own checks; an SSH byte stream adapter may not be as careful.) The
//! CRC is not authentication: object hashes are what make content trustworthy.

use std::io::{self, Read, Write};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::transport::Duplex;

pub const HEADER_LEN: usize = 8;
/// Frames larger than this are refused even before limits are negotiated.
pub const HARD_MAX_FRAME: u32 = 64 * 1024 * 1024;
/// Limit used for the Hello exchange, before the server announces its own.
pub const HANDSHAKE_MAX_FRAME: u32 = 64 * 1024;

pub struct Channel {
    io: Box<dyn Duplex>,
    max_frame: u32,
    io_timeout: Option<Duration>,
    /// Bytes written to / read from the transport, including frame headers.
    pub sent: u64,
    pub received: u64,
    /// The peer's authenticated device key, when the connection runs over the secure channel.
    peer_key: Option<crate::identity::PublicKey>,
}

fn map_io(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::Timeout,
        _ => Error::Io(e),
    }
}

impl Channel {
    pub fn new(io: Box<dyn Duplex>, io_timeout: Option<Duration>) -> Self {
        Channel {
            io,
            max_frame: HANDSHAKE_MAX_FRAME,
            io_timeout,
            sent: 0,
            received: 0,
            peer_key: None,
        }
    }

    /// Records the authenticated identity of the peer (set by the secure handshake).
    pub fn with_peer_key(mut self, key: Option<crate::identity::PublicKey>) -> Self {
        self.peer_key = key;
        self
    }

    pub fn peer_key(&self) -> Option<&crate::identity::PublicKey> {
        self.peer_key.as_ref()
    }

    /// Bytes on the transport `(sent, received)`: ciphertext when encrypted, otherwise the
    /// frame bytes.
    pub fn wire_counts(&self) -> (u64, u64) {
        self.io.wire_counts().unwrap_or((self.sent, self.received))
    }

    pub fn set_max_frame(&mut self, n: u32) {
        self.max_frame = n.min(HARD_MAX_FRAME);
    }

    pub fn max_frame(&self) -> u32 {
        self.max_frame
    }

    pub fn peer(&self) -> String {
        self.io.peer()
    }

    pub fn shutdown(&mut self) {
        let _ = self.io.shutdown();
    }

    pub fn send(&mut self, payload: &[u8]) -> Result<()> {
        if payload.len() as u64 > self.max_frame as u64 {
            return Err(Error::protocol(format!(
                "refusing to send a {}-byte frame (limit {})",
                payload.len(),
                self.max_frame
            )));
        }
        // Timeouts are best effort: on a socket the peer has already closed (macOS reports
        // EINVAL) setting them fails, and the read/write below then reports what really
        // happened, including a final error frame that is still waiting to be read.
        let _ = self.io.set_timeouts(None, self.io_timeout);
        let mut head = [0u8; HEADER_LEN];
        head[..4].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        head[4..].copy_from_slice(&crc32fast::hash(payload).to_be_bytes());
        if payload.len() <= 16 * 1024 {
            let mut v = Vec::with_capacity(HEADER_LEN + payload.len());
            v.extend_from_slice(&head);
            v.extend_from_slice(payload);
            self.io.write_all(&v).map_err(map_io)?;
        } else {
            self.io.write_all(&head).map_err(map_io)?;
            self.io.write_all(payload).map_err(map_io)?;
        }
        self.io.flush().map_err(map_io)?;
        self.sent += (HEADER_LEN + payload.len()) as u64;
        Ok(())
    }

    /// Reads one frame. `idle` bounds the wait for the first byte (how long a quiet peer is
    /// tolerated); once a frame has begun, the channel's I/O timeout applies to the rest.
    pub fn recv(&mut self, idle: Option<Duration>) -> Result<Vec<u8>> {
        let _ = self.io.set_timeouts(idle, None);
        let mut first = [0u8; 1];
        loop {
            match self.io.read(&mut first) {
                Ok(0) => {
                    return Err(Error::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed",
                    )))
                }
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(map_io(e)),
            }
        }
        let _ = self.io.set_timeouts(self.io_timeout, None);
        let mut head = [0u8; HEADER_LEN];
        head[0] = first[0];
        self.io.read_exact(&mut head[1..]).map_err(map_io)?;
        let len = u32::from_be_bytes(head[..4].try_into().unwrap());
        let crc = u32::from_be_bytes(head[4..].try_into().unwrap());
        if len > self.max_frame {
            return Err(Error::protocol(format!(
                "frame of {len} bytes exceeds limit {}",
                self.max_frame
            )));
        }
        let mut payload = vec![0u8; len as usize];
        self.io.read_exact(&mut payload).map_err(map_io)?;
        self.received += (HEADER_LEN + payload.len()) as u64;
        if crc32fast::hash(&payload) != crc {
            return Err(Error::protocol("frame checksum mismatch"));
        }
        Ok(payload)
    }
}
