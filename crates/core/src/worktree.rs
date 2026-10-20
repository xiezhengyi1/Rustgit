//! Working-directory scanning, portable path rules, and safe checkout.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{params, OptionalExtension};
use rustgit_store::ObjectId;
use unicode_normalization::UnicodeNormalization;

use crate::error::{Error, Result};
use crate::objects::{validate_name, EntryKind, RESERVED_ROOT_NAME};
use crate::repo::{Flat, FlatEntry, Repo, RGIT_DIR};

#[derive(Clone, Debug)]
pub struct WorkFile {
    pub is_symlink: bool,
    pub size: u64,
    pub exec: bool,
    pub mtime_ns: i64,
    pub ino: u64,
    /// Symlink target bytes (symlinks only).
    pub target: Option<Box<[u8]>>,
}

#[derive(Default, Debug)]
pub struct Scan {
    pub files: BTreeMap<String, WorkFile>,
    /// Paths that cannot be tracked, with the reason.
    pub skipped: Vec<(String, String)>,
}

pub const THEIRS_SUFFIX: &str = ".rgit-theirs";

#[cfg(unix)]
fn is_exec(m: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn is_exec(_: &fs::Metadata) -> bool {
    false
}
#[cfg(unix)]
fn inode(m: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(m)
}
#[cfg(not(unix))]
fn inode(_: &fs::Metadata) -> u64 {
    0
}

fn mtime_ns(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn link_bytes(p: &Path) -> std::io::Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Ok(fs::read_link(p)?.as_os_str().as_bytes().to_vec())
}
#[cfg(not(unix))]
fn link_bytes(p: &Path) -> std::io::Result<Vec<u8>> {
    Ok(fs::read_link(p)?
        .to_string_lossy()
        .into_owned()
        .into_bytes())
}

const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Everything that makes a set of paths unrepresentable on Windows, macOS or Linux.
pub fn portability_problems<'a, I>(paths: I) -> Vec<String>
where
    I: Iterator<Item = &'a str> + Clone,
{
    use std::hash::{Hash, Hasher};
    fn h64(s: &str) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    }
    /// Hash of the case- and Unicode-form-insensitive spelling of `s`.
    fn fold_hash(s: &str, scratch: &mut String) -> u64 {
        scratch.clear();
        if s.is_ascii() {
            scratch.extend(s.chars().map(|c| c.to_ascii_lowercase()));
        } else {
            scratch.extend(s.nfd().flat_map(|c| c.to_lowercase()));
        }
        h64(scratch)
    }

    let mut problems = Vec::new();
    // Only hashes are kept (fold hash -> spelling hash): memory stays a few bytes per path
    // instead of two strings per path component.
    let mut folded: HashMap<u64, u64> = HashMap::new();
    let mut suspects: HashSet<u64> = HashSet::new();
    let mut reported: HashSet<String> = HashSet::new();
    let mut prefix = String::new();
    let mut scratch = String::new();
    for path in paths.clone() {
        if path.len() > 240 {
            problems.push(format!("{path}: path longer than 240 bytes"));
        }
        prefix.clear();
        for comp in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(comp);
            let stem = comp.split('.').next().unwrap_or("");
            if stem.len() <= 4
                && WINDOWS_RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem))
                && reported.insert(format!("r:{prefix}"))
            {
                problems.push(format!("{prefix}: reserved device name on Windows"));
            }
            if (comp.ends_with('.') || comp.ends_with(' '))
                && reported.insert(format!("t:{prefix}"))
            {
                problems.push(format!("{prefix}: name ends with a dot or space"));
            }
            if comp
                .chars()
                .any(|c| "<>:\"\\|?*".contains(c) || (c as u32) < 0x20)
                && reported.insert(format!("c:{prefix}"))
            {
                problems.push(format!(
                    "{prefix}: contains a character not allowed on Windows"
                ));
            }
            let fh = fold_hash(&prefix, &mut scratch);
            let oh = h64(&prefix);
            match folded.get(&fh) {
                Some(&o) if o != oh => {
                    suspects.insert(fh);
                }
                Some(_) => {}
                None => {
                    folded.insert(fh, oh);
                }
            }
        }
    }
    if !suspects.is_empty() {
        // Resolve the (rare) suspects to real spellings; a hash collision that is not a real
        // collision simply reports nothing.
        let mut spellings: HashMap<u64, Vec<String>> = HashMap::new();
        for path in paths {
            prefix.clear();
            for comp in path.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(comp);
                let fh = fold_hash(&prefix, &mut scratch);
                if suspects.contains(&fh) {
                    let v = spellings.entry(fh).or_default();
                    if !v.contains(&prefix) {
                        v.push(prefix.clone());
                    }
                }
            }
        }
        let mut found: Vec<&Vec<String>> = spellings.values().filter(|v| v.len() > 1).collect();
        found.sort();
        for v in found {
            problems.push(format!(
                "{} and {} differ only by case or Unicode form",
                v[0], v[1]
            ));
        }
    }
    problems
}

