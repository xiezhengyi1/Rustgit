//! Deciding which logical objects must cross the wire.
//!
//! Both directions use the same rule. A commit that the receiver is known to hold
//! *completely* (every ancestor, tree, manifest and chunk it names) is a boundary. The
//! sender walks the commits that are not behind a boundary, and for each one lists only
//! the objects that differ from its first parent's tree, because everything else is
//! covered by that parent (either behind the boundary or listed for an earlier commit).
//! The list says nothing about pack layout or compression: it is a list of ids.

use std::collections::{HashMap, HashSet, VecDeque};

use rustgit_core::objects::{EntryKind, Manifest, Tree};
use rustgit_core::Repo;
use rustgit_store::{ObjectId, ObjectType};

use crate::error::Result;

/// Every commit reachable from `tips` (including the tips). Missing commits are an error:
/// callers only pass tips whose history is known to be complete.
pub fn reachable_commits(repo: &Repo, tips: &[ObjectId]) -> Result<HashSet<ObjectId>> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = tips.to_vec();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        stack.extend(repo.read_commit(&id)?.parents);
    }
    Ok(seen)
}

/// Commits reachable from `heads` that are not reachable from `exclude`, newest first in
/// breadth-first order.
pub fn new_commits(
    repo: &Repo,
    heads: &[ObjectId],
    exclude: &HashSet<ObjectId>,
) -> Result<Vec<ObjectId>> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut queue: VecDeque<ObjectId> = VecDeque::new();
    let mut out = Vec::new();
    for h in heads {
        if !exclude.contains(h) && seen.insert(*h) {
            queue.push_back(*h);
        }
    }
    while let Some(id) = queue.pop_front() {
        out.push(id);
        for p in repo.read_commit(&id)?.parents {
            if !exclude.contains(&p) && seen.insert(p) {
                queue.push_back(p);
            }
        }
    }
    Ok(out)
}

/// Orders `commits` so every commit comes after all of its parents that are also in the set.
pub fn oldest_first(repo: &Repo, commits: &[ObjectId]) -> Result<Vec<ObjectId>> {
    let set: HashSet<ObjectId> = commits.iter().copied().collect();
    let mut done: HashSet<ObjectId> = HashSet::new();
    let mut out = Vec::with_capacity(commits.len());
    for c in commits {
        // Iterative post-order over parents restricted to the set.
        let mut stack: Vec<(ObjectId, bool)> = vec![(*c, false)];
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                out.push(id);
                continue;
            }
            if !set.contains(&id) || !done.insert(id) {
                continue;
            }
            stack.push((id, true));
            for p in repo.read_commit(&id)?.parents {
                if set.contains(&p) && !done.contains(&p) {
                    stack.push((p, false));
                }
            }
        }
    }
    Ok(out)
}

/// An object to transfer plus, optionally, a similar object (the same path in the first
/// parent) that the receiver is guaranteed to hold by the time this one is processed. The
/// hint lets the wire carry a delta instead of the whole object.
#[derive(Clone, Copy, Debug)]
pub struct PlanObject {
    pub ty: ObjectType,
    pub id: ObjectId,
    pub hint: Option<ObjectId>,
}

/// The objects introduced by `commits`: the commits themselves and, for each, the trees,
/// manifests and chunks that differ from its first parent. Every object appears once.
/// Objects are listed oldest commit first, so a hint always precedes the object using it.
pub fn enumerate_objects(repo: &Repo, commits: &[ObjectId]) -> Result<Vec<(ObjectType, ObjectId)>> {
    Ok(enumerate_hinted(repo, commits)?
        .into_iter()
        .map(|o| (o.ty, o.id))
        .collect())
}

pub fn enumerate_hinted(repo: &Repo, commits: &[ObjectId]) -> Result<Vec<PlanObject>> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut out: Vec<PlanObject> = Vec::new();
    for c in oldest_first(repo, commits)? {
        let commit = repo.read_commit(&c)?;
        if seen.insert(c) {
            out.push(PlanObject {
                ty: ObjectType::Commit,
                id: c,
                hint: commit.parents.first().copied(),
            });
        }
        let parent_tree = match commit.parents.first() {
            Some(p) => Some(repo.read_commit(p)?.tree),
            None => None,
        };
        walk_tree(repo, commit.tree, parent_tree, &mut seen, &mut out)?;
    }
    Ok(out)
}

