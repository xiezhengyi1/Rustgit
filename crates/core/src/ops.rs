//! User-level version-control operations.

use std::fs;
use std::path::Path;

use rustgit_store::{ObjectId, ObjectType};

use crate::diff::{detect_renames, diff_flat, Change};
use crate::error::{Error, Result};
use crate::objects::Commit;
use crate::repo::{validate_ref_name, Conflict, Flat, FlatEntry, MergeState, Repo};
use crate::worktree::{portability_problems, Op, THEIRS_SUFFIX};

#[derive(Debug)]
pub struct Status {
    pub branch: String,
    pub head: Option<ObjectId>,
    pub changes: Vec<Change>,
    pub skipped: Vec<(String, String)>,
    pub merge: Option<MergeState>,
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug)]
pub struct MergeResult {
    pub commit: Option<ObjectId>,
    pub fast_forward: bool,
    pub already_up_to_date: bool,
    pub conflicts: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveWith {
    Ours,
    Theirs,
    /// The user edited the working file (or removed it) and says it is final.
    Manual,
}

fn normalize_sel(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .map(|p| {
            let p = p.replace('\\', "/");
            let p = p.trim_start_matches("./").trim_end_matches('/').to_string();
            if p == "." {
                String::new()
            } else {
                p
            }
        })
        .collect()
}

/// Path selection for commits. Lookups are O(path depth), so selecting tens of thousands of
/// files stays linear (the slice form of `selected` is O(selection size) per path).
struct Selection {
    all: bool,
    include: std::collections::HashSet<String>,
    except: std::collections::HashSet<String>,
}

impl Selection {
    fn new(paths: &[String], except: &[String]) -> Self {
        let inc = normalize_sel(paths);
        Selection {
            all: inc.is_empty() || inc.iter().any(|s| s.is_empty()),
            include: inc.into_iter().filter(|s| !s.is_empty()).collect(),
            except: normalize_sel(except).into_iter().collect(),
        }
    }

    /// Everything is committed (no restriction at all).
    fn is_full(&self) -> bool {
        self.all && self.except.is_empty()
    }

    fn has(&self, path: &str) -> bool {
        let mut in_scope = self.all;
        let mut hit_except = false;
        let mut end = path.len();
        loop {
            let prefix = &path[..end];
            if !in_scope && self.include.contains(prefix) {
                in_scope = true;
            }
            if self.except.contains(prefix) {
                hit_except = true;
            }
            match prefix.rfind('/') {
                Some(i) => end = i,
                None => break,
            }
        }
        in_scope && !hit_except
    }
}

fn selected(sel: &[String], path: &str) -> bool {
    sel.iter().any(|s| {
        s.is_empty()
            || path == s
            || (path.starts_with(s.as_str()) && path.as_bytes().get(s.len()) == Some(&b'/'))
    })
}

fn has_markers(bytes: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(bytes) else {
        return false;
    };
    s.lines()
        .any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "=======")
}

impl Repo {
    pub fn status(&mut self) -> Result<Status> {
        let head = self.head_commit()?;
        let head_flat = self.head_flat()?;
        let scan = self.scan_with_tracked(&head_flat)?;
        let work = self.work_flat(&scan, false, &head_flat, None)?;
        let changes = detect_renames(diff_flat(&head_flat, &work));
        Ok(Status {
            branch: self.current_branch()?,
            head,
            changes,
            skipped: scan.skipped,
            merge: self.merge_state()?,
            conflicts: self.conflicts()?,
        })
    }

    fn ensure_clean(&mut self) -> Result<()> {
        let st = self.status()?;
        if !st.changes.is_empty() {
            return Err(Error::DirtyWorkingTree(
                st.changes.into_iter().map(|c| c.path).collect(),
            ));
        }
        Ok(())
    }

    fn timestamp() -> (i64, i32) {
        let now = chrono::Local::now();
        (now.timestamp(), now.offset().local_minus_utc() / 60)
    }

    fn make_commit(
        &mut self,
        parents: Vec<ObjectId>,
        tree: ObjectId,
        message: &str,
    ) -> Result<ObjectId> {
        let (author_name, author_email) = self.author()?;
        let (timestamp, tz_offset_min) = Self::timestamp();
        let c = Commit {
            parents,
            tree,
            author_name,
            author_email,
            timestamp,
            tz_offset_min,
            message: message.to_string(),
        };
        Ok(self.store.put(ObjectType::Commit, &c.encode(), None)?)
    }

