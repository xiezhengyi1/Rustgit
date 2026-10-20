//! Canonical binary primitives shared by all object encodings.

use crate::error::{Error, Result};
use crate::id::ObjectId;

#[derive(Default)]
pub struct Encoder {
    pub buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn varint(&mut self, mut v: u64) -> &mut Self {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(byte);
                return self;
            }
            self.buf.push(byte | 0x80);
        }
    }
    pub fn zigzag(&mut self, v: i64) -> &mut Self {
        self.varint(((v << 1) ^ (v >> 63)) as u64)
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.varint(b.len() as u64);
        self.buf.extend_from_slice(b);
        self
    }
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }
    pub fn id(&mut self, id: &ObjectId) -> &mut Self {
        self.buf.extend_from_slice(&id.0);
        self
    }
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn u8(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| Error::malformed("unexpected end of data"))?;
        self.pos += 1;
        Ok(b)
    }

    pub fn varint(&mut self) -> Result<u64> {
        let mut v: u64 = 0;
        for i in 0..10 {
            let b = self.u8()?;
            let low = (b & 0x7f) as u64;
            if i == 9 && low > 1 {
                return Err(Error::malformed("varint overflows u64"));
            }
            v |= low << (7 * i);
            if b & 0x80 == 0 {
                if i > 0 && low == 0 {
                    return Err(Error::malformed("non-minimal varint"));
                }
                return Ok(v);
            }
        }
        Err(Error::malformed("varint too long"))
    }

    pub fn zigzag(&mut self) -> Result<i64> {
        let u = self.varint()?;
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }

    /// Reads a length-prefixed byte string, refusing lengths above `max`.
    pub fn bytes(&mut self, max: usize) -> Result<&'a [u8]> {
        let len = self.varint()?;
        if len > max as u64 {
            return Err(Error::Limit(format!(
                "field of {len} bytes exceeds limit {max}"
            )));
        }
        let len = len as usize;
        if self.remaining() < len {
            return Err(Error::malformed("length prefix exceeds data"));
        }
        let s = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(s)
    }

    pub fn string(&mut self, max: usize) -> Result<&'a str> {
        std::str::from_utf8(self.bytes(max)?).map_err(|_| Error::malformed("string is not UTF-8"))
    }

    pub fn id(&mut self) -> Result<ObjectId> {
        if self.remaining() < 32 {
            return Err(Error::malformed("truncated object id"));
        }
        let id = ObjectId::from_slice(&self.buf[self.pos..self.pos + 32])?;
        self.pos += 32;
        Ok(id)
    }

    pub fn finish(self) -> Result<()> {
        if self.pos != self.buf.len() {
            return Err(Error::malformed("trailing bytes after object"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip_and_minimal() {
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut e = Encoder::new();
            e.varint(v);
            let b = e.finish();
            let mut d = Decoder::new(&b);
            assert_eq!(d.varint().unwrap(), v);
            d.finish().unwrap();
        }
        // 0x80 0x00 is an overlong encoding of 0
        assert!(Decoder::new(&[0x80, 0x00]).varint().is_err());
        // 11-byte varint
        assert!(Decoder::new(&[0xff; 11]).varint().is_err());
    }

    #[test]
    fn zigzag_roundtrip() {
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -1234567] {
            let mut e = Encoder::new();
            e.zigzag(v);
            let b = e.finish();
            assert_eq!(Decoder::new(&b).zigzag().unwrap(), v);
        }
    }

    #[test]
    fn length_limits_are_enforced() {
        let mut e = Encoder::new();
        e.bytes(&[1, 2, 3]);
        let b = e.finish();
        assert!(Decoder::new(&b).bytes(2).is_err());
        assert!(Decoder::new(&b[..3]).bytes(10).is_err());
    }
}
