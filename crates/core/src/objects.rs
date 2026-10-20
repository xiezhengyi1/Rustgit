//! Logical object model: manifests, trees, commits (docs/format.md §1).

use rustgit_store::codec::{Decoder, Encoder};
use rustgit_store::{hash_object, ObjectId, ObjectType};

use crate::error::{Error, Result};

pub const FORMAT_VERSION: u8 = 1;
pub const MAX_NAME: usize = 255;
pub const MAX_TREE_ENTRIES: u64 = 1_000_000;
pub const MAX_MANIFEST_CHUNKS: u64 = 1_048_576;
pub const MAX_PARENTS: u64 = 8;
pub const MAX_MESSAGE: usize = 1 << 20;
pub const MAX_IDENT: usize = 1024;
pub const MAX_DEPTH: usize = 256;
pub const RESERVED_ROOT_NAME: &str = ".rgit";

fn bad(m: impl Into<String>) -> Error {
    Error::Invalid(m.into())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum EntryKind {
    File = 1,
    ChunkedFile = 2,
    Dir = 3,
    Symlink = 4,
}

impl EntryKind {
    fn from_u8(v: u8) -> Result<Self> {
        Ok(match v {
            1 => EntryKind::File,
            2 => EntryKind::ChunkedFile,
            3 => EntryKind::Dir,
            4 => EntryKind::Symlink,
            _ => return Err(bad(format!("unknown tree entry kind {v}"))),
        })
    }
    pub fn is_file(self) -> bool {
        matches!(self, EntryKind::File | EntryKind::ChunkedFile)
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(bad(format!(
            "entry name length {} out of range",
            name.len()
        )));
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(bad(format!("illegal entry name {name:?}")));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub kind: EntryKind,
    pub exec: bool,
    pub id: ObjectId,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.u8(FORMAT_VERSION).varint(self.entries.len() as u64);
        for t in &self.entries {
            e.string(&t.name)
                .u8(t.kind as u8)
                .u8(t.exec as u8)
                .id(&t.id);
        }
        e.finish()
    }

    pub fn decode(data: &[u8]) -> Result<Tree> {
        let mut d = Decoder::new(data);
        let v = d.u8()?;
        if v != FORMAT_VERSION {
            return Err(Error::Invalid(format!("unsupported tree version {v}")));
        }
        let n = d.varint()?;
        if n > MAX_TREE_ENTRIES {
            return Err(bad("too many tree entries"));
        }
        let mut entries: Vec<TreeEntry> = Vec::with_capacity(n.min(4096) as usize);
        for _ in 0..n {
            let name = d.string(MAX_NAME)?.to_string();
            validate_name(&name)?;
            let kind = EntryKind::from_u8(d.u8()?)?;
            let flags = d.u8()?;
            if flags > 1 || (flags == 1 && !kind.is_file()) {
                return Err(bad("invalid entry flags"));
            }
            let id = d.id()?;
            if let Some(prev) = entries.last() {
                if prev.name.as_bytes() >= name.as_bytes() {
                    return Err(bad("tree entries not strictly sorted"));
                }
            }
            entries.push(TreeEntry {
                name,
                kind,
                exec: flags == 1,
                id,
            });
        }
        d.finish()?;
        Ok(Tree { entries })
    }

    pub fn id(&self) -> ObjectId {
        hash_object(ObjectType::Tree, &self.encode())
    }

    pub fn find(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_bytes().cmp(name.as_bytes()))
            .ok()
            .map(|i| &self.entries[i])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub total_len: u64,
    pub chunks: Vec<ObjectId>,
}

impl Manifest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.u8(FORMAT_VERSION)
            .varint(self.total_len)
            .varint(self.chunks.len() as u64);
        for c in &self.chunks {
            e.id(c);
        }
        e.finish()
    }

    pub fn decode(data: &[u8]) -> Result<Manifest> {
        let mut d = Decoder::new(data);
        let v = d.u8()?;
        if v != FORMAT_VERSION {
            return Err(bad(format!("unsupported manifest version {v}")));
        }
        let total_len = d.varint()?;
        let n = d.varint()?;
        if !(2..=MAX_MANIFEST_CHUNKS).contains(&n) {
            return Err(bad("manifest chunk count out of range"));
        }
        let mut chunks = Vec::with_capacity(n.min(4096) as usize);
        for _ in 0..n {
            chunks.push(d.id()?);
        }
        d.finish()?;
        Ok(Manifest { total_len, chunks })
    }

    pub fn id(&self) -> ObjectId {
        hash_object(ObjectType::Manifest, &self.encode())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub parents: Vec<ObjectId>,
    pub tree: ObjectId,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub tz_offset_min: i32,
    pub message: String,
}