    /// Commits the working directory (or only `paths`). Objects are made durable before
    /// the branch reference moves.
    pub fn commit(&mut self, message: &str, paths: &[String]) -> Result<ObjectId> {
        self.commit_sel(message, paths, &[])
    }

    /// Commits every change except the listed files or directories.
    pub fn commit_except(&mut self, message: &str, except: &[String]) -> Result<ObjectId> {
        self.commit_sel(message, &[], except)
    }

    fn commit_sel(&mut self, message: &str, paths: &[String], except: &[String]) -> Result<ObjectId> {
        let ms = self.merge_state()?;
        let head = self.head_commit()?;
        let head_flat = self.head_flat()?;
        let sel = Selection::new(paths, except);
        let mut parents: Vec<ObjectId> = head.into_iter().collect();
        let mut msg = message.to_string();
        if let Some(ms) = &ms {
            if !sel.is_full() {
                return Err(Error::Other(
                    "a merge is in progress: partial commits are not possible".into(),
                ));
            }
            let unresolved = self.conflicts()?.iter().filter(|c| !c.resolved).count();
            if unresolved > 0 {
                return Err(Error::UnresolvedConflicts(unresolved));
            }
            parents.extend(ms.extra_parents.iter().copied());
            if msg.trim().is_empty() {
                msg = ms.message.clone();
            }
        }
        if msg.trim().is_empty() {
            return Err(Error::Other("commit message must not be empty".into()));
        }
        let scan = self.scan_with_tracked(&head_flat)?;
        let new_flat = if sel.is_full() {
            self.work_flat(&scan, true, &head_flat, None)?
        } else {
            let pred = |p: &str| sel.has(p);
            let mut f: Flat = head_flat
                .iter()
                .filter(|(p, _)| !pred(p))
                .map(|(p, e)| (p.clone(), e.clone()))
                .collect();
            f.extend(self.work_flat(&scan, true, &head_flat, Some(&pred))?);
            f
        };
        let problems = portability_problems(new_flat.keys().map(String::as_str));
        if !problems.is_empty() {
            return Err(Error::Unportable(problems));
        }
        let old_root = match head {
            Some(h) => Some(self.read_commit(&h)?.tree),
            None => None,
        };
        let tree = self.write_tree(&new_flat, old_root.as_ref())?;
        if parents.len() <= 1 && old_root == Some(tree) || (head.is_none() && new_flat.is_empty()) {
            return Err(Error::NothingToCommit);
        }
        let id = self.make_commit(parents, tree, &msg)?;
        self.flush()?;
        let branch = self.current_branch()?;
        self.cas_ref(&format!("heads/{branch}"), head, Some(id))?;
        if ms.is_some() {
            self.clear_merge_state()?;
        }
        Ok(id)
    }

    // ----- branches -------------------------------------------------------------------

    /// Creating a branch writes one reference row; no content is copied.
    pub fn branch_create(&mut self, name: &str, from: Option<&str>) -> Result<ObjectId> {
        validate_ref_name(name)?;
        let target = match from {
            Some(r) => self.resolve_rev(r)?,
            None => self
                .head_commit()?
                .ok_or_else(|| Error::Other("no commits yet".into()))?,
        };
        if self
            .get_ref(&format!("heads/{name}"))?
            .and_then(|r| r.commit)
            .is_some()
        {
            return Err(Error::BranchExists(name.into()));
        }
        let cur = self
            .get_ref(&format!("heads/{name}"))?
            .and_then(|r| r.commit);
        self.cas_ref(&format!("heads/{name}"), cur, Some(target))?;
        Ok(target)
    }

