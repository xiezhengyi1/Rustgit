//! Verification, garbage collection and statistics.

use std::collections::{HashSet, VecDeque};

use rustgit_store::{ObjectId, ObjectType, RepackOptions, RepackStats, StoreStats};

use crate::error::Result;
use crate::objects::{EntryKind, Tree};
use crate::repo::{Flat, Repo};

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub commits: u64,
    pub objects_checked: u64,
    pub problems: Vec<String>,
}

#[derive(Debug)]
pub struct GcReport {
    pub repack: RepackStats,
    pub freed_bytes: u64,
    pub live_objects: u64,
}

#[derive(Debug)]
pub struct RepoStats {
    pub store: StoreStats,
    pub repo_dir_bytes: u64,
    pub commits: u64,
    pub branches: u64,
}

impl Repo {
    /// Every reference that keeps history alive.
    pub fn root_commits(&self) -> Result<Vec<ObjectId>> {
        let mut roots: Vec<ObjectId> = Vec::new();
        for prefix in ["heads/", "tags/", "remotes/", "shelf/", "recovery/"] {
            roots.extend(self.list_refs(prefix)?.into_iter().map(|(_, id)| id));
        }
        if let Some(ms) = self.merge_state()? {
            roots.push(ms.ours);
            roots.extend(ms.extra_parents);
        }
        roots.sort();
        roots.dedup();
        Ok(roots)
    }

    /// Re-hashes the store and checks that every reachable commit, tree, manifest and
    /// chunk exists with consistent lengths.
    pub fn verify(&self) -> Result<VerifyReport> {
        let mut rep = VerifyReport::default();
        for p in self.store.verify_physical()? {
            rep.problems.push(format!("store: {p}"));
        }
        rep.objects_checked = self.store.stats()?.objects;
        let mut seen_commits: HashSet<ObjectId> = HashSet::new();
        let mut seen_trees: HashSet<ObjectId> = HashSet::new();
        let mut queue: VecDeque<ObjectId> = self.root_commits()?.into();
        while let Some(id) = queue.pop_front() {
            if !seen_commits.insert(id) {
                continue;
            }
            let c = match self.read_commit(&id) {
                Ok(c) => c,
                Err(e) => {
                    rep.problems.push(format!("commit {}: {e}", id.short()));
                    continue;
                }
            };
            rep.commits += 1;
            queue.extend(c.parents.iter().copied());
            self.verify_tree(&c.tree, 0, &mut seen_trees, &mut rep);
        }
        Ok(rep)
    }

    fn verify_tree(
        &self,
        id: &ObjectId,
        depth: usize,
        seen: &mut HashSet<ObjectId>,
        rep: &mut VerifyReport,
    ) {
        if !seen.insert(*id) {
            return;
        }
        if depth > crate::objects::MAX_DEPTH {
            rep.problems
                .push(format!("tree {}: nesting too deep", id.short()));
            return;
        }
        let t: Tree = match self.read_tree(id) {
            Ok(t) => t,
            Err(e) => {
                rep.problems.push(format!("tree {}: {e}", id.short()));
                return;
            }
        };
        for e in &t.entries {
            match e.kind {
                EntryKind::Dir => self.verify_tree(&e.id, depth + 1, seen, rep),
                EntryKind::File | EntryKind::Symlink => {
                    if !self.store.has(&e.id).unwrap_or(false) {
                        rep.problems
                            .push(format!("{}: chunk {} is missing", e.name, e.id.short()));
                    }
                }
                EntryKind::ChunkedFile => match self.read_manifest(&e.id) {
                    Err(err) => {
                        rep.problems
                            .push(format!("{}: manifest {}: {err}", e.name, e.id.short()))
                    }
                    Ok(m) => {
                        let mut total = 0u64;
                        for c in &m.chunks {
                            match self.store.logical_len(c) {
                                Ok(Some(l)) => total += l,
                                _ => rep.problems.push(format!(
                                    "{}: chunk {} is missing",
                                    e.name,
                                    c.short()
                                )),
                            }
                        }
                        if total != m.total_len
                            && rep.problems.iter().all(|p| !p.contains(&c_short(&e.id)))
                        {
                            rep.problems
                                .push(format!("{}: manifest length mismatch", e.name));
                        }
                    }
                },
            }
        }
    }

