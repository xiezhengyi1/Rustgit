//! The encrypted, mutually authenticated channel. It wraps any [`Duplex`], so every transport
//! (TCP now, SSH and Iroh later) gets the same device authentication. Application framing
//! (`frame.rs`) runs unchanged on top of the resulting [`SecureDuplex`].
//!
//! Construction: **Noise XX** (`Noise_XX_25519_ChaChaPoly_BLAKE2s`, via the `snow` crate; see
//! docs/decisions.md D19 for why). Both sides send their static key encrypted. Each side also
//! sends a *proof* in the handshake payload: its Ed25519 identity key and a signature binding
//! that identity to the Noise static key. The client checks the host proof against the
//! fingerprint pinned from the invitation **before** it sends its own message 3, so a wrong
//! host never learns which device is connecting. The host learns the device's Ed25519 key
//! from message 3 and decides authorization above this layer.
//!
//! Wire: `magic(8)` then Noise handshake messages and, after the handshake, transport records,
//! each `u16 BE length | ciphertext` (ciphertext includes the 16-byte tag, so a record carries
//! at most 65519 bytes of plaintext). Any record that fails authentication kills the channel.

use std::io::{self, Read, Write};
use std::time::Duration;

use snow::params::NoiseParams;
use snow::{Builder, HandshakeState, TransportState};
use rustgit_store::codec::{Decoder, Encoder};

use crate::error::{Error, ErrorCode, Result, WireError};
use crate::identity::{pin_accepts, DeviceKey, Fingerprint, PublicKey, Succession};
use crate::msg::Response;
use crate::transport::Duplex;

/// First bytes of every secure connection. The first four read as a `u32` length far above
/// any frame limit, so a plaintext host rejects them immediately.
pub const MAGIC: [u8; 8] = *b"RGSEC\x00\x01\n";
const MAGIC_FAMILY: &[u8] = b"RGSEC";
const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"rustgit secure channel v1";
const MAX_RECORD: usize = 65535;
const TAG: usize = 16;
const MAX_PLAIN: usize = MAX_RECORD - TAG;
const BINDING_DOMAIN: &[u8] = b"rustgit noise static binding v1";
const PROOF_VERSION: u8 = 1;
const MAX_CHAIN: usize = 16;

fn params() -> NoiseParams {
    PATTERN.parse().expect("valid noise pattern")
}

fn hs_err(e: impl std::fmt::Display) -> Error {
    Error::Handshake(e.to_string())
}

// ---- identity proof carried in the handshake payload -----------------------------------------

struct Proof {
    key: PublicKey,
    sig: [u8; 64],
    chain: Vec<Succession>,
}

fn binding(noise_pub: &[u8], initiator: bool) -> Vec<u8> {
    let mut m = BINDING_DOMAIN.to_vec();
    m.push(initiator as u8);
    m.extend_from_slice(noise_pub);
    m
}

fn noise_public(secret: &[u8; 32]) -> Vec<u8> {
    use snow::params::DHChoice;
    use snow::resolvers::{CryptoResolver, DefaultResolver};
    let mut dh = DefaultResolver
        .resolve_dh(&DHChoice::Curve25519)
        .expect("curve25519 available");
    dh.set(secret);
    dh.pubkey().to_vec()
}

fn encode_proof(key: &DeviceKey, noise_pub: &[u8], initiator: bool, chain: &[Succession]) -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(PROOF_VERSION)
        .bytes(&key.public().0)
        .bytes(&key.sign(&binding(noise_pub, initiator)))
        .varint(chain.len() as u64);
    for l in chain {
        e.bytes(&l.to_bytes());
    }
    e.finish()
}

fn decode_proof(payload: &[u8]) -> Result<Proof> {
    let bad = |_| Error::Handshake("malformed identity proof".into());
    let mut d = Decoder::new(payload);
    if d.u8().map_err(bad)? != PROOF_VERSION {
        return Err(Error::Handshake("unsupported identity proof version".into()));
    }
    let key = PublicKey(
        d.bytes(32)
            .map_err(bad)?
            .try_into()
            .map_err(|_| Error::Handshake("malformed identity proof".into()))?,
    );
    let sig: [u8; 64] = d
        .bytes(64)
        .map_err(bad)?
        .try_into()
        .map_err(|_| Error::Handshake("malformed identity proof".into()))?;
    let n = d.varint().map_err(bad)? as usize;
    if n > MAX_CHAIN {
        return Err(Error::Handshake("key succession chain too long".into()));
    }
    let mut chain = Vec::with_capacity(n);
    for _ in 0..n {
        chain.push(
            Succession::from_bytes(d.bytes(128).map_err(bad)?)
                .ok_or_else(|| Error::Handshake("malformed key succession".into()))?,
        );
    }
    d.finish().map_err(bad)?;
    Ok(Proof { key, sig, chain })
}

