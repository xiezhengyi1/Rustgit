//! Append-only pack record format (see docs/format.md §3.1).

use std::io::{ErrorKind, Read};

use crate::error::{Error, Result};
use crate::id::{ObjectId, ObjectType};

pub const MAGIC: [u8; 8] = *b"RGPK\x01\0\0\0";
pub const MAX_OBJECT: u64 = 64 * 1024 * 1024;
/// Depth writers use unless configured otherwise (the plan's limit).
pub const DEFAULT_MAX_DELTA_DEPTH: u32 = 4;
/// Deepest chain a reader accepts; guards against cycles and corrupt data.
pub const FORMAT_MAX_DELTA_DEPTH: u32 = 64;
/// Limits for solid groups of small objects (decompressed size / member count).
pub const MAX_GROUP_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_GROUP_MEMBERS: usize = 8192;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Encoding {
    Raw = 0,
    Zstd = 1,
    Delta = 2,
    /// Several small objects compressed together as one zstd frame (physical only).
    Group = 3,
}

impl Encoding {
    pub fn from_u8(v: u8) -> Result<Self> {
        Ok(match v {
            0 => Encoding::Raw,
            1 => Encoding::Zstd,
            2 => Encoding::Delta,
            3 => Encoding::Group,
            _ => return Err(Error::corrupt(format!("unknown pack encoding {v}"))),
        })
    }
}

#[derive(Clone, Debug)]
pub struct RecordHead {
    pub ty: ObjectType,
    pub enc: Encoding,
    pub logical_len: u64,
    pub stored_len: u64,
    pub base: Option<ObjectId>,
}

/// Serializes a record; returns bytes and the payload offset within them.
pub fn encode_record(head: &RecordHead, payload: &[u8]) -> (Vec<u8>, usize) {
    let mut e = crate::codec::Encoder::new();
    e.u8(head.ty as u8)
        .u8(head.enc as u8)
        .varint(head.logical_len)
        .varint(head.stored_len);
    if let Some(b) = &head.base {
        e.id(b);
    }
    let payload_off = e.buf.len();
    e.buf.extend_from_slice(payload);
    let crc = crc32fast::hash(&e.buf);
    e.buf.extend_from_slice(&crc.to_le_bytes());
    (e.finish(), payload_off)
}

pub enum ReadOutcome {
    /// Clean end of file at a record boundary.
    Eof,
    /// The file ends in the middle of a record.
    Torn,
    Record {
        head: RecordHead,
        payload: Vec<u8>,
        /// Offset of the payload relative to the record start.
        payload_off: usize,
        /// Total record length including trailer.
        len: u64,
    },
}

struct Tee<'a, R: Read> {
    inner: &'a mut R,
    hasher: crc32fast::Hasher,
    n: u64,
}

impl<R: Read> Tee<'_, R> {
    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        self.inner.read_exact(buf)?;
        self.hasher.update(buf);
        self.n += buf.len() as u64;
        Ok(())
    }
    fn byte(&mut self) -> std::io::Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn varint(&mut self) -> std::io::Result<std::result::Result<u64, ()>> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.byte()?;
            let low = (b & 0x7f) as u64;
            if i == 9 && low > 1 {
                return Ok(Err(()));
            }
            v |= low << (7 * i);
            if b & 0x80 == 0 {
                if i > 0 && low == 0 {
                    return Ok(Err(()));
                }
                return Ok(Ok(v));
            }
        }
        Ok(Err(()))
    }
}

/// Reads the next record from `r`.
pub fn read_record<R: Read>(r: &mut R) -> Result<ReadOutcome> {
    let mut first = [0u8; 1];
    loop {
        match r.read(&mut first) {
            Ok(0) => return Ok(ReadOutcome::Eof),
            Ok(_) => break,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let mut t = Tee {
        inner: r,
        hasher: crc32fast::Hasher::new(),
        n: 1,
    };
    t.hasher.update(&first);
    macro_rules! torn {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(ReadOutcome::Torn),
                Err(e) => return Err(e.into()),
            }
        };
    }
    let ty = ObjectType::from_u8(first[0])?;
    let enc = Encoding::from_u8(torn!(t.byte()))?;
    let logical_len = torn!(t.varint()).map_err(|_| Error::corrupt("bad varint in pack record"))?;
    let stored_len = torn!(t.varint()).map_err(|_| Error::corrupt("bad varint in pack record"))?;
    if logical_len > MAX_OBJECT || stored_len > logical_len {
        return Err(Error::corrupt("pack record length out of range"));
    }
    let base = if enc == Encoding::Delta {
        let mut b = [0u8; 32];
        torn!(t.read_exact(&mut b));
        Some(ObjectId(b))
    } else {
        None
    };
    let payload_off = t.n as usize;
    let mut payload = vec![0u8; stored_len as usize];
    torn!(t.read_exact(&mut payload));
    let expect = t.hasher.clone().finalize();
    let mut trailer = [0u8; 4];
    torn!(t.inner.read_exact(&mut trailer));
    if u32::from_le_bytes(trailer) != expect {
        return Err(Error::corrupt("pack record checksum mismatch"));
    }
    if enc == Encoding::Raw && stored_len != logical_len {
        return Err(Error::corrupt("raw record length mismatch"));
    }
    Ok(ReadOutcome::Record {
        head: RecordHead {
            ty,
            enc,
            logical_len,
            stored_len,
            base,
        },
        payload,
        payload_off,
        len: t.n + 4,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_and_torn_detection() {
        let head = RecordHead {
            ty: ObjectType::Chunk,
            enc: Encoding::Raw,
            logical_len: 5,
            stored_len: 5,
            base: None,
        };
        let (bytes, poff) = encode_record(&head, b"hello");
        let mut r = &bytes[..];
        match read_record(&mut r).unwrap() {
            ReadOutcome::Record {
                payload,
                payload_off,
                len,
                ..
            } => {
                assert_eq!(payload, b"hello");
                assert_eq!(payload_off, poff);
                assert_eq!(len as usize, bytes.len());
            }
            _ => panic!(),
        }
        assert!(matches!(read_record(&mut r).unwrap(), ReadOutcome::Eof));
        for cut in 1..bytes.len() {
            let mut r = &bytes[..cut];
            assert!(
                matches!(read_record(&mut r).unwrap(), ReadOutcome::Torn),
                "cut {cut}"
            );
        }
        let mut bad = bytes.clone();
        bad[poff] ^= 1;
        assert!(read_record(&mut &bad[..]).is_err());
    }
}
