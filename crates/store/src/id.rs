use std::fmt;

use crate::error::{Error, Result};

/// 32-byte BLAKE3 object identifier (domain separated by object type).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub [u8; 32]);

impl ObjectId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_slice(b: &[u8]) -> Result<Self> {
        let arr: [u8; 32] = b
            .try_into()
            .map_err(|_| Error::malformed("object id must be 32 bytes"))?;
        Ok(ObjectId(arr))
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        if s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::malformed("object id must be 64 hex characters"));
        }
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        Ok(ObjectId(out))
    }

    /// Short form for human display.
    pub fn short(&self) -> String {
        self.to_hex()[..10].to_string()
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.short())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum ObjectType {
    Chunk = 0,
    Manifest = 1,
    Tree = 2,
    Commit = 3,
}

impl ObjectType {
    pub fn from_u8(v: u8) -> Result<Self> {
        Ok(match v {
            0 => ObjectType::Chunk,
            1 => ObjectType::Manifest,
            2 => ObjectType::Tree,
            3 => ObjectType::Commit,
            _ => return Err(Error::corrupt(format!("unknown object type {v}"))),
        })
    }

    /// BLAKE3 derive-key context; part of the permanent format.
    pub fn context(self) -> &'static str {
        match self {
            ObjectType::Chunk => "rustgit v1 object:chunk",
            ObjectType::Manifest => "rustgit v1 object:manifest",
            ObjectType::Tree => "rustgit v1 object:tree",
            ObjectType::Commit => "rustgit v1 object:commit",
        }
    }
}

pub fn hash_object(ty: ObjectType, data: &[u8]) -> ObjectId {
    let mut h = blake3::Hasher::new_derive_key(ty.context());
    h.update(data);
    ObjectId(*h.finalize().as_bytes())
}
