//! Tree comparison, rename presentation and text diffs.

use std::collections::HashMap;

use crate::error::Result;
use crate::objects::EntryKind;
use crate::repo::{Flat, FlatEntry, Repo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// Exact-content rename from the given path (presentation only).
    Renamed(String),
}

#[derive(Clone, Debug)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    pub old: Option<FlatEntry>,
    pub new: Option<FlatEntry>,
}

pub fn diff_flat(a: &Flat, b: &Flat) -> Vec<Change> {
    let mut out = Vec::new();
    for (p, e) in a {
        match b.get(p) {
            None => out.push(Change {
                path: p.clone(),
                kind: ChangeKind::Deleted,
                old: Some(e.clone()),
                new: None,
            }),
            Some(n) if n != e => out.push(Change {
                path: p.clone(),
                kind: ChangeKind::Modified,
                old: Some(e.clone()),
                new: Some(n.clone()),
            }),
            _ => {}
        }
    }
    for (p, n) in b {
        if !a.contains_key(p) {
            out.push(Change {
                path: p.clone(),
                kind: ChangeKind::Added,
                old: None,
                new: Some(n.clone()),
            });
        }
    }
    out.sort_by(|x, y| x.path.cmp(&y.path));
    out
}

/// Pairs deleted and added files with identical content. Heuristic, display only.
pub fn detect_renames(changes: Vec<Change>) -> Vec<Change> {
    let has = |k: ChangeKind| changes.iter().any(|c| c.kind == k);
    if !has(ChangeKind::Added) || !has(ChangeKind::Deleted) {
        return changes;
    }
    let mut deleted: HashMap<_, Vec<usize>> = HashMap::new();
    for (i, c) in changes.iter().enumerate() {
        if c.kind == ChangeKind::Deleted {
            if let Some(o) = &c.old {
                if o.kind != EntryKind::Symlink {
                    deleted.entry(o.id).or_default().push(i);
                }
            }
        }
    }
    let mut consumed = vec![false; changes.len()];
    let mut out = Vec::new();
    for (i, c) in changes.iter().enumerate() {
        if c.kind == ChangeKind::Added {
            if let Some(n) = &c.new {
                if let Some(list) = deleted.get_mut(&n.id) {
                    if let Some(di) = list.pop() {
                        consumed[di] = true;
                        out.push(Change {
                            path: c.path.clone(),
                            kind: ChangeKind::Renamed(changes[di].path.clone()),
                            old: changes[di].old.clone(),
                            new: c.new.clone(),
                        });
                        consumed[i] = true;
                    }
                }
            }
        }
    }
    for (i, c) in changes.into_iter().enumerate() {
        if !consumed[i] {
            out.push(c);
        }
    }
    out.sort_by(|x, y| x.path.cmp(&y.path));
    out
}

pub fn is_text(bytes: &[u8]) -> bool {
    bytes.len() <= 16 * 1024 * 1024 && !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

pub fn unified_diff(path: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> String {
    let empty: &[u8] = b"";
    let (o, n) = (old.unwrap_or(empty), new.unwrap_or(empty));
    if !is_text(o) || !is_text(n) {
        return format!("Binary files a/{path} and b/{path} differ\n");
    }
    let (os, ns) = (
        std::str::from_utf8(o).unwrap(),
        std::str::from_utf8(n).unwrap(),
    );
    let a = if old.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".to_string()
    };
    let b = if new.is_some() {
        format!("b/{path}")
    } else {
        "/dev/null".to_string()
    };
    similar::TextDiff::from_lines(os, ns)
        .unified_diff()
        .context_radius(3)
        .header(&a, &b)
        .to_string()
}

impl Repo {
    /// Unified diff between two flat trees for the given changes.
    pub fn render_diff(&self, changes: &[Change]) -> Result<String> {
        self.render_diff_inner(changes, false)
    }

    /// Like `render_diff`, but the "new" side lives in the working directory (not
    /// necessarily stored yet), so it is read from disk.
    pub fn render_diff_work(&self, changes: &[Change]) -> Result<String> {
        self.render_diff_inner(changes, true)
    }

    fn render_diff_inner(&self, changes: &[Change], new_from_work: bool) -> Result<String> {
        let mut out = String::new();
        for c in changes {
            let label = match &c.kind {
                ChangeKind::Renamed(from) => {
                    out.push_str(&format!("rename {from} -> {}\n", c.path));
                    continue;
                }
                _ => c.path.as_str(),
            };
            let old = c
                .old
                .as_ref()
                .map(|e| self.read_file_bytes(e))
                .transpose()?;
            let new = match (&c.new, new_from_work) {
                (Some(e), false) => Some(self.read_file_bytes(e)?),
                (Some(e), true) if e.kind == EntryKind::Symlink => {
                    Some(self.read_file_bytes(e).unwrap_or_default())
                }
                (Some(_), true) => Some(std::fs::read(self.root.join(&c.path))?),
                (None, _) => None,
            };
            out.push_str(&unified_diff(label, old.as_deref(), new.as_deref()));
        }
        Ok(out)
    }
}