    pub fn branch_delete(&mut self, name: &str, force: bool) -> Result<ObjectId> {
        if name == self.current_branch()? {
            return Err(Error::DeleteCurrent(name.into()));
        }
        let tip = self
            .get_ref(&format!("heads/{name}"))?
            .and_then(|r| r.commit)
            .ok_or_else(|| Error::BranchNotFound(name.into()))?;
        if !force {
            if let Some(h) = self.head_commit()? {
                if !self.is_ancestor(&tip, &h)? {
                    return Err(Error::Other(format!(
                        "branch '{name}' is not merged into '{}' (tip {}); use --force to delete it anyway",
                        self.current_branch()?,
                        tip.short()
                    )));
                }
            }
        }
        self.cas_ref(&format!("heads/{name}"), Some(tip), None)?;
        Ok(tip)
    }

    pub fn switch(&mut self, name: &str, create: bool) -> Result<()> {
        if self.merge_state()?.is_some() {
            return Err(Error::MergeInProgress);
        }
        if create {
            self.branch_create(name, None)?;
        }
        let target = self
            .get_ref(&format!("heads/{name}"))?
            .and_then(|r| r.commit)
            .ok_or_else(|| Error::BranchNotFound(name.into()))?;
        let cur = self.head_flat()?;
        let tgt = self.commit_flat(&target)?;
        self.checkout_flat(&cur, &tgt, false)?;
        self.set_config("head", name)?;
        Ok(())
    }

    // ----- recovery snapshots, restore, shelve ----------------------------------------