/// The peer's Ed25519 key must have signed *this* connection's Noise static key.
fn verify_binding(p: &Proof, remote_static: &[u8], peer_is_initiator: bool) -> Result<()> {
    if !p.key.verify(&binding(remote_static, peer_is_initiator), &p.sig) {
        return Err(Error::Handshake(
            "peer identity is not bound to its channel key".into(),
        ));
    }
    Ok(())
}

// ---- handshake record I/O --------------------------------------------------------------------

fn write_hs(io: &mut dyn Duplex, prefix: &[u8], msg: &[u8]) -> io::Result<()> {
    let mut v = Vec::with_capacity(prefix.len() + 2 + msg.len());
    v.extend_from_slice(prefix);
    v.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    v.extend_from_slice(msg);
    io.write_all(&v)?;
    io.flush()
}

fn read_hs(io: &mut dyn Duplex) -> io::Result<Vec<u8>> {
    let mut l = [0u8; 2];
    io.read_exact(&mut l)?;
    let mut m = vec![0u8; u16::from_be_bytes(l) as usize];
    io.read_exact(&mut m)?;
    Ok(m)
}

fn closed_during_handshake(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => Error::Handshake(
            "the host closed the connection during the handshake; it may not accept the secure \
             channel (is it a plaintext host started with --insecure-open-access?)"
                .into(),
        ),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::Timeout,
        _ => Error::Io(e),
    }
}

// ---- the handshake ---------------------------------------------------------------------------

/// What the client learned about the host.
#[derive(Clone, Debug)]
pub struct HostAuth {
    pub host_key: PublicKey,
    /// The host presented a key different from the pin but proved succession from it. The
    /// caller should persist `host_key.fingerprint()` as the new pin.
    pub rotated: bool,
}

/// Client side. Verifies the host against `pinned` (exact key, or a verified succession chain
/// from it) and only then reveals the device identity.
pub fn connect_secure(
    mut io: Box<dyn Duplex>,
    me: &DeviceKey,
    pinned: &Fingerprint,
    timeout: Duration,
) -> Result<(Box<dyn Duplex>, HostAuth)> {
    io.set_timeouts(Some(timeout), Some(timeout))?;
    let secret = me.noise_static_secret();
    let my_pub = noise_public(&secret);
    let mut hs: HandshakeState = Builder::new(params())
        .local_private_key(&secret[..])
        .map_err(hs_err)?
        .prologue(PROLOGUE)
        .map_err(hs_err)?
        .build_initiator()
        .map_err(hs_err)?;
    let mut buf = vec![0u8; MAX_RECORD];
    let mut wire_sent = 0u64;
    let mut wire_recv = 0u64;

    // -> e
    let n = hs.write_message(&[], &mut buf).map_err(hs_err)?;
    write_hs(&mut *io, &MAGIC, &buf[..n]).map_err(closed_during_handshake)?;
    wire_sent += (MAGIC.len() + 2 + n) as u64;

    // <- e, ee, s, es  (+ host proof)
    let m2 = read_hs(&mut *io).map_err(closed_during_handshake)?;
    wire_recv += 2 + m2.len() as u64;
    let mut payload = vec![0u8; MAX_RECORD];
    let pn = hs.read_message(&m2, &mut payload).map_err(hs_err)?;
    let proof = decode_proof(&payload[..pn])?;
    let remote_static = hs
        .get_remote_static()
        .ok_or_else(|| Error::Handshake("host sent no static key".into()))?
        .to_vec();
    verify_binding(&proof, &remote_static, false)?;
    if !pin_accepts(pinned, &proof.key, &proof.chain) {
        return Err(Error::HostKeyMismatch {
            pinned: *pinned,
            presented: proof.key.fingerprint(),
        });
    }

    // -> s, se  (+ device proof). Only now does the host learn who we are.
    let out = encode_proof(me, &my_pub, true, &[]);
    let n = hs.write_message(&out, &mut buf).map_err(hs_err)?;
    write_hs(&mut *io, &[], &buf[..n]).map_err(Error::Io)?;
    wire_sent += 2 + n as u64;

    let ts = hs.into_transport_mode().map_err(hs_err)?;
    let rotated = proof.key.fingerprint() != *pinned;
    Ok((
        Box::new(SecureDuplex::new(io, ts, wire_sent, wire_recv)),
        HostAuth {
            host_key: proof.key,
            rotated,
        },
    ))
}