impl Commit {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.u8(FORMAT_VERSION).varint(self.parents.len() as u64);
        for p in &self.parents {
            e.id(p);
        }
        e.id(&self.tree)
            .string(&self.author_name)
            .string(&self.author_email)
            .zigzag(self.timestamp)
            .zigzag(self.tz_offset_min as i64)
            .string(&self.message);
        e.finish()
    }

    pub fn decode(data: &[u8]) -> Result<Commit> {
        let mut d = Decoder::new(data);
        let v = d.u8()?;
        if v != FORMAT_VERSION {
            return Err(bad(format!("unsupported commit version {v}")));
        }
        let n = d.varint()?;
        if n > MAX_PARENTS {
            return Err(bad("too many parents"));
        }
        let mut parents: Vec<ObjectId> = Vec::new();
        for _ in 0..n {
            let p = d.id()?;
            if parents.contains(&p) {
                return Err(bad("duplicate parent"));
            }
            parents.push(p);
        }
        let tree = d.id()?;
        let author_name = d.string(MAX_IDENT)?.to_string();
        let author_email = d.string(MAX_IDENT)?.to_string();
        let timestamp = d.zigzag()?;
        let tz = d.zigzag()?;
        if !(-1440..=1440).contains(&tz) {
            return Err(bad("timezone offset out of range"));
        }
        let message = d.string(MAX_MESSAGE)?.to_string();
        d.finish()?;
        Ok(Commit {
            parents,
            tree,
            author_name,
            author_email,
            timestamp,
            tz_offset_min: tz as i32,
            message,
        })
    }

    pub fn id(&self) -> ObjectId {
        hash_object(ObjectType::Commit, &self.encode())
    }
}

/// The objects a logical object points at, with their expected types: a commit names its
/// parents and tree, a tree its entries, a manifest its chunks. This is the dependency
/// relation that "closed" (fully present) history is defined by; synchronization uses it
/// to verify that nothing a ref points at is missing. Decoding is strict, so a malformed
/// object is an error rather than an empty list.
pub fn references(ty: ObjectType, data: &[u8]) -> Result<Vec<(ObjectType, ObjectId)>> {
    Ok(match ty {
        ObjectType::Chunk => Vec::new(),
        ObjectType::Manifest => Manifest::decode(data)?
            .chunks
            .into_iter()
            .map(|c| (ObjectType::Chunk, c))
            .collect(),
        ObjectType::Tree => Tree::decode(data)?
            .entries
            .into_iter()
            .map(|e| {
                let t = match e.kind {
                    EntryKind::File | EntryKind::Symlink => ObjectType::Chunk,
                    EntryKind::ChunkedFile => ObjectType::Manifest,
                    EntryKind::Dir => ObjectType::Tree,
                };
                (t, e.id)
            })
            .collect(),
        ObjectType::Commit => {
            let c = Commit::decode(data)?;
            let mut v: Vec<_> = c
                .parents
                .into_iter()
                .map(|p| (ObjectType::Commit, p))
                .collect();
            v.push((ObjectType::Tree, c.tree));
            v
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ObjectId {
        ObjectId([n; 32])
    }

    #[test]
    fn tree_roundtrip_and_ordering() {
        let t = Tree {
            entries: vec![
                TreeEntry {
                    name: "a".into(),
                    kind: EntryKind::File,
                    exec: true,
                    id: id(1),
                },
                TreeEntry {
                    name: "b".into(),
                    kind: EntryKind::Dir,
                    exec: false,
                    id: id(2),
                },
            ],
        };
        assert_eq!(Tree::decode(&t.encode()).unwrap(), t);
        let swapped = Tree {
            entries: vec![t.entries[1].clone(), t.entries[0].clone()],
        };
        assert!(Tree::decode(&swapped.encode()).is_err());
        let mut dup = t.clone();
        dup.entries.push(t.entries[1].clone());
        assert!(Tree::decode(&dup.encode()).is_err());
    }

    #[test]
    fn rejects_bad_names_flags_and_trailing_bytes() {
        for n in ["", ".", "..", "a/b", "a\0b"] {
            assert!(validate_name(n).is_err(), "{n:?}");
        }
        let bad_flag = Tree {
            entries: vec![TreeEntry {
                name: "d".into(),
                kind: EntryKind::Dir,
                exec: true,
                id: id(1),
            }],
        };
        assert!(Tree::decode(&bad_flag.encode()).is_err());
        let mut b = Tree::default().encode();
        b.push(0);
        assert!(Tree::decode(&b).is_err());
    }

    #[test]
    fn commit_and_manifest_roundtrip() {
        let c = Commit {
            parents: vec![id(1), id(2)],
            tree: id(3),
            author_name: "Ann".into(),
            author_email: "ann@example.com".into(),
            timestamp: -5,
            tz_offset_min: 480,
            message: "héllo".into(),
        };
        assert_eq!(Commit::decode(&c.encode()).unwrap(), c);
        let dup = Commit {
            parents: vec![id(1), id(1)],
            ..c.clone()
        };
        assert!(Commit::decode(&dup.encode()).is_err());
        let m = Manifest {
            total_len: 10,
            chunks: vec![id(1), id(2)],
        };
        assert_eq!(Manifest::decode(&m.encode()).unwrap(), m);
        let one = Manifest {
            total_len: 10,
            chunks: vec![id(1)],
        };
        assert!(Manifest::decode(&one.encode()).is_err());
    }
}