/// A symlink target is only safe if it stays inside the project: relative, no `..`.
pub fn symlink_target_problem(target: &[u8]) -> Option<String> {
    let t = String::from_utf8_lossy(target);
    if t.is_empty() {
        return Some("empty symlink target".into());
    }
    if t.starts_with('/') || t.starts_with('\\') || t.contains(':') {
        return Some(format!("absolute symlink target {t:?}"));
    }
    if t.split(['/', '\\']).any(|c| c == "..") {
        return Some(format!("symlink target {t:?} leaves the project"));
    }
    None
}

#[derive(Clone, Debug)]
pub enum Op {
    Write { path: String, entry: FlatEntry },
    Delete { path: String },
}

impl Repo {
    pub fn scan(&self) -> Result<Scan> {
        self.scan_with_tracked(&self.head_flat()?)
    }

    pub(crate) fn scan_with_tracked(&self, tracked: &Flat) -> Result<Scan> {
        let mut b = ignore::WalkBuilder::new(&self.root);
        b.standard_filters(false)
            .hidden(false)
            .follow_links(false)
            // Project-local rules only, even without a .git directory. At the same
            // directory, RustGit-specific rules override Git rules.
            .add_custom_ignore_filename(".gitignore")
            .add_custom_ignore_filename(".rgitignore")
            .sort_by_file_name(|a, b| a.cmp(b))
            .filter_entry(|e| {
                let n = e.file_name();
                !(n == RGIT_DIR || n == ".git")
            });
        let mut scan = Scan::default();
        'entries: for ent in b.build() {
            let ent = match ent {
                Ok(e) => e,
                Err(e) => {
                    scan.skipped.push((String::new(), e.to_string()));
                    continue;
                }
            };
            if ent.depth() == 0 {
                continue;
            }
            let Some(ft) = ent.file_type() else { continue };
            if ft.is_dir() {
                continue;
            }
            let rel = ent.path().strip_prefix(&self.root).unwrap_or(ent.path());
            let mut parts = Vec::new();
            for c in rel.components() {
                match c {
                    Component::Normal(s) => match s.to_str() {
                        Some(s) => parts.push(s.to_string()),
                        None => {
                            scan.skipped.push((
                                rel.to_string_lossy().into_owned(),
                                "file name is not valid UTF-8".into(),
                            ));
                            continue 'entries;
                        }
                    },
                    _ => continue 'entries,
                }
            }
            for p in &parts {
                if let Err(e) = validate_name(p) {
                    scan.skipped.push((parts.join("/"), e.to_string()));
                    continue 'entries;
                }
            }
            let path = parts.join("/");
            if path == RESERVED_ROOT_NAME {
                continue;
            }
            let meta = match ent.metadata() {
                Ok(m) => m,
                Err(e) => {
                    scan.skipped.push((path, e.to_string()));
                    continue;
                }
            };
            Self::add_work_file(&mut scan, path, ent.path(), &meta);
        }
        self.include_work_files(&mut scan, tracked);
        Ok(scan)
    }

    pub(crate) fn include_work_files(&self, scan: &mut Scan, tracked: &Flat) {
        // Ignore rules govern discovery of untracked files. A previously committed
        // file still needs modification/deletion detection, including in ignored dirs.
        for path in tracked.keys() {
            if scan.files.contains_key(path) || path.split('/').any(|p| p == RGIT_DIR || p == ".git") {
                continue;
            }
            if self.check_no_symlink_ancestors(path).is_err() {
                continue;
            }
            let absolute = self.root.join(path);
            match fs::symlink_metadata(&absolute) {
                Ok(meta) => Self::add_work_file(scan, path.clone(), &absolute, &meta),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                Err(e) => scan.skipped.push((path.clone(), e.to_string())),
            }
        }
    }

    fn add_work_file(scan: &mut Scan, path: String, absolute: &Path, meta: &fs::Metadata) {
        if meta.file_type().is_symlink() {
            match link_bytes(absolute) {
                Ok(target) => {
                    scan.files.insert(path, WorkFile {
                        is_symlink: true, size: target.len() as u64, exec: false,
                        mtime_ns: 0, ino: 0, target: Some(target.into_boxed_slice()),
                    });
                }
                Err(e) => scan.skipped.push((path, e.to_string())),
            }
        } else if meta.is_file() {
            scan.files.insert(path, WorkFile {
                is_symlink: false, size: meta.len(), exec: is_exec(meta),
                mtime_ns: mtime_ns(meta), ino: inode(meta), target: None,
            });
        } else {
            scan.skipped.push((path, "not a regular file or symlink".into()));
        }
    }

    fn now_ns() -> i64 {
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0)
    }

    fn cache_get(&self, path: &str, f: &WorkFile) -> Option<(EntryKind, ObjectId, bool)> {
        let row = self
            .local
            .query_row(
                "SELECT size,mtime_ns,ino,kind,id,cached_ns,stored FROM statcache WHERE path=?",
                [path],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, u8>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()
            .ok()??;
        let (size, mt, ino, kind, id, cached, stored) = row;
                // Racy-clean rule: only trust the cache if the file was last modified well
        // before the cache entry was written.
        if size as u64 != f.size
            || mt != f.mtime_ns
            || ino as u64 != f.ino
            || f.mtime_ns.saturating_add(2_000_000_000) > cached
        {
            return None;
        }
        let kind = if kind == 1 {
            EntryKind::File
        } else {
            EntryKind::ChunkedFile
        };
        Some((kind, ObjectId::from_slice(&id).ok()?, stored != 0))
    }

    fn cache_put(&self, path: &str, f: &WorkFile, kind: EntryKind, id: &ObjectId, stored: bool) {
        let _ = self.local.execute(
            "INSERT OR REPLACE INTO statcache(path,size,mtime_ns,ino,kind,id,cached_ns,stored) VALUES(?,?,?,?,?,?,?,?)",
            params![
                path,
                f.size as i64,
                f.mtime_ns,
                f.ino as i64,
                kind as u8,
                &id.0[..],
                Self::now_ns(),
                stored as i64
            ],
        );
    }

    /// Hashes (and, with `store_it`, stores) the working files selected by `only`
    /// (all files when `None`), reusing `prev` entries as delta hints.
    pub fn work_flat(
        &mut self,
        scan: &Scan,
        store_it: bool,
        prev: &Flat,
        only: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<Flat> {
        let mut out = Flat::new();
        let _ = self.local.execute_batch("BEGIN");
        let res = (|| -> Result<()> {
            for (path, f) in &scan.files {
                if let Some(p) = only {
                    if !p(path) {
                        continue;
                    }
                }
                if f.is_symlink {
                    let t: &[u8] = f.target.as_deref().unwrap_or_default();
                    let id = self.ingest_symlink(path, t, store_it)?;
                    out.insert(
                        path.clone(),
                        FlatEntry {
                            kind: EntryKind::Symlink,
                            exec: false,
                            id,
                        },
                    );
                    continue;
                }
                if let Some((kind, id, stored)) = self.cache_get(path, f) {
                    if !store_it || (stored && self.store.has(&id)?) {
                        out.insert(
                            path.clone(),
                            FlatEntry {
                                kind,
                                exec: f.exec,
                                id,
                            },
                        );
                        continue;
                    }
                }
                let prev_entry = self.hint_entry(prev, path);
                let (kind, id) = self.ingest_path(path, store_it, prev_entry.as_ref())?;
                self.cache_put(path, f, kind, &id, store_it);
                out.insert(
                    path.clone(),
                    FlatEntry {
                        kind,
                        exec: f.exec,
                        id,
                    },
                );
            }
            Ok(())
        })();
        let _ = self.local.execute_batch("COMMIT");
        res?;
        Ok(out)
    }

    // ----- applying changes to the working directory ----------------------------------

    fn check_no_symlink_ancestors(&self, path: &str) -> Result<()> {
        let mut cur = self.root.clone();
        let comps: Vec<&str> = path.split('/').collect();
        for c in &comps[..comps.len() - 1] {
            cur.push(c);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(Error::Invalid(format!(
                        "refusing to write through symlink at {}",
                        cur.display()
                    )));
                }
                Ok(m) if !m.is_dir() => {
                    return Err(Error::Invalid(format!(
                        "{} is a file, not a directory",
                        cur.display()
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn write_entry(&self, path: &str, e: &FlatEntry) -> Result<()> {
        self.check_no_symlink_ancestors(path)?;
        let abs: PathBuf = self.root.join(path);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::symlink_metadata(&abs) {
            Ok(m) if m.is_dir() => {
                return Err(Error::Invalid(format!(
                    "{path} is a directory in the working tree"
                )));
            }
            _ => {}
        }
        if e.kind == EntryKind::Symlink {
            let target = self.store.get(&e.id)?.1;
            if let Some(p) = symlink_target_problem(&target) {
                return Err(Error::Unportable(vec![format!("{path}: {p}")]));
            }
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                let _ = fs::remove_file(&abs);
                std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), &abs)?;
                return Ok(());
            }
            #[cfg(not(unix))]
            return Err(Error::Unportable(vec![format!(
                "{path}: symlinks are not supported on this platform"
            )]));
        }
        let tmp = abs.with_file_name(format!(".rgit-tmp-{}", uuid::Uuid::new_v4().simple()));
        {
            let mut f = fs::File::create(&tmp)?;
            let mut w = std::io::BufWriter::new(&mut f);
            let r = self.write_file_to(e, &mut w);
            let r = r.and_then(|_| w.flush().map_err(Error::from));
            drop(w);
            if let Err(err) = r {
                let _ = fs::remove_file(&tmp);
                return Err(err);
            }
            // No fsync: working files can always be recreated from the repository.
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if e.exec { 0o755 } else { 0o644 };
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&tmp, &abs).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    fn remove_path(&self, path: &str) -> Result<()> {
        self.check_no_symlink_ancestors(path)?;
        let abs = self.root.join(path);
        match fs::symlink_metadata(&abs) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => fs::remove_file(&abs)?,
            Err(_) => {}
        }
        // Remove directories that became empty.
        let mut p = abs.parent().map(Path::to_path_buf);
        while let Some(d) = p {
            if d == self.root || fs::remove_dir(&d).is_err() {
                break;
            }
            p = d.parent().map(Path::to_path_buf);
        }
        Ok(())
    }

    /// Applies planned operations: all deletions first, then writes.
    pub fn apply_ops(&self, ops: &[Op]) -> Result<()> {
        for op in ops {
            if let Op::Delete { path } = op {
                self.remove_path(path)?;
            }
        }
        for op in ops {
            if let Op::Write { path, entry } = op {
                self.write_entry(path, entry)?;
            }
        }
        Ok(())
    }

    /// Plans operations turning `from` into `to`.
    pub fn plan_ops(from: &Flat, to: &Flat) -> Vec<Op> {
        let mut ops = Vec::new();
        for p in from.keys() {
            if !to.contains_key(p) {
                ops.push(Op::Delete { path: p.clone() });
            }
        }
        for (p, e) in to {
            if from.get(p) != Some(e) {
                ops.push(Op::Write {
                    path: p.clone(),
                    entry: e.clone(),
                });
            }
        }
        ops
    }

    /// Moves the working directory from the committed state `current` to `target`.
    ///
    /// Without `force`, refuses (touching nothing) if the operation would overwrite or
    /// delete uncommitted work, an untracked file, or an ignored file. Always refuses
    /// targets that cannot be represented portably.
    pub fn checkout_flat(&mut self, current: &Flat, target: &Flat, force: bool) -> Result<()> {
        let problems = portability_problems(target.keys().map(String::as_str));
        if !problems.is_empty() {
            return Err(Error::Unportable(problems));
        }
        for (p, e) in target {
            if e.kind == EntryKind::Symlink {
                let t = self.store.get(&e.id)?.1;
                if let Some(pr) = symlink_target_problem(&t) {
                    return Err(Error::Unportable(vec![format!("{p}: {pr}")]));
                }
                #[cfg(not(unix))]
                return Err(Error::Unportable(vec![format!(
                    "{p}: symlinks are not supported on this platform"
                )]));
            }
        }
        let ops = Self::plan_ops(current, target);
        if !force {
            let scan = self.scan_with_tracked(current)?;
            let touched: HashSet<String> = ops
                .iter()
                .map(|o| match o {
                    Op::Write { path, .. } | Op::Delete { path } => path.clone(),
                })
                .collect();
            let work =
                self.work_flat(&scan, false, current, Some(&|p: &str| touched.contains(p)))?;
            let mut dirty: Vec<String> = Vec::new();
            for op in &ops {
                match op {
                    Op::Delete { path } => {
                        if work.get(path) != current.get(path)
                            && (work.contains_key(path) || scan.files.contains_key(path))
                        {
                            dirty.push(path.clone());
                        }
                    }
                    Op::Write { path, entry } => {
                        let w = work.get(path);
                        if w == Some(entry) {
                            continue;
                        }
                        if w != current.get(path) {
                            dirty.push(path.clone());
                            continue;
                        }
                        if w.is_none() && fs::symlink_metadata(self.root.join(path)).is_ok() {
                            // an ignored/untracked file sits where we want to write
                            dirty.push(path.clone());
                            continue;
                        }
                        // untracked file in the way of a new directory
                        let comps: Vec<&str> = path.split('/').collect();
                        let mut pre = String::new();
                        for c in &comps[..comps.len() - 1] {
                            if !pre.is_empty() {
                                pre.push('/');
                            }
                            pre.push_str(c);
                            if scan.files.contains_key(&pre) && !current.contains_key(&pre) {
                                dirty.push(pre.clone());
                            }
                        }
                    }
                }
            }
            dirty.sort();
            dirty.dedup();
            if !dirty.is_empty() {
                return Err(Error::DirtyWorkingTree(dirty));
            }
        }
        self.apply_ops(&ops)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::portability_problems;

    #[test]
    fn detects_case_and_unicode_form_collisions_with_hashed_keys() {
        let p = portability_problems(["src/Readme.md", "src/README.md", "ok.txt"].into_iter());
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("Readme.md") && p[0].contains("README.md"), "{p:?}");
        // é as one code point vs e + combining accent
        let p = portability_problems(["caf\u{e9}.txt", "cafe\u{301}.txt"].into_iter());
        assert_eq!(p.len(), 1, "{p:?}");
        // a directory that differs only by case collides even if the files differ
        let p = portability_problems(["Dir/a.txt", "dir/b.txt"].into_iter());
        assert!(p.iter().any(|m| m.contains("Dir") && m.contains("dir")), "{p:?}");
    }

    #[test]
    fn clean_trees_and_windows_rules() {
        assert!(portability_problems(["a/b.txt", "a/c.txt", "d/e.txt"].into_iter()).is_empty());
        let p = portability_problems(["docs/aux.txt", "x/y. ", "bad/a:b"].into_iter());
        assert_eq!(p.len(), 3, "{p:?}");
        // the same directory repeated for many files is not a collision
        let many: Vec<String> = (0..1000).map(|i| format!("same/dir/f{i}")).collect();
        assert!(portability_problems(many.iter().map(String::as_str)).is_empty());
    }
}