/// Host side. Returns the encrypted duplex and the authenticated device key. A peer that
/// speaks plaintext gets one plaintext `SecurityRequired` error frame and is dropped.
pub fn accept_secure(
    mut io: Box<dyn Duplex>,
    host: &DeviceKey,
    chain: &[Succession],
    timeout: Duration,
) -> Result<(Box<dyn Duplex>, PublicKey)> {
    io.set_timeouts(Some(timeout), Some(timeout))?;
    let mut magic = [0u8; 8];
    io.read_exact(&mut magic).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::Timeout,
        _ => Error::Io(e),
    })?;
    if magic != MAGIC {
        let msg = if magic.starts_with(MAGIC_FAMILY) {
            "unsupported secure channel version"
        } else {
            "this host only accepts encrypted, authenticated connections (join with an invitation, \
             or add the remote with --host-key)"
        };
        send_plain_error(&mut *io, ErrorCode::SecurityRequired, msg);
        // Swallow what the peer already sent so closing does not reset the connection and
        // destroy the error frame before the peer reads it.
        let _ = io.set_timeouts(Some(Duration::from_millis(300)), None);
        let mut sink = [0u8; 4096];
        let _ = io.read(&mut sink);
        return Err(Error::Handshake("peer did not start the secure channel".into()));
    }
    let secret = host.noise_static_secret();
    let my_pub = noise_public(&secret);
    let mut hs = Builder::new(params())
        .local_private_key(&secret[..])
        .map_err(hs_err)?
        .prologue(PROLOGUE)
        .map_err(hs_err)?
        .build_responder()
        .map_err(hs_err)?;
    let mut buf = vec![0u8; MAX_RECORD];
    let mut payload = vec![0u8; MAX_RECORD];
    let mut wire_recv = magic.len() as u64;
    let mut wire_sent = 0u64;

    // <- e
    let m1 = read_hs(&mut *io).map_err(closed_during_handshake)?;
    wire_recv += 2 + m1.len() as u64;
    hs.read_message(&m1, &mut payload).map_err(hs_err)?;

    // -> e, ee, s, es  (+ host proof)
    let out = encode_proof(host, &my_pub, false, chain);
    let n = hs.write_message(&out, &mut buf).map_err(hs_err)?;
    write_hs(&mut *io, &[], &buf[..n]).map_err(Error::Io)?;
    wire_sent += 2 + n as u64;

    // <- s, se  (+ device proof)
    let m3 = read_hs(&mut *io).map_err(closed_during_handshake)?;
    wire_recv += 2 + m3.len() as u64;
    let pn = hs.read_message(&m3, &mut payload).map_err(hs_err)?;
    let proof = decode_proof(&payload[..pn])?;
    let remote_static = hs
        .get_remote_static()
        .ok_or_else(|| Error::Handshake("device sent no static key".into()))?
        .to_vec();
    verify_binding(&proof, &remote_static, true)?;
    if !proof.key.is_valid() {
        return Err(Error::Handshake("invalid device key".into()));
    }
    let ts = hs.into_transport_mode().map_err(hs_err)?;
    Ok((
        Box::new(SecureDuplex::new(io, ts, wire_sent, wire_recv)),
        proof.key,
    ))
}

fn send_plain_error(io: &mut dyn Duplex, code: ErrorCode, msg: &str) {
    let payload = Response::Error(WireError::new(code, msg)).encode();
    let mut v = Vec::with_capacity(8 + payload.len());
    v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    v.extend_from_slice(&crc32fast::hash(&payload).to_be_bytes());
    v.extend_from_slice(&payload);
    let _ = io.write_all(&v);
    let _ = io.flush();
}

// ---- the encrypted stream --------------------------------------------------------------------