fn walk_tree(
    repo: &Repo,
    tree: ObjectId,
    parent: Option<ObjectId>,
    seen: &mut HashSet<ObjectId>,
    out: &mut Vec<PlanObject>,
) -> Result<()> {
    if parent == Some(tree) || !seen.insert(tree) {
        return Ok(());
    }
    out.push(PlanObject {
        ty: ObjectType::Tree,
        id: tree,
        hint: parent,
    });
    let t = repo.read_tree(&tree)?;
    let p: Option<Tree> = match parent {
        Some(pid) => Some(repo.read_tree(&pid)?),
        None => None,
    };
    // The previous file of the same kind and extension in this directory: a likely relative
    // (numbered snapshots, copies) when the path has no counterpart in the parent. It is
    // either in the offered list ahead of the current object or covered by the parent, so
    // the receiver holds it by the time it is needed.
    let mut prev_sibling: HashMap<(String, EntryKind), ObjectId> = HashMap::new();
    for e in &t.entries {
        let pe = p.as_ref().and_then(|p| p.find(&e.name));
        let sib_key = (
            e.name.rsplit_once('.').map(|(_, x)| x.to_string()).unwrap_or_default(),
            e.kind,
        );
        let sibling = prev_sibling.get(&sib_key).copied();
        if matches!(e.kind, EntryKind::File | EntryKind::ChunkedFile) {
            prev_sibling.insert(sib_key, e.id);
        }
        if let Some(pe) = pe {
            if pe.id == e.id {
                continue;
            }
        }
        match e.kind {
            EntryKind::Dir => {
                let ptree = pe.filter(|x| x.kind == EntryKind::Dir).map(|x| x.id);
                walk_tree(repo, e.id, ptree, seen, out)?;
            }
            EntryKind::File | EntryKind::Symlink => {
                if seen.insert(e.id) {
                    let hint = pe
                        .filter(|x| x.kind == e.kind)
                        .map(|x| x.id)
                        .or(if e.kind == EntryKind::File { sibling } else { None });
                    out.push(PlanObject {
                        ty: ObjectType::Chunk,
                        id: e.id,
                        hint,
                    });
                }
            }
            EntryKind::ChunkedFile => {
                let parent_manifest = match pe {
                    Some(x) if x.kind == EntryKind::ChunkedFile => Some(x.id),
                    _ => None,
                };
                let pchunks: HashSet<ObjectId> = match parent_manifest {
                    Some(id) => repo.read_manifest(&id)?.chunks.into_iter().collect(),
                    None => HashSet::new(),
                };
                // Pair against the parent's version, else against the sibling file.
                let base_manifest = parent_manifest.or(sibling);
                let base_chunks: Option<Vec<ObjectId>> = match base_manifest {
                    Some(id) => Some(repo.read_manifest(&id)?.chunks),
                    None => None,
                };
                if seen.insert(e.id) {
                    out.push(PlanObject {
                        ty: ObjectType::Manifest,
                        id: e.id,
                        hint: base_manifest,
                    });
                    let m: Manifest = repo.read_manifest(&e.id)?;
                    let hints = match &base_chunks {
                        Some(old) => pair_chunks(&m.chunks, old),
                        None => vec![None; m.chunks.len()],
                    };
                    for (ch, hint) in m.chunks.iter().zip(hints) {
                        if !pchunks.contains(ch) && seen.insert(*ch) {
                            out.push(PlanObject {
                                ty: ObjectType::Chunk,
                                id: *ch,
                                hint,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// For each chunk of a file's new version, a chunk of the old version that probably holds
/// similar bytes. Chunks both versions share are anchors; the new chunks between two anchors
/// are paired in order with the old chunks between the same anchors (an edit usually replaces
/// the chunk or chunks in place, while content-defined boundaries re-synchronize after it).
pub fn pair_chunks(new: &[ObjectId], old: &[ObjectId]) -> Vec<Option<ObjectId>> {
    let mut pos: std::collections::HashMap<ObjectId, usize> = std::collections::HashMap::new();
    for (i, c) in old.iter().enumerate() {
        pos.entry(*c).or_insert(i);
    }
    let anchor = |c: &ObjectId, from: usize| pos.get(c).copied().filter(|&p| p >= from);
    let mut hints = vec![None; new.len()];
    let mut next_old = 0usize;
    let mut i = 0usize;
    while i < new.len() {
        if let Some(p) = anchor(&new[i], next_old) {
            next_old = p + 1;
            i += 1;
            continue;
        }
        let mut j = i;
        while j < new.len() && anchor(&new[j], next_old).is_none() {
            j += 1;
        }
        let old_end = if j < new.len() {
            anchor(&new[j], next_old).unwrap_or(old.len())
        } else {
            old.len()
        };
        let run = &old[next_old.min(old_end)..old_end];
        for (k, slot) in hints[i..j].iter_mut().enumerate() {
            *slot = run.get(k).or(run.last()).copied();
        }
        i = j;
    }
    hints
}

/// Checks that everything `commits` introduce is present in `repo` and decodes. Used by a
/// receiver before it lets a ref point at newly received history.
pub fn verify_present(repo: &Repo, commits: &[ObjectId]) -> Result<Vec<ObjectId>> {
    let mut missing = Vec::new();
    for (_, id) in enumerate_objects(repo, commits)? {
        if !repo.store.has(&id)? {
            missing.push(id);
        }
    }
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgit_core::Repo;

    #[test]
    fn chunk_pairing_follows_anchors() {
        let id = |n: u8| ObjectId([n; 32]);
        // b is replaced in place by x; a, c, d are shared.
        let h = pair_chunks(&[id(1), id(9), id(3), id(4)], &[id(1), id(2), id(3), id(4)]);
        assert_eq!(h, vec![None, Some(id(2)), None, None]);
        // One chunk split in two: both new chunks get the old chunk (the second reuses the last).
        let h = pair_chunks(&[id(1), id(8), id(9), id(3)], &[id(1), id(2), id(3)]);
        assert_eq!(h[1], Some(id(2)));
        assert_eq!(h[2], Some(id(2)));
        // Two old chunks merged into one new chunk.
        let h = pair_chunks(&[id(1), id(9), id(4)], &[id(1), id(2), id(3), id(4)]);
        assert_eq!(h[1], Some(id(2)));
        // Appended data has nothing to pair with beyond the last old chunk.
        let h = pair_chunks(&[id(1), id(2), id(7), id(8)], &[id(1), id(2)]);
        assert_eq!(h[2], None);
        assert_eq!(h[3], None);
        // No old version.
        assert_eq!(pair_chunks(&[id(1)], &[]), vec![None]);
    }

    fn commit_files(repo: &mut Repo, files: &[(&str, Vec<u8>)], msg: &str) -> ObjectId {
        for (p, c) in files {
            let path = repo.root.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        repo.commit(msg, &[]).unwrap()
    }

    #[test]
    fn only_changed_objects_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let mut repo = Repo::init(dir.path(), "main").unwrap();
        let c1 = commit_files(
            &mut repo,
            &[
                ("a/x.txt", b"one".to_vec()),
                ("a/y.txt", b"two".to_vec()),
                ("b/z.txt", b"three".to_vec()),
            ],
            "c1",
        );
        let c2 = commit_files(&mut repo, &[("a/x.txt", b"ONE".to_vec())], "c2");
        let all = new_commits(&repo, &[c2], &HashSet::new()).unwrap();
        assert_eq!(all.len(), 2);
        let full = enumerate_objects(&repo, &all).unwrap();
        let exclude = reachable_commits(&repo, &[c1]).unwrap();
        let inc = new_commits(&repo, &[c2], &exclude).unwrap();
        assert_eq!(inc, vec![c2]);
        let delta = enumerate_objects(&repo, &inc).unwrap();
        // commit + root tree + tree a + chunk "ONE"; tree b and y.txt are unchanged.
        assert_eq!(delta.len(), 4, "{delta:?}");
        assert!(delta.len() < full.len());
        let ids: HashSet<_> = delta.iter().map(|(_, i)| *i).collect();
        assert_eq!(ids.len(), delta.len());
        assert!(verify_present(&repo, &inc).unwrap().is_empty());
    }
}
