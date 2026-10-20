//! Three-way tree merge. Text merges automatically when safe; everything else is a
//! recorded conflict that needs an explicit decision.

use std::collections::BTreeSet;

use rustgit_store::chunker;
use rustgit_store::{ObjectId, ObjectType};

use crate::diff::is_text;
use crate::error::{Error, Result};
use crate::objects::{EntryKind, Manifest};
use crate::repo::{Flat, FlatEntry, Repo};

#[derive(Clone, Debug)]
pub struct MergeConflict {
    pub path: String,
    pub kind: &'static str,
    pub base: Option<FlatEntry>,
    pub ours: Option<FlatEntry>,
    pub theirs: Option<FlatEntry>,
    /// Marker text for working-directory display (text conflicts only).
    pub text: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub merged: Flat,
    pub conflicts: Vec<MergeConflict>,
}

impl Repo {
    /// Stores in-memory bytes using the repository's chunking rules.
    pub fn store_bytes(&mut self, data: &[u8]) -> Result<(EntryKind, ObjectId)> {
        if data.len() as u64 <= self.cfg.small_file_max {
            return Ok((
                EntryKind::File,
                self.store.put(ObjectType::Chunk, data, None)?,
            ));
        }
        let mut chunks = Vec::new();
        for (off, len) in chunker::split(data, &self.cfg.chunker) {
            chunks.push(
                self.store
                    .put(ObjectType::Chunk, &data[off..off + len], None)?,
            );
        }
        if chunks.len() == 1 {
            return Ok((EntryKind::File, chunks[0]));
        }
        let m = Manifest {
            total_len: data.len() as u64,
            chunks,
        };
        Ok((
            EntryKind::ChunkedFile,
            self.store.put(ObjectType::Manifest, &m.encode(), None)?,
        ))
    }

    pub fn merge_flat(&mut self, base: &Flat, ours: &Flat, theirs: &Flat) -> Result<Outcome> {
        let paths: BTreeSet<&String> = base
            .keys()
            .chain(ours.keys())
            .chain(theirs.keys())
            .collect();
        let mut out = Outcome::default();
        for p in paths {
            let (b, o, t) = (base.get(p), ours.get(p), theirs.get(p));
            let result = if o == t {
                o.cloned()
            } else if o == b {
                t.cloned()
            } else if t == b {
                o.cloned()
            } else {
                match self.merge_entry(p, b, o, t)? {
                    Ok(e) => Some(e),
                    Err(c) => {
                        out.conflicts.push(c);
                        continue;
                    }
                }
            };
            if let Some(e) = result {
                out.merged.insert(p.clone(), e);
            }
        }
        // A path may not be both a file and a directory.
        let mut all: Vec<&String> = out.merged.keys().collect();
        all.extend(out.conflicts.iter().map(|c| &c.path));
        let set: BTreeSet<&str> = all.iter().map(|s| s.as_str()).collect();
        for p in &set {
            let mut idx = 0;
            while let Some(i) = p[idx..].find('/') {
                let pre = &p[..idx + i];
                if set.contains(pre) {
                    return Err(Error::Other(format!(
                        "file/directory conflict: '{pre}' is a file on one side and a directory on the other; \
                         rename one of them on its branch and merge again"
                    )));
                }
                idx += i + 1;
            }
        }
        Ok(out)
    }

    fn merge_entry(
        &mut self,
        path: &str,
        b: Option<&FlatEntry>,
        o: Option<&FlatEntry>,
        t: Option<&FlatEntry>,
    ) -> Result<std::result::Result<FlatEntry, MergeConflict>> {
        let conflict = |kind: &'static str, text: Option<Vec<u8>>| MergeConflict {
            path: path.to_string(),
            kind,
            base: b.cloned(),
            ours: o.cloned(),
            theirs: t.cloned(),
            text,
        };
        let (Some(o), Some(t)) = (o, t) else {
            return Ok(Err(conflict("delete/modify", None)));
        };
        let file = |e: &FlatEntry| e.kind.is_file();
        if !file(o) || !file(t) || b.map(|b| !file(b)).unwrap_or(false) {
            return Ok(Err(conflict("type", None)));
        }
        let bytes_b = match b {
            Some(b) => self.read_file_bytes(b)?,
            None => Vec::new(),
        };
        let (bytes_o, bytes_t) = (self.read_file_bytes(o)?, self.read_file_bytes(t)?);
        if !(is_text(&bytes_b) && is_text(&bytes_o) && is_text(&bytes_t)) {
            return Ok(Err(conflict("binary", None)));
        }
        let (sb, so, st) = (
            std::str::from_utf8(&bytes_b).unwrap(),
            std::str::from_utf8(&bytes_o).unwrap(),
            std::str::from_utf8(&bytes_t).unwrap(),
        );
        match diffy::merge(sb, so, st) {
            Ok(merged) => {
                let (kind, id) = self.store_bytes(merged.as_bytes())?;
                let exec = match b {
                    Some(b) if o.exec != b.exec => o.exec,
                    Some(_) => t.exec,
                    None => o.exec,
                };
                Ok(Ok(FlatEntry { kind, exec, id }))
            }
            Err(markers) => Ok(Err(conflict("content", Some(markers.into_bytes())))),
        }
    }
}
