//! Repository handle: layout, metadata database, refs, config, object access.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use rustgit_store::chunker::{self, ChunkerConfig};
use rustgit_store::{hash_object, ObjectId, ObjectType, Store, StoreOptions};

use crate::error::{Error, Result};
use crate::objects::*;

pub const RGIT_DIR: &str = ".rgit";
pub const SUPPORTED_FORMAT: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatEntry {
    pub kind: EntryKind,
    pub exec: bool,
    pub id: ObjectId,
}

pub type Flat = BTreeMap<String, FlatEntry>;

#[derive(Clone, Copy, Debug)]
pub struct RepoConfig {
    pub chunker: ChunkerConfig,
    pub small_file_max: u64,
}

impl Default for RepoConfig {
    fn default() -> Self {
        RepoConfig {
            chunker: ChunkerConfig::default(),
            small_file_max: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefValue {
    pub commit: Option<ObjectId>,
    pub generation: u64,
}

pub struct Repo {
    pub root: PathBuf,
    pub store: Store,
    pub(crate) meta: Connection,
    pub(crate) local: Connection,
    pub cfg: RepoConfig,
}

pub fn validate_ref_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("//")
        && !name.contains("..")
        && !name.ends_with(".lock")
        && name
            .split('/')
            .all(|p| !p.is_empty() && !p.starts_with('.'))
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidName(name.to_string()))
    }
}

fn encode_flat(e: &Option<FlatEntry>) -> Option<Vec<u8>> {
    e.as_ref().map(|e| {
        let mut v = vec![e.kind as u8, e.exec as u8];
        v.extend_from_slice(&e.id.0);
        v
    })
}

fn decode_flat(b: Option<Vec<u8>>) -> Result<Option<FlatEntry>> {
    let Some(b) = b else { return Ok(None) };
    if b.len() != 34 {
        return Err(Error::Invalid("bad stored entry".into()));
    }
    let kind = match b[0] {
        1 => EntryKind::File,
        2 => EntryKind::ChunkedFile,
        3 => EntryKind::Dir,
        4 => EntryKind::Symlink,
        _ => return Err(Error::Invalid("bad stored entry kind".into())),
    };
    Ok(Some(FlatEntry {
        kind,
        exec: b[1] == 1,
        id: ObjectId::from_slice(&b[2..])?,
    }))
}

#[derive(Clone, Debug)]
pub struct Conflict {
    pub path: String,
    pub kind: String,
    pub base: Option<FlatEntry>,
    pub ours: Option<FlatEntry>,
    pub theirs: Option<FlatEntry>,
    pub resolved: bool,
}

#[derive(Clone, Debug)]
pub struct MergeState {
    pub kind: String,
    pub label: String,
    pub ours: ObjectId,
    pub extra_parents: Vec<ObjectId>,
    pub message: String,
}

impl Repo {
    // ----- creation / opening ---------------------------------------------------------

    pub fn init(root: &Path, default_branch: &str) -> Result<Repo> {
        validate_ref_name(default_branch)?;
        let dir = root.join(RGIT_DIR);
        if dir.join("FORMAT").exists() {
            return Err(Error::AlreadyExists(root.to_path_buf()));
        }
        fs::create_dir_all(&dir)?;
        let uuid = uuid::Uuid::new_v4().to_string();
        fs::write(
            dir.join("FORMAT"),
            format!("{{\"format\":{SUPPORTED_FORMAT},\"uuid\":\"{uuid}\"}}\n"),
        )?;
        let repo = Self::open_dir(root)?;
        repo.set_config("repo.uuid", &uuid)?;
        repo.set_config("head", default_branch)?;
        repo.set_config("chunk.version", "1")?;
        Ok(repo)
    }

    pub fn open(root: &Path) -> Result<Repo> {
        if !root.join(RGIT_DIR).join("FORMAT").exists() {
            return Err(Error::NotARepository(root.to_path_buf()));
        }
        Self::open_dir(root)
    }

    /// Finds the project containing `start` by walking up the directory tree.
    pub fn discover(start: &Path) -> Result<Repo> {
        let start = fs::canonicalize(start)?;
        let mut cur: &Path = &start;
        loop {
            if cur.join(RGIT_DIR).join("FORMAT").exists() {
                return Self::open_dir(cur);
            }
            match cur.parent() {
                Some(p) => cur = p,
                None => return Err(Error::NotARepository(start.clone())),
            }
        }
    }