/// A [`Duplex`] whose bytes are Noise transport records. Writes are buffered and sealed into
/// one record per [`Write::flush`] (or when 64 KiB accumulate), so the frame layer's header +
/// payload become a single record.
pub struct SecureDuplex {
    io: Box<dyn Duplex>,
    ts: TransportState,
    dead: bool,
    // read state survives timeouts, so a slow peer can be polled with short deadlines
    hdr: [u8; 2],
    hdr_have: usize,
    body: Vec<u8>,
    body_have: usize,
    plain: Vec<u8>,
    plain_pos: usize,
    // write state
    wbuf: Vec<u8>,
    out: Vec<u8>,
    wire_sent: u64,
    wire_recv: u64,
}

impl SecureDuplex {
    fn new(io: Box<dyn Duplex>, ts: TransportState, wire_sent: u64, wire_recv: u64) -> Self {
        SecureDuplex {
            io,
            ts,
            dead: false,
            hdr: [0; 2],
            hdr_have: 0,
            body: Vec::new(),
            body_have: 0,
            plain: Vec::new(),
            plain_pos: 0,
            wbuf: Vec::with_capacity(MAX_PLAIN),
            out: Vec::new(),
            wire_sent,
            wire_recv,
        }
    }

    fn broken() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, "secure channel is broken")
    }

    /// Reads and decrypts one record into `plain`. `Ok(false)` is a clean end of stream.
    fn fill(&mut self) -> io::Result<bool> {
        if self.dead {
            return Err(Self::broken());
        }
        while self.hdr_have < 2 {
            let n = self.io.read(&mut self.hdr[self.hdr_have..])?;
            if n == 0 {
                return if self.hdr_have == 0 {
                    Ok(false)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
            self.hdr_have += n;
            self.wire_recv += n as u64;
        }
        let len = u16::from_be_bytes(self.hdr) as usize;
        if len < TAG {
            self.dead = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "secure record shorter than its tag",
            ));
        }
        self.body.resize(len, 0);
        while self.body_have < len {
            let n = self.io.read(&mut self.body[self.body_have..])?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.body_have += n;
            self.wire_recv += n as u64;
        }
        self.plain.resize(len, 0);
        let r = self.ts.read_message(&self.body, &mut self.plain);
        self.hdr_have = 0;
        self.body_have = 0;
        match r {
            Ok(n) => {
                self.plain.truncate(n);
                self.plain_pos = 0;
                Ok(true)
            }
            Err(_) => {
                self.dead = true;
                self.plain.clear();
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "secure record failed authentication (tampered, replayed or reordered)",
                ))
            }
        }
    }

    fn seal(&mut self) -> io::Result<()> {
        if self.wbuf.is_empty() {
            return Ok(());
        }
        if self.dead {
            return Err(Self::broken());
        }
        let mut out = std::mem::take(&mut self.out);
        out.clear();
        out.resize(2 + self.wbuf.len() + TAG, 0);
        let n = match self.ts.write_message(&self.wbuf, &mut out[2..]) {
            Ok(n) => n,
            Err(e) => {
                self.dead = true;
                return Err(io::Error::other(e.to_string()));
            }
        };
        out[..2].copy_from_slice(&(n as u16).to_be_bytes());
        out.truncate(2 + n);
        self.wbuf.clear();
        let r = self.io.write_all(&out);
        if r.is_err() {
            // a partial record cannot be resynchronised
            self.dead = true;
        } else {
            self.wire_sent += out.len() as u64;
        }
        self.out = out;
        r
    }
}

impl Read for SecureDuplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.plain_pos >= self.plain.len() {
            if !self.fill()? {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.plain.len() - self.plain_pos);
        buf[..n].copy_from_slice(&self.plain[self.plain_pos..self.plain_pos + n]);
        self.plain_pos += n;
        Ok(n)
    }
}

impl Write for SecureDuplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.dead {
            return Err(Self::broken());
        }
        let room = MAX_PLAIN - self.wbuf.len();
        let n = buf.len().min(room);
        self.wbuf.extend_from_slice(&buf[..n]);
        if self.wbuf.len() == MAX_PLAIN {
            self.seal()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.seal()?;
        self.io.flush()
    }
}

impl Duplex for SecureDuplex {
    fn set_timeouts(&mut self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()> {
        self.io.set_timeouts(read, write)
    }
    fn shutdown(&mut self) -> io::Result<()> {
        self.io.shutdown()
    }
    fn peer(&self) -> String {
        self.io.peer()
    }
    fn wire_counts(&self) -> Option<(u64, u64)> {
        Some((self.wire_sent, self.wire_recv))
    }
}