    /// Live objects in dependency order with delta hints (previous version of the same
    /// path / chunk position).
    fn live_items(&self) -> Result<Vec<(ObjectId, Option<ObjectId>)>> {
        let mut commits: Vec<ObjectId> = Vec::new();
        {
            let mut seen = HashSet::new();
            for r in self.root_commits()? {
                for (id, _) in self.history(&r, usize::MAX)? {
                    if seen.insert(id) {
                        commits.push(id);
                    }
                }
            }
        }
        // Oldest first so a hint always precedes the objects that use it.
        let mut ordered: Vec<(ObjectId, crate::objects::Commit)> = Vec::new();
        for id in commits {
            ordered.push((id, self.read_commit(&id)?));
        }
        let mut indeg_children: std::collections::HashMap<ObjectId, usize> = Default::default();
        for (_, c) in &ordered {
            for p in &c.parents {
                *indeg_children.entry(*p).or_default() += 1;
            }
        }
        // Topological (parents first).
        let by_id: std::collections::HashMap<ObjectId, &crate::objects::Commit> =
            ordered.iter().map(|(i, c)| (*i, c)).collect();
        let mut emitted: HashSet<ObjectId> = HashSet::new();
        let mut order: Vec<ObjectId> = Vec::new();
        let mut sorted_ids: Vec<ObjectId> = ordered.iter().map(|(i, _)| *i).collect();
        sorted_ids.sort_by_key(|i| by_id[i].timestamp);
        fn visit(
            id: ObjectId,
            by_id: &std::collections::HashMap<ObjectId, &crate::objects::Commit>,
            emitted: &mut HashSet<ObjectId>,
            order: &mut Vec<ObjectId>,
        ) {
            let mut stack = vec![(id, false)];
            while let Some((cur, expanded)) = stack.pop() {
                if emitted.contains(&cur) {
                    continue;
                }
                if expanded {
                    emitted.insert(cur);
                    order.push(cur);
                } else {
                    stack.push((cur, true));
                    for p in &by_id[&cur].parents {
                        if !emitted.contains(p) {
                            stack.push((*p, false));
                        }
                    }
                }
            }
        }
        for id in sorted_ids {
            visit(id, &by_id, &mut emitted, &mut order);
        }

        let mut items: Vec<(ObjectId, Option<ObjectId>)> = Vec::new();
        let mut seen: HashSet<ObjectId> = HashSet::new();
        for cid in order {
            let c = by_id[&cid];
            let parent_tree = c.parents.first().map(|p| by_id[p].tree);
            let parent_flat: Flat = match &parent_tree {
                Some(t) => self.flatten(t)?,
                None => Flat::new(),
            };
            self.collect_tree(
                &c.tree,
                parent_tree.as_ref(),
                "",
                &parent_flat,
                &mut items,
                &mut seen,
            )?;
            if seen.insert(cid) {
                items.push((cid, None));
            }
        }
        Ok(items)
    }

    fn collect_tree(
        &self,
        tree: &ObjectId,
        old_tree: Option<&ObjectId>,
        prefix: &str,
        parent_flat: &Flat,
        items: &mut Vec<(ObjectId, Option<ObjectId>)>,
        seen: &mut HashSet<ObjectId>,
    ) -> Result<()> {
        if seen.contains(tree) {
            return Ok(());
        }
        let t = self.read_tree(tree)?;
        let old = match old_tree {
            Some(o) => self.read_tree(o).ok(),
            None => None,
        };
        for e in &t.entries {
            let path = if prefix.is_empty() {
                e.name.clone()
            } else {
                format!("{prefix}/{}", e.name)
            };
            match e.kind {
                EntryKind::Dir => {
                    let old_child = old
                        .as_ref()
                        .and_then(|o| o.find(&e.name))
                        .filter(|x| x.kind == EntryKind::Dir)
                        .map(|x| x.id);
                    self.collect_tree(&e.id, old_child.as_ref(), &path, parent_flat, items, seen)?;
                }
                EntryKind::Symlink => {
                    if seen.insert(e.id) {
                        items.push((e.id, None));
                    }
                }
                EntryKind::File | EntryKind::ChunkedFile => {
                    let prev = self.hint_entry(parent_flat, &path);
                    let layout = match &prev {
                        Some(p) if p.id != e.id => self.layout_pub(p),
                        _ => vec![],
                    };
                    let hint_at = |off: u64| -> Option<ObjectId> {
                        let i = layout.partition_point(|&(_, end, _)| end <= off);
                        layout
                            .get(i)
                            .filter(|&&(s, _, _)| s <= off)
                            .map(|&(_, _, id)| id)
                    };
                    if e.kind == EntryKind::File {
                        if seen.insert(e.id) {
                            items.push((e.id, hint_at(0)));
                        }
                    } else {
                        let m = self.read_manifest(&e.id)?;
                        let mut off = 0u64;
                        for c in &m.chunks {
                            let l = self.store.logical_len(c)?.unwrap_or(0);
                            if seen.insert(*c) {
                                items.push((*c, hint_at(off)));
                            }
                            off += l;
                        }
                        if seen.insert(e.id) {
                            items.push((e.id, None));
                        }
                    }
                }
            }
        }
        if seen.insert(*tree) {
            items.push((*tree, old_tree.copied()));
        }
        Ok(())
    }

    /// Repacks idle packs keeping only history reachable from references.
    pub fn gc(
        &mut self,
        grace_secs: u64,
        prune_grace_secs: u64,
        aggressive: bool,
    ) -> Result<GcReport> {
        self.flush()?;
        let items = self.live_items()?;
        let live = items.len() as u64;
        let mut opts = if aggressive {
            RepackOptions::aggressive()
        } else {
            RepackOptions::default()
        };
        opts.grace_secs = grace_secs;
        let repack = self.store.repack(&items, &opts)?;
        let freed = self.store.prune(prune_grace_secs)?;
        // Anything that remembered "this object's dependencies were all present" (the
        // hosting layer does) must re-check: repacking drops unreachable objects, so a
        // kept-but-unreachable tree can lose a child that an older pack held.
        let epoch: u64 = self
            .get_config("maintenance.epoch")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        self.set_config("maintenance.epoch", &(epoch + 1).to_string())?;
        Ok(GcReport {
            repack,
            freed_bytes: freed,
            live_objects: live,
        })
    }

    pub fn stats(&self) -> Result<RepoStats> {
        let store = self.store.stats()?;
        let commits = store_commit_count(self)?;
        Ok(RepoStats {
            repo_dir_bytes: self.store.disk_usage()?,
            commits,
            branches: self.branches()?.len() as u64,
            store,
        })
    }

    pub fn reindex(&mut self) -> Result<rustgit_store::ReindexReport> {
        Ok(self.store.reindex()?)
    }
}

fn c_short(id: &ObjectId) -> String {
    id.short()
}

fn store_commit_count(r: &Repo) -> Result<u64> {
    let mut n = 0;
    for id in r.store.ids()? {
        if r.store.object_type(&id)? == Some(ObjectType::Commit) {
            n += 1;
        }
    }
    Ok(n)
}