    fn open_dir(root: &Path) -> Result<Repo> {
        let dir = root.join(RGIT_DIR);
        let fmt = fs::read_to_string(dir.join("FORMAT"))?;
        let version = fmt
            .split("\"format\":")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| Error::Invalid("unreadable FORMAT file".into()))?;
        if version > SUPPORTED_FORMAT {
            return Err(Error::Invalid(format!(
                "repository format {version} is newer than this build supports ({SUPPORTED_FORMAT})"
            )));
        }
        let store = Store::open(&dir, StoreOptions::default())?;
        let meta = Connection::open(dir.join("meta.sqlite"))?;
        meta.execute_batch(
            "PRAGMA page_size=1024; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS config(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS refs(name TEXT PRIMARY KEY, commit_id BLOB, gen INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS applied_updates(
                 key BLOB PRIMARY KEY, ref_name TEXT NOT NULL, commit_id BLOB, gen INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS merge_state(
                 id INTEGER PRIMARY KEY CHECK(id=1), kind TEXT NOT NULL, label TEXT NOT NULL,
                 ours BLOB NOT NULL, extra_parents BLOB NOT NULL, message TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS conflicts(
                 path TEXT PRIMARY KEY, kind TEXT NOT NULL, base BLOB, ours BLOB, theirs BLOB,
                 resolved INTEGER NOT NULL DEFAULT 0);",
        )?;
        let local = Connection::open(dir.join("local.sqlite"))?;
        local.execute_batch(
            "PRAGMA page_size=1024; PRAGMA journal_mode=DELETE; PRAGMA synchronous=OFF;
             CREATE TABLE IF NOT EXISTS statcache(
                 path TEXT PRIMARY KEY, size INTEGER, mtime_ns INTEGER, ino INTEGER,
                 kind INTEGER, id BLOB, cached_ns INTEGER, stored INTEGER) WITHOUT ROWID;",
        )?;
        let mut repo = Repo {
            root: root.to_path_buf(),
            store,
            meta,
            local,
            cfg: RepoConfig::default(),
        };
        repo.load_config()?;
        Ok(repo)
    }

    fn load_config(&mut self) -> Result<()> {
        let mut c = RepoConfig::default();
        let geti = |s: &Repo, k: &str| -> Result<Option<usize>> {
            Ok(s.get_config(k)?.and_then(|v| v.parse().ok()))
        };
        if let Some(v) = geti(self, "chunk.min")? {
            c.chunker.min = v;
        }
        if let Some(v) = geti(self, "chunk.avg")? {
            c.chunker.avg = v;
        }
        if let Some(v) = geti(self, "chunk.max")? {
            c.chunker.max = v;
        }
        if let Some(v) = geti(self, "small_file_max")? {
            c.small_file_max = v as u64;
        }
        c.chunker.validate()?;
        if let Some(d) = geti(self, "delta.max_depth")? {
            self.store.set_max_delta_depth(d as u32);
        }
        self.cfg = c;
        Ok(())
    }

    // ----- config ---------------------------------------------------------------------

    pub fn get_config(&self, k: &str) -> Result<Option<String>> {
        Ok(self
            .meta
            .query_row("SELECT v FROM config WHERE k=?", [k], |r| r.get(0))
            .optional()?)
    }