    /// Records the working state (of `sel`, or everything) as a commit under a protected
    /// reference so it can be recovered later. Returns the reference name and commit.
    fn snapshot_working(
        &mut self,
        ref_prefix: &str,
        message: &str,
        sel: &[String],
        additional_paths: Option<&Flat>,
    ) -> Result<(String, ObjectId)> {
        let head = self.head_commit()?;
        let head_flat = self.head_flat()?;
        let mut scan = self.scan_with_tracked(&head_flat)?;
        if let Some(additional) = additional_paths { self.include_work_files(&mut scan, additional); }
        let flat = if sel.is_empty() {
            self.work_flat(&scan, true, &head_flat, None)?
        } else {
            let pred = |p: &str| selected(sel, p);
            let mut f: Flat = head_flat
                .iter()
                .filter(|(p, _)| !pred(p))
                .map(|(p, e)| (p.clone(), e.clone()))
                .collect();
            f.extend(self.work_flat(&scan, true, &head_flat, Some(&pred))?);
            f
        };
        let old_root = match head {
            Some(h) => Some(self.read_commit(&h)?.tree),
            None => None,
        };
        let tree = self.write_tree(&flat, old_root.as_ref())?;
        let id = self.make_commit(head.into_iter().collect(), tree, message)?;
        self.flush()?;
        let n = self
            .list_refs(&format!("{ref_prefix}/"))?
            .iter()
            .filter_map(|(n, _)| n.split('-').next().and_then(|x| x.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            + 1;
        let name = format!("{ref_prefix}/{n}");
        self.cas_ref(&name, None, Some(id))?;
        Ok((name, id))
    }

    /// Restores `paths` from `rev` (default HEAD). Uncommitted edits to those paths are
    /// only overwritten with `force`, and then a recovery snapshot is kept first.
    pub fn restore(
        &mut self,
        rev: Option<&str>,
        paths: &[String],
        force: bool,
    ) -> Result<Vec<String>> {
        let sel = normalize_sel(paths);
        if sel.is_empty() {
            return Err(Error::Other(
                "name the files or directories to restore".into(),
            ));
        }
        let head_flat = self.head_flat()?;
        let src = match rev {
            Some(r) => {
                let c = self.resolve_rev(r)?;
                self.commit_flat(&c)?
            }
            None => head_flat.clone(),
        };
        let mut scan = self.scan_with_tracked(&head_flat)?;
        self.include_work_files(&mut scan, &src);
        let work = self.work_flat(&scan, false, &head_flat, Some(&|p: &str| selected(&sel, p)))?;
        let mut ops = Vec::new();
        let mut dirty = Vec::new();
        let candidates: std::collections::BTreeSet<&String> = src
            .keys()
            .chain(head_flat.keys())
            .chain(work.keys())
            .filter(|p| selected(&sel, p))
            .collect();
        for p in &candidates {
            let want = src.get(*p);
            let have = work.get(*p);
            if want == have {
                continue;
            }
            // Whole-version restore preserves unrelated untracked files.
            if want.is_none() && !head_flat.contains_key(*p) {
                continue;
            }
            // Restoring a file the user deleted loses nothing; overwriting edits does.
            if have.is_some() && have != head_flat.get(*p) {
                dirty.push((*p).clone());
            }
            match want {
                Some(e) => ops.push(Op::Write {
                    path: (*p).clone(),
                    entry: e.clone(),
                }),
                None if have.is_some() => {
                    if !head_flat.contains_key(*p) {
                        // untracked file that does not exist in the source revision: leave it
                        continue;
                    }
                    ops.push(Op::Delete { path: (*p).clone() })
                }
                None => {}
            }
        }
        if ops.is_empty()
            && !sel.iter().any(|s| {
                candidates
                    .iter()
                    .any(|c| selected(std::slice::from_ref(s), c))
            })
        {
            return Err(Error::Other(format!(
                "no tracked file matches {}",
                sel.join(", ")
            )));
        }
        let problems = portability_problems(ops.iter().filter_map(|o| match o {
            Op::Write { path, .. } => Some(path.as_str()),
            _ => None,
        }));
        if !problems.is_empty() {
            return Err(Error::Unportable(problems));
        }
        if !dirty.is_empty() {
            if !force {
                return Err(Error::DirtyWorkingTree(dirty));
            }
            self.snapshot_working("recovery", "recovery snapshot before restore", &sel, Some(&src))?;
        }
        self.apply_ops(&ops)?;
        Ok(ops
            .iter()
            .map(|o| match o {
                Op::Write { path, .. } | Op::Delete { path } => path.clone(),
            })
            .collect())
    }

    /// Saves all uncommitted work as a shelf and resets the working directory to HEAD.
    pub fn shelve(&mut self, message: &str) -> Result<String> {
        if self.merge_state()?.is_some() {
            return Err(Error::MergeInProgress);
        }
        let st = self.status()?;
        if st.changes.is_empty() {
            return Err(Error::NothingToCommit);
        }
        let msg = if message.trim().is_empty() {
            "shelved changes"
        } else {
            message
        };
        let head_flat = self.head_flat()?;
        let scan = self.scan_with_tracked(&head_flat)?;
        let work = self.work_flat(&scan, true, &head_flat, None)?;
        let (name, _) = self.snapshot_working("shelf", msg, &[], None)?;
        let ops = Self::plan_ops(&work, &head_flat);
        self.apply_ops(&ops)?;
        Ok(name)
    }

    pub fn unshelve(&mut self, name: &str) -> Result<MergeResult> {
        let id = self
            .get_ref(&format!("shelf/{name}"))?
            .and_then(|r| r.commit)
            .ok_or_else(|| Error::BranchNotFound(format!("shelf/{name}")))?;
        let c = self.read_commit(&id)?;
        let base = match c.parents.first() {
            Some(p) => self.commit_flat(p)?,
            None => Flat::new(),
        };
        let theirs = self.flatten(&c.tree)?;
        self.apply_three_way(
            "unshelve",
            &format!("shelf/{name}"),
            &base,
            &theirs,
            vec![],
            String::new(),
            false,
        )
    }

    pub fn drop_shelf(&mut self, name: &str) -> Result<()> {
        let full = format!("shelf/{name}");
        let cur = self
            .get_ref(&full)?
            .and_then(|r| r.commit)
            .ok_or_else(|| Error::BranchNotFound(full.clone()))?;
        self.cas_ref(&full, Some(cur), None)?;
        Ok(())
    }

    // ----- merge / revert / resolve ---------------------------------------------------

    pub fn merge(&mut self, branch: &str) -> Result<MergeResult> {
        if self.merge_state()?.is_some() {
            return Err(Error::MergeInProgress);
        }
        // A local branch wins; otherwise a remote-tracking branch such as `origin/main`.
        let mut theirs = self.get_ref(&format!("heads/{branch}"))?.and_then(|r| r.commit);
        if theirs.is_none() {
            theirs = self
                .get_ref(&format!("remotes/{branch}"))?
                .and_then(|r| r.commit);
        }
        let theirs_id = theirs.ok_or_else(|| Error::BranchNotFound(branch.into()))?;
        let Some(ours_id) = self.head_commit()? else {
            // Unborn branch: adopt the other branch outright.
            let tgt = self.commit_flat(&theirs_id)?;
            self.checkout_flat(&Flat::new(), &tgt, false)?;
            let cur = self.current_branch()?;
            self.cas_ref(&format!("heads/{cur}"), None, Some(theirs_id))?;
            return Ok(MergeResult {
                commit: Some(theirs_id),
                fast_forward: true,
                already_up_to_date: false,
                conflicts: vec![],
            });
        };
        if ours_id == theirs_id || self.is_ancestor(&theirs_id, &ours_id)? {
            return Ok(MergeResult {
                commit: None,
                fast_forward: false,
                already_up_to_date: true,
                conflicts: vec![],
            });
        }
        let cur = self.current_branch()?;
        if self.is_ancestor(&ours_id, &theirs_id)? {
            let cur_flat = self.commit_flat(&ours_id)?;
            let tgt = self.commit_flat(&theirs_id)?;
            self.checkout_flat(&cur_flat, &tgt, false)?;
            self.cas_ref(&format!("heads/{cur}"), Some(ours_id), Some(theirs_id))?;
            return Ok(MergeResult {
                commit: Some(theirs_id),
                fast_forward: true,
                already_up_to_date: false,
                conflicts: vec![],
            });
        }
        let base_id = self
            .merge_base(&ours_id, &theirs_id)?
            .ok_or_else(|| Error::Other("refusing to merge unrelated histories".into()))?;
        let base = self.commit_flat(&base_id)?;
        let theirs = self.commit_flat(&theirs_id)?;
        let msg = format!("Merge branch '{branch}' into '{cur}'");
        self.apply_three_way("merge", branch, &base, &theirs, vec![theirs_id], msg, true)
    }

    /// Creates a commit that undoes `rev` (its first-parent change).
    pub fn revert(&mut self, rev: &str) -> Result<MergeResult> {
        if self.merge_state()?.is_some() {
            return Err(Error::MergeInProgress);
        }
        let id = self.resolve_rev(rev)?;
        let c = self.read_commit(&id)?;
        let parent = c
            .parents
            .first()
            .ok_or_else(|| Error::Other("cannot revert a root commit".into()))?;
        let base = self.flatten(&c.tree)?;
        let theirs = self.commit_flat(parent)?;
        let first = c.message.lines().next().unwrap_or("");
        let msg = format!("Revert \"{first}\"\n\nThis reverts commit {id}.");
        self.apply_three_way("revert", &id.short(), &base, &theirs, vec![], msg, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_three_way(
        &mut self,
        kind: &str,
        label: &str,
        base: &Flat,
        theirs: &Flat,
        extra_parents: Vec<ObjectId>,
        message: String,
        auto_commit: bool,
    ) -> Result<MergeResult> {
        self.ensure_clean()?;
        let ours_id = self
            .head_commit()?
            .ok_or_else(|| Error::Other("no commits yet".into()))?;
        let ours = self.commit_flat(&ours_id)?;
        let outcome = self.merge_flat(base, &ours, theirs)?;
        // Working target: merged result, with conflicted paths showing "ours".
        let mut target = outcome.merged.clone();
        for c in &outcome.conflicts {
            if let Some(o) = &c.ours {
                target.insert(c.path.clone(), o.clone());
            }
        }
        self.checkout_flat(&ours, &target, false)?;
        let mut conflicts: Vec<Conflict> = Vec::new();
        for c in &outcome.conflicts {
            let abs = self.root.join(&c.path);
            if let Some(text) = &c.text {
                if let Some(parent) = abs.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&abs, text)?;
            } else if let Some(t) = &c.theirs {
                let side = format!("{}{THEIRS_SUFFIX}", c.path);
                let mut f = fs::File::create(self.root.join(&side))?;
                self.write_file_to(t, &mut f)?;
            }
            conflicts.push(Conflict {
                path: c.path.clone(),
                kind: c.kind.to_string(),
                base: c.base.clone(),
                ours: c.ours.clone(),
                theirs: c.theirs.clone(),
                resolved: false,
            });
        }
        let names: Vec<String> = conflicts.iter().map(|c| c.path.clone()).collect();
        if conflicts.is_empty() {
            if !auto_commit {
                return Ok(MergeResult {
                    commit: None,
                    fast_forward: false,
                    already_up_to_date: false,
                    conflicts: names,
                });
            }
            let ours_tree = self.read_commit(&ours_id)?.tree;
            let tree = self.write_tree(&target, Some(&ours_tree))?;
            if tree == ours_tree && extra_parents.is_empty() {
                return Err(Error::NothingToCommit);
            }
            let mut parents = vec![ours_id];
            parents.extend(extra_parents);
            let id = self.make_commit(parents, tree, &message)?;
            self.flush()?;
            let cur = self.current_branch()?;
            self.cas_ref(&format!("heads/{cur}"), Some(ours_id), Some(id))?;
            return Ok(MergeResult {
                commit: Some(id),
                fast_forward: false,
                already_up_to_date: false,
                conflicts: names,
            });
        }
        self.flush()?;
        self.set_merge_state(
            &MergeState {
                kind: kind.into(),
                label: label.into(),
                ours: ours_id,
                extra_parents,
                message,
            },
            &conflicts,
        )?;
        Ok(MergeResult {
            commit: None,
            fast_forward: false,
            already_up_to_date: false,
            conflicts: names,
        })
    }

    pub fn merge_abort(&mut self) -> Result<()> {
        let st = self.merge_state()?.ok_or(Error::NoMerge)?;
        let cur_flat = self.commit_flat(&st.ours)?;
        let scan = self.scan_with_tracked(&cur_flat)?;
        let work = self.work_flat(&scan, false, &cur_flat, None)?;
        for c in self.conflicts()? {
            let _ = fs::remove_file(self.root.join(format!("{}{THEIRS_SUFFIX}", c.path)));
        }
        let ops = Self::plan_ops(&work, &cur_flat);
        self.apply_ops(&ops)?;
        self.clear_merge_state()
    }

    pub fn resolve(&mut self, path: &str, how: ResolveWith) -> Result<()> {
        let st = self.merge_state()?.ok_or(Error::NoMerge)?;
        let path = normalize_sel(&[path.to_string()]).remove(0);
        let c = self
            .conflicts()?
            .into_iter()
            .find(|c| c.path == path)
            .ok_or_else(|| Error::Other(format!("'{path}' is not in conflict")))?;
        let abs = self.root.join(&path);
        match how {
            ResolveWith::Ours | ResolveWith::Theirs => {
                let side = if how == ResolveWith::Ours {
                    &c.ours
                } else {
                    &c.theirs
                };
                match side {
                    Some(e) => {
                        let ops = [Op::Write {
                            path: path.clone(),
                            entry: e.clone(),
                        }];
                        self.apply_ops(&ops)?;
                    }
                    None => self.apply_ops(&[Op::Delete { path: path.clone() }])?,
                }
            }
            ResolveWith::Manual => {
                if let Ok(bytes) = fs::read(&abs) {
                    if c.kind == "content" && has_markers(&bytes) {
                        return Err(Error::Other(format!(
                            "'{path}' still contains conflict markers"
                        )));
                    }
                }
            }
        }
        let _ = fs::remove_file(self.root.join(format!("{path}{THEIRS_SUFFIX}")));
        self.mark_resolved(&path)?;
        if st.kind == "unshelve" && self.conflicts()?.iter().all(|c| c.resolved) {
            self.clear_merge_state()?;
        }
        Ok(())
    }

    pub fn show_commit(&self, id: &ObjectId) -> Result<(Commit, Vec<Change>)> {
        let c = self.read_commit(id)?;
        let new = self.flatten(&c.tree)?;
        let old = match c.parents.first() {
            Some(p) => self.commit_flat(p)?,
            None => Flat::new(),
        };
        Ok((c, detect_renames(diff_flat(&old, &new))))
    }

    pub fn file_at(&self, rev: &str, path: &str) -> Result<Vec<u8>> {
        let id = self.resolve_rev(rev)?;
        let flat = self.commit_flat(&id)?;
        let e: &FlatEntry = flat
            .get(path)
            .ok_or_else(|| Error::Other(format!("'{path}' not in {rev}")))?;
        self.read_file_bytes(e)
    }

    /// Absolute path of the project root.
    pub fn project_dir(&self) -> &Path {
        &self.root
    }
}