    /// All config entries whose key starts with `prefix`, sorted by key.
    pub fn config_with_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let mut st = self
            .meta
            .prepare("SELECT k, v FROM config WHERE substr(k, 1, ?) = ? ORDER BY k")?;
        let rows = st.query_map(params![prefix.len() as i64, prefix], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn set_config(&self, k: &str, v: &str) -> Result<()> {
        self.meta.execute(
            "INSERT OR REPLACE INTO config(k,v) VALUES(?,?)",
            params![k, v],
        )?;
        Ok(())
    }

    pub fn set_user_config(&mut self, k: &str, v: &str) -> Result<()> {
        self.set_config(k, v)?;
        if k.starts_with("chunk.") || k.starts_with("delta.") || k == "small_file_max" {
            self.load_config()?;
        }
        Ok(())
    }

    pub fn uuid(&self) -> Result<String> {
        Ok(self.get_config("repo.uuid")?.unwrap_or_default())
    }

    pub fn author(&self) -> Result<(String, String)> {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".into());
        let name = self
            .get_config("user.name")?
            .unwrap_or_else(|| user.clone());
        let email = self
            .get_config("user.email")?
            .unwrap_or_else(|| format!("{user}@localhost"));
        Ok((name, email))
    }

    // ----- refs -----------------------------------------------------------------------

    pub fn get_ref(&self, name: &str) -> Result<Option<RefValue>> {
        Ok(self
            .meta
            .query_row(
                "SELECT commit_id, gen FROM refs WHERE name=?",
                [name],
                |r| Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?
            .map(|(c, g)| -> Result<RefValue> {
                Ok(RefValue {
                    commit: c.map(|b| ObjectId::from_slice(&b)).transpose()?,
                    generation: g as u64,
                })
            })
            .transpose()?)
    }

    /// Compare-and-swap on a ref. `expected` is the commit the caller last saw (None =
    /// absent or deleted). `new = None` deletes. Returns the new generation.
    pub fn cas_ref(
        &self,
        name: &str,
        expected: Option<ObjectId>,
        new: Option<ObjectId>,
    ) -> Result<u64> {
        self.cas_ref_checked(name, expected, None, new, None)
    }

    /// Compare-and-swap that can also require the generation the caller saw and can record
    /// an idempotency key in the same transaction (used by the hosting layer: after a crash
    /// the key tells a retried request that its update was already applied).
    pub fn cas_ref_checked(
        &self,
        name: &str,
        expected: Option<ObjectId>,
        expected_gen: Option<u64>,
        new: Option<ObjectId>,
        applied_key: Option<&[u8]>,
    ) -> Result<u64> {
        validate_ref_name(name)?;
        let tx = self.meta.unchecked_transaction()?;
        let cur = self.get_ref(name)?;
        let (cur_commit, gen) = match &cur {
            Some(v) => (v.commit, v.generation),
            None => (None, 0),
        };
        if cur_commit != expected || expected_gen.is_some_and(|g| g != gen) {
            return Err(Error::RefConflict {
                name: name.to_string(),
            });
        }
        let new_gen = gen + 1;
        tx.execute(
            "INSERT OR REPLACE INTO refs(name, commit_id, gen) VALUES(?,?,?)",
            params![name, new.as_ref().map(|c| c.0.to_vec()), new_gen as i64],
        )?;
        if let Some(k) = applied_key {
            tx.execute(
                "INSERT OR REPLACE INTO applied_updates(key, ref_name, commit_id, gen) VALUES(?,?,?,?)",
                params![k, name, new.as_ref().map(|c| c.0.to_vec()), new_gen as i64],
            )?;
            // Keep the idempotency log bounded: the newest 100k keys are plenty for retries.
            tx.execute(
                "DELETE FROM applied_updates WHERE rowid <= (SELECT MAX(rowid) FROM applied_updates) - 100000",
                [],
            )?;
        }
        tx.commit()?;
        Ok(new_gen)
    }

    /// The result recorded for an idempotency key by `cas_ref_checked`, if any.
    pub fn applied_update(&self, key: &[u8]) -> Result<Option<(String, Option<ObjectId>, u64)>> {
        let row = self
            .meta
            .query_row(
                "SELECT ref_name, commit_id, gen FROM applied_updates WHERE key=?",
                [key],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<Vec<u8>>>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some((n, c, g)) => Ok(Some((
                n,
                c.map(|b| ObjectId::from_slice(&b)).transpose()?,
                g as u64,
            ))),
        }
    }

    /// Every ref record under `prefix`, including deletion tombstones (commit = None).
    /// Names are returned in full, sorted.
    pub fn ref_records(&self, prefix: &str) -> Result<Vec<(String, RefValue)>> {
        let mut st = self
            .meta
            .prepare("SELECT name, commit_id, gen FROM refs WHERE name LIKE ? ESCAPE '\\' ORDER BY name")?;
        let pat = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = st.query_map([pat], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<Vec<u8>>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (n, c, g) = r?;
            out.push((
                n,
                RefValue {
                    commit: c.map(|b| ObjectId::from_slice(&b)).transpose()?,
                    generation: g as u64,
                },
            ));
        }
        Ok(out)
    }

    /// Lists refs under `prefix` (for example "heads/") that are not deleted.
    pub fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>> {
        let mut st = self
            .meta
            .prepare("SELECT name, commit_id FROM refs WHERE name LIKE ? ESCAPE '\\' AND commit_id IS NOT NULL ORDER BY name")?;
        let pat = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = st.query_map([pat], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (n, c) = r?;
            out.push((n[prefix.len()..].to_string(), ObjectId::from_slice(&c)?));
        }
        Ok(out)
    }

    pub fn current_branch(&self) -> Result<String> {
        Ok(self.get_config("head")?.unwrap_or_else(|| "main".into()))
    }

    pub fn head_commit(&self) -> Result<Option<ObjectId>> {
        Ok(self
            .get_ref(&format!("heads/{}", self.current_branch()?))?
            .and_then(|r| r.commit))
    }

    pub fn branches(&self) -> Result<Vec<(String, ObjectId)>> {
        self.list_refs("heads/")
    }

    // ----- merge state ----------------------------------------------------------------

    pub fn merge_state(&self) -> Result<Option<MergeState>> {
        let r = self
            .meta
            .query_row(
                "SELECT kind,label,ours,extra_parents,message FROM merge_state WHERE id=1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        match r {
            None => Ok(None),
            Some((kind, label, ours, extra, message)) => {
                let extra_parents = extra
                    .chunks(32)
                    .map(ObjectId::from_slice)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(Some(MergeState {
                    kind,
                    label,
                    ours: ObjectId::from_slice(&ours)?,
                    extra_parents,
                    message,
                }))
            }
        }
    }

    pub(crate) fn set_merge_state(&self, st: &MergeState, conflicts: &[Conflict]) -> Result<()> {
        let tx = self.meta.unchecked_transaction()?;
        tx.execute("DELETE FROM merge_state", [])?;
        tx.execute("DELETE FROM conflicts", [])?;
        let extra: Vec<u8> = st.extra_parents.iter().flat_map(|p| p.0).collect();
        tx.execute(
            "INSERT INTO merge_state(id,kind,label,ours,extra_parents,message) VALUES(1,?,?,?,?,?)",
            params![st.kind, st.label, &st.ours.0[..], extra, st.message],
        )?;
        for c in conflicts {
            tx.execute(
                "INSERT INTO conflicts(path,kind,base,ours,theirs,resolved) VALUES(?,?,?,?,?,0)",
                params![
                    c.path,
                    c.kind,
                    encode_flat(&c.base),
                    encode_flat(&c.ours),
                    encode_flat(&c.theirs)
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn clear_merge_state(&self) -> Result<()> {
        let tx = self.meta.unchecked_transaction()?;
        tx.execute("DELETE FROM merge_state", [])?;
        tx.execute("DELETE FROM conflicts", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn conflicts(&self) -> Result<Vec<Conflict>> {
        let mut st = self
            .meta
            .prepare("SELECT path,kind,base,ours,theirs,resolved FROM conflicts ORDER BY path")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
                r.get::<_, Option<Vec<u8>>>(3)?,
                r.get::<_, Option<Vec<u8>>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (path, kind, b, o, t, res) = r?;
            out.push(Conflict {
                path,
                kind,
                base: decode_flat(b)?,
                ours: decode_flat(o)?,
                theirs: decode_flat(t)?,
                resolved: res != 0,
            });
        }
        Ok(out)
    }

    pub(crate) fn mark_resolved(&self, path: &str) -> Result<()> {
        self.meta
            .execute("UPDATE conflicts SET resolved=1 WHERE path=?", [path])?;
        Ok(())
    }

    // ----- object access --------------------------------------------------------------

    pub fn read_commit(&self, id: &ObjectId) -> Result<Commit> {
        let (ty, data) = self.store.get(id)?;
        if ty != ObjectType::Commit {
            return Err(Error::Invalid(format!("{id} is not a commit")));
        }
        Commit::decode(&data)
    }

    pub fn read_tree(&self, id: &ObjectId) -> Result<Tree> {
        let (ty, data) = self.store.get(id)?;
        if ty != ObjectType::Tree {
            return Err(Error::Invalid(format!("{id} is not a tree")));
        }
        Tree::decode(&data)
    }

    pub fn read_manifest(&self, id: &ObjectId) -> Result<Manifest> {
        let (ty, data) = self.store.get(id)?;
        if ty != ObjectType::Manifest {
            return Err(Error::Invalid(format!("{id} is not a manifest")));
        }
        Manifest::decode(&data)
    }

    pub fn flatten(&self, tree: &ObjectId) -> Result<Flat> {
        let mut out = Flat::new();
        self.flatten_into(tree, "", 0, &mut out)?;
        Ok(out)
    }

    fn flatten_into(
        &self,
        tree: &ObjectId,
        prefix: &str,
        depth: usize,
        out: &mut Flat,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(Error::Invalid("tree nesting too deep".into()));
        }
        let t = self.read_tree(tree)?;
        for e in &t.entries {
            if depth == 0 && e.name == RESERVED_ROOT_NAME {
                return Err(Error::Invalid(format!(
                    "reserved name {RESERVED_ROOT_NAME} at root"
                )));
            }
            let path = if prefix.is_empty() {
                e.name.clone()
            } else {
                format!("{prefix}/{}", e.name)
            };
            match e.kind {
                EntryKind::Dir => self.flatten_into(&e.id, &path, depth + 1, out)?,
                kind => {
                    out.insert(
                        path,
                        FlatEntry {
                            kind,
                            exec: e.exec,
                            id: e.id,
                        },
                    );
                }
            }
        }
        Ok(())
    }

    pub fn commit_flat(&self, commit: &ObjectId) -> Result<Flat> {
        let c = self.read_commit(commit)?;
        self.flatten(&c.tree)
    }

    pub fn head_flat(&self) -> Result<Flat> {
        match self.head_commit()? {
            Some(c) => self.commit_flat(&c),
            None => Ok(Flat::new()),
        }
    }

    /// Writes trees for `flat`, using `old_root` as the delta hint source.
    pub fn write_tree(&mut self, flat: &Flat, old_root: Option<&ObjectId>) -> Result<ObjectId> {
        let items: Vec<(&str, &FlatEntry)> = flat.iter().map(|(k, v)| (k.as_str(), v)).collect();
        self.write_tree_rec(&items, 0, old_root.copied(), 0)
    }

    fn write_tree_rec(
        &mut self,
        items: &[(&str, &FlatEntry)],
        skip: usize,
        old: Option<ObjectId>,
        depth: usize,
    ) -> Result<ObjectId> {
        if depth > MAX_DEPTH {
            return Err(Error::Invalid("tree nesting too deep".into()));
        }
        let old_tree = match old {
            Some(id) => self.read_tree(&id).ok(),
            None => None,
        };
        let mut entries: Vec<TreeEntry> = Vec::new();
        let mut i = 0;
        while i < items.len() {
            let rest = &items[i].0[skip..];
            match rest.find('/') {
                None => {
                    validate_name(rest)?;
                    if depth == 0 && rest == RESERVED_ROOT_NAME {
                        return Err(Error::Invalid(format!(
                            "{RESERVED_ROOT_NAME} cannot be tracked"
                        )));
                    }
                    let e = items[i].1;
                    entries.push(TreeEntry {
                        name: rest.to_string(),
                        kind: e.kind,
                        exec: e.exec,
                        id: e.id,
                    });
                    i += 1;
                }
                Some(pos) => {
                    let dir = &rest[..pos];
                    validate_name(dir)?;
                    let mut j = i;
                    while j < items.len()
                        && items[j].0[skip..].starts_with(dir)
                        && items[j].0[skip..].as_bytes().get(dir.len()) == Some(&b'/')
                    {
                        j += 1;
                    }
                    let old_child = old_tree
                        .as_ref()
                        .and_then(|t| t.find(dir))
                        .filter(|e| e.kind == EntryKind::Dir)
                        .map(|e| e.id);
                    let child = self.write_tree_rec(
                        &items[i..j],
                        skip + dir.len() + 1,
                        old_child,
                        depth + 1,
                    )?;
                    entries.push(TreeEntry {
                        name: dir.to_string(),
                        kind: EntryKind::Dir,
                        exec: false,
                        id: child,
                    });
                    i = j;
                }
            }
        }
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        for w in entries.windows(2) {
            if w[0].name == w[1].name {
                return Err(Error::Invalid(format!(
                    "path {:?} is both a file and a directory",
                    w[0].name
                )));
            }
        }
        let t = Tree { entries };
        let hint = old;
        Ok(self
            .store
            .put(ObjectType::Tree, &t.encode(), hint.as_ref())?)
    }

    // ----- file content ---------------------------------------------------------------

    /// Layout of an existing file version as `(start, end, chunk id)` for delta hints.
    /// Best earlier version to delta against: the same path if it existed, otherwise the
    /// neighbouring file (same directory and extension) in sorted order. Snapshots and
    /// copies of large files are usually named next to each other.
    pub fn hint_entry(&self, flat: &Flat, path: &str) -> Option<FlatEntry> {
        use std::ops::Bound::{Excluded, Unbounded};
        if let Some(e) = flat.get(path) {
            return Some(e.clone());
        }
        let (dir, name) = match path.rfind('/') {
            Some(i) => (&path[..=i], &path[i + 1..]),
            None => ("", path),
        };
        let ext = name.rsplit_once('.').map(|(_, e)| e);
        let ok = |k: &String, e: &FlatEntry| {
            e.kind.is_file()
                && k.starts_with(dir)
                && !k[dir.len()..].contains('/')
                && k[dir.len()..].rsplit_once('.').map(|(_, x)| x) == ext
        };
        let owned = path.to_string();
        if let Some((k, e)) = flat
            .range::<String, _>((Unbounded, Excluded(&owned)))
            .next_back()
        {
            if ok(k, e) {
                return Some(e.clone());
            }
        }
        if let Some((k, e)) = flat
            .range::<String, _>((Excluded(&owned), Unbounded))
            .next()
        {
            if ok(k, e) {
                return Some(e.clone());
            }
        }
        None
    }

    pub fn layout_pub(&self, e: &FlatEntry) -> Vec<(u64, u64, ObjectId)> {
        self.layout(e)
    }

    fn layout(&self, e: &FlatEntry) -> Vec<(u64, u64, ObjectId)> {
        match e.kind {
            EntryKind::File => match self.store.logical_len(&e.id) {
                Ok(Some(l)) => vec![(0, l, e.id)],
                _ => vec![],
            },
            EntryKind::ChunkedFile => {
                let Ok(m) = self.read_manifest(&e.id) else {
                    return vec![];
                };
                let mut v = Vec::with_capacity(m.chunks.len());
                let mut off = 0;
                for c in &m.chunks {
                    let l = self.store.logical_len(c).ok().flatten().unwrap_or(0);
                    v.push((off, off + l, *c));
                    off += l;
                }
                v
            }
            _ => vec![],
        }
    }

    /// Stores (or only hashes, when `store_it` is false) the file at `rel`.
    pub fn ingest_path(
        &mut self,
        rel: &str,
        store_it: bool,
        prev: Option<&FlatEntry>,
    ) -> Result<(EntryKind, ObjectId)> {
        let abs = self.root.join(rel);
        let cfg = self.cfg;
        let layout = prev.map(|p| self.layout(p)).unwrap_or_default();
        let hint_at = |off: u64| -> Option<ObjectId> {
            let i = layout.partition_point(|&(_, end, _)| end <= off);
            layout
                .get(i)
                .filter(|&&(s, _, _)| s <= off)
                .map(|&(_, _, id)| id)
        };
        let mut f = fs::File::open(&abs)?;
        let m1 = f.metadata()?;
        let size = m1.len();
        let mut result: (EntryKind, ObjectId);
        if size <= cfg.small_file_max {
            let mut buf = Vec::with_capacity(size as usize);
            (&mut f).take(size + 1).read_to_end(&mut buf)?;
            if buf.len() as u64 != size {
                return Err(Error::FileChanged(rel.to_string()));
            }
            let id = if store_it {
                self.store
                    .put(ObjectType::Chunk, &buf, hint_at(0).as_ref())?
            } else {
                hash_object(ObjectType::Chunk, &buf)
            };
            result = (EntryKind::File, id);
        } else {
            let mut chunks: Vec<ObjectId> = Vec::new();
            let mut buf: Vec<u8> = Vec::with_capacity(cfg.chunker.max * 2);
            let mut eof = false;
            let mut offset: u64 = 0;
            let mut total: u64 = 0;
            loop {
                while !eof && buf.len() < cfg.chunker.max {
                    let old = buf.len();
                    buf.resize(cfg.chunker.max, 0);
                    let n = f.read(&mut buf[old..])?;
                    buf.truncate(old + n);
                    if n == 0 {
                        eof = true;
                    }
                }
                if buf.is_empty() {
                    break;
                }
                let cut = chunker::first_cut(&buf, &cfg.chunker);
                let chunk = &buf[..cut];
                let id = if store_it {
                    self.store
                        .put(ObjectType::Chunk, chunk, hint_at(offset).as_ref())?
                } else {
                    hash_object(ObjectType::Chunk, chunk)
                };
                chunks.push(id);
                offset += cut as u64;
                total += cut as u64;
                buf.drain(..cut);
            }
            if total != size {
                return Err(Error::FileChanged(rel.to_string()));
            }
            result = if chunks.len() == 1 {
                (EntryKind::File, chunks[0])
            } else {
                let man = Manifest {
                    total_len: total,
                    chunks,
                };
                let id = if store_it {
                    self.store.put(ObjectType::Manifest, &man.encode(), None)?
                } else {
                    man.id()
                };
                (EntryKind::ChunkedFile, id)
            };
        }
        let m2 = f.metadata()?;
        if m2.len() != size || m2.modified().ok() != m1.modified().ok() {
            return Err(Error::FileChanged(rel.to_string()));
        }
        let _ = &mut result;
        Ok(result)
    }

    pub fn ingest_symlink(&mut self, rel: &str, target: &[u8], store_it: bool) -> Result<ObjectId> {
        let _ = rel;
        Ok(if store_it {
            self.store.put(ObjectType::Chunk, target, None)?
        } else {
            hash_object(ObjectType::Chunk, target)
        })
    }

    /// Reads the complete bytes of a file entry (whole or chunked).
    pub fn read_file_bytes(&self, e: &FlatEntry) -> Result<Vec<u8>> {
        match e.kind {
            EntryKind::File | EntryKind::Symlink => Ok(self.store.get(&e.id)?.1),
            EntryKind::ChunkedFile => {
                let m = self.read_manifest(&e.id)?;
                let mut out = Vec::with_capacity(m.total_len as usize);
                for c in &m.chunks {
                    out.extend_from_slice(&self.store.get(c)?.1);
                }
                if out.len() as u64 != m.total_len {
                    return Err(Error::Invalid(
                        "manifest length does not match chunks".into(),
                    ));
                }
                Ok(out)
            }
            EntryKind::Dir => Err(Error::Invalid("not a file".into())),
        }
    }

    /// Streams a file entry into `w`.
    pub fn write_file_to(&self, e: &FlatEntry, w: &mut dyn std::io::Write) -> Result<u64> {
        match e.kind {
            EntryKind::File | EntryKind::Symlink => {
                let d = self.store.get(&e.id)?.1;
                w.write_all(&d)?;
                Ok(d.len() as u64)
            }
            EntryKind::ChunkedFile => {
                let m = self.read_manifest(&e.id)?;
                let mut n = 0u64;
                for c in &m.chunks {
                    let d = self.store.get(c)?.1;
                    w.write_all(&d)?;
                    n += d.len() as u64;
                }
                if n != m.total_len {
                    return Err(Error::Invalid(
                        "manifest length does not match chunks".into(),
                    ));
                }
                Ok(n)
            }
            EntryKind::Dir => Err(Error::Invalid("not a file".into())),
        }
    }

    // ----- revisions & history --------------------------------------------------------

    /// Resolves a branch name, full/short commit id, or `HEAD`, with optional `~N` suffix.
    pub fn resolve_rev(&self, rev: &str) -> Result<ObjectId> {
        let (base, ups) = match rev.split_once('~') {
            Some((b, n)) => (
                b,
                n.parse::<usize>()
                    .map_err(|_| Error::UnknownRevision(rev.into()))?,
            ),
            None => (rev, 0),
        };
        let mut id = self
            .resolve_base(base)
            .ok_or_else(|| Error::UnknownRevision(rev.into()))?;
        for _ in 0..ups {
            let c = self.read_commit(&id)?;
            id = *c
                .parents
                .first()
                .ok_or_else(|| Error::UnknownRevision(rev.into()))?;
        }
        Ok(id)
    }

    fn resolve_base(&self, base: &str) -> Option<ObjectId> {
        if base == "HEAD" {
            return self.head_commit().ok().flatten();
        }
        if let Ok(Some(r)) = self.get_ref(&format!("heads/{base}")) {
            if let Some(c) = r.commit {
                return Some(c);
            }
        }
        if let Ok(Some(r)) = self.get_ref(&format!("remotes/{base}")) {
            if let Some(c) = r.commit {
                return Some(c);
            }
        }
        if let Ok(Some(r)) = self.get_ref(&format!("tags/{base}")) {
            if let Some(c) = r.commit {
                return Some(c);
            }
        }
        if base.len() == 64 {
            return ObjectId::from_hex(base)
                .ok()
                .filter(|i| self.store.has(i).unwrap_or(false));
        }
        if base.len() >= 6 && base.bytes().all(|c| c.is_ascii_hexdigit()) {
            let lower = base.to_ascii_lowercase();
            let mut hit = None;
            for id in self.store.ids().ok()? {
                if id.to_hex().starts_with(&lower)
                    && self.store.object_type(&id).ok().flatten() == Some(ObjectType::Commit)
                {
                    if hit.is_some() {
                        return None;
                    }
                    hit = Some(id);
                }
            }
            return hit;
        }
        None
    }

    /// Commits reachable from `from`, newest first by commit time (ties by traversal).
    pub fn history(&self, from: &ObjectId, limit: usize) -> Result<Vec<(ObjectId, Commit)>> {
        if limit == usize::MAX {
            return self.history_all(from);
        }
        // A bounded log reads only about `limit` commits (newest first by commit time),
        // not the whole history.
        let mut heap: std::collections::BinaryHeap<(i64, usize, ObjectId)> = Default::default();
        let mut commits: HashMap<usize, Commit> = HashMap::new();
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut next = 0usize;
        let first = self.read_commit(from)?;
        heap.push((first.timestamp, usize::MAX - next, *from));
        commits.insert(usize::MAX - next, first);
        seen.insert(*from);
        next += 1;
        let mut out = Vec::new();
        while let Some((_, key, id)) = heap.pop() {
            let c = commits.remove(&key).expect("queued commit");
            for p in &c.parents {
                if seen.insert(*p) {
                    let pc = self.read_commit(p)?;
                    let k = usize::MAX - next;
                    next += 1;
                    heap.push((pc.timestamp, k, *p));
                    commits.insert(k, pc);
                }
            }
            out.push((id, c));
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Every commit reachable from `from`, children before parents.
    pub fn history_all(&self, from: &ObjectId) -> Result<Vec<(ObjectId, Commit)>> {
        let limit = usize::MAX;
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut all: Vec<(ObjectId, Commit)> = Vec::new();
        let mut queue = VecDeque::from([*from]);
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            let c = self.read_commit(&id)?;
            for p in &c.parents {
                queue.push_back(*p);
            }
            all.push((id, c));
        }
        // Order so that every commit appears before its parents.
        let index: HashMap<ObjectId, usize> = all
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (*id, i))
            .collect();
        let mut indeg: Vec<usize> = vec![0; all.len()];
        for (_, c) in &all {
            for p in &c.parents {
                indeg[index[p]] += 1;
            }
        }
        let mut ready: Vec<usize> = (0..all.len()).filter(|&i| indeg[i] == 0).collect();
        let mut out = Vec::with_capacity(all.len());
        while !ready.is_empty() {
            ready.sort_by_key(|&i| (all[i].1.timestamp, std::cmp::Reverse(i)));
            let i = ready.pop().unwrap();
            out.push(all[i].clone());
            for p in &all[i].1.parents {
                let pi = index[p];
                indeg[pi] -= 1;
                if indeg[pi] == 0 {
                    ready.push(pi);
                }
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    pub fn ancestors(&self, from: &ObjectId) -> Result<HashSet<ObjectId>> {
        let mut seen = HashSet::new();
        let mut stack = vec![*from];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            stack.extend(self.read_commit(&id)?.parents);
        }
        Ok(seen)
    }

    pub fn is_ancestor(&self, ancestor: &ObjectId, of: &ObjectId) -> Result<bool> {
        Ok(self.ancestors(of)?.contains(ancestor))
    }

    pub fn merge_base(&self, a: &ObjectId, b: &ObjectId) -> Result<Option<ObjectId>> {
        let anc_a = self.ancestors(a)?;
        let anc_b = self.ancestors(b)?;
        let common: Vec<ObjectId> = anc_a.intersection(&anc_b).copied().collect();
        // Best common ancestors: not an ancestor of another common ancestor.
        let mut best: Vec<(i64, ObjectId)> = Vec::new();
        let mut shadowed: HashSet<ObjectId> = HashSet::new();
        for c in &common {
            for p in self.read_commit(c)?.parents {
                shadowed.extend(self.ancestors(&p)?);
            }
        }
        for c in &common {
            if !shadowed.contains(c) {
                best.push((self.read_commit(c)?.timestamp, *c));
            }
        }
        best.sort();
        Ok(best.pop().map(|(_, id)| id))
    }

    pub fn flush(&mut self) -> Result<()> {
        Ok(self.store.flush()?)
    }
}
