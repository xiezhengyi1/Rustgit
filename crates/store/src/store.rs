//! Pack-backed object store with a rebuildable SQLite index.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::codec::{Decoder, Encoder};
use crate::error::{Error, Result};
use crate::id::{hash_object, ObjectId, ObjectType};
use crate::pack::{
    encode_record, read_record, Encoding, ReadOutcome, RecordHead, DEFAULT_MAX_DELTA_DEPTH,
    FORMAT_MAX_DELTA_DEPTH, MAGIC, MAX_GROUP_BYTES, MAX_GROUP_MEMBERS, MAX_OBJECT,
};

const INDEX_VERSION: i64 = 2;
const OBJECTS_DIR: &str = "objects";

#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub zstd_level: i32,
    pub delta_level: i32,
    /// Roll to a new pack once the active one reaches this size.
    pub pack_max: u64,
    /// Re-hash objects as they are read.
    pub verify_on_read: bool,
    /// Longest delta chain this writer creates (readers accept up to 64).
    pub max_delta_depth: u32,
}

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions {
            zstd_level: 3,
            delta_level: 12,
            pack_max: 256 * 1024 * 1024,
            verify_on_read: true,
            max_delta_depth: DEFAULT_MAX_DELTA_DEPTH,
        }
    }
}

#[derive(Clone, Debug)]
struct Row {
    ty: ObjectType,
    pack: i64,
    off: u64,
    stored_len: u64,
    enc: Encoding,
    logical_len: u64,
    base: Option<ObjectId>,
    depth: u32,
    /// Position inside a solid group (encoding Group only).
    member: u32,
}

/// A decoded solid group: members are `(type, range into data)`.
struct Group {
    members: Vec<(ObjectType, std::ops::Range<usize>)>,
    data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct RepackOptions {
    /// Only packs idle for at least this long are rewritten.
    pub grace_secs: u64,
    /// zstd level for rewritten objects and groups.
    pub level: i32,
    pub delta_level: i32,
    /// Objects up to this size are grouped together instead of stored one by one.
    pub group_object_max: usize,
    /// Target uncompressed size of a group.
    pub group_target: usize,
}

impl Default for RepackOptions {
    fn default() -> Self {
        RepackOptions {
            grace_secs: 3600,
            level: 9,
            delta_level: 12,
            group_object_max: 32 * 1024,
            group_target: 128 * 1024,
        }
    }
}

impl RepackOptions {
    pub fn aggressive() -> Self {
        RepackOptions {
            grace_secs: 0,
            level: 19,
            delta_level: 9,
            ..Default::default()
        }
    }
}

/// A small object waiting to be grouped: (id, type, length, delta hint).
type Pending = (ObjectId, ObjectType, u32, Option<ObjectId>);

#[derive(Default)]
struct Lineage {
    root_of: HashMap<ObjectId, ObjectId>,
    chains: HashMap<ObjectId, Vec<ObjectId>>,
}

struct Active {
    id: i64,
    file: File,
    /// Length including unwritten buffered bytes.
    len: u64,
    buf: Vec<u8>,
}

impl Active {
    fn flush_buf(&mut self) -> Result<()> {
        if !self.buf.is_empty() {
            self.file.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

#[derive(Default, Debug, Clone)]
pub struct StoreStats {
    pub objects: u64,
    pub logical_bytes: u64,
    pub stored_bytes: u64,
    pub packs: u64,
    pub pack_file_bytes: u64,
    pub index_file_bytes: u64,
    pub delta_objects: u64,
}

#[derive(Default, Debug, Clone)]
pub struct RepackStats {
    pub moved: u64,
    pub dropped: u64,
    pub dropped_logical_bytes: u64,
    pub retired_packs: u64,
    pub new_deltas: u64,
    pub grouped_objects: u64,
    pub groups: u64,
}

#[derive(Default, Debug, Clone)]
pub struct ReindexReport {
    pub packs: u64,
    pub objects: u64,
    pub problems: Vec<String>,
}

pub struct Store {
    dir: PathBuf,
    db: Connection,
    opts: StoreOptions,
    active: RefCell<Active>,
    readers: RefCell<HashMap<i64, File>>,
    in_txn: Cell<bool>,
    groups: RefCell<HashMap<(i64, u64), Arc<Group>>>,
    warnings: Vec<String>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn pack_path(dir: &Path, id: i64) -> PathBuf {
    dir.join(OBJECTS_DIR).join(format!("pack-{id:06}.rgpk"))
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}

#[cfg(windows)]
fn read_at(f: &File, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, off)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        off += n as u64;
    }
    Ok(())
}

fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(path) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

// zstd contexts are large (tens of MiB at high levels) and `zstd::bulk::compress` allocates and
// frees one per call, which fragmented the heap and inflated peak memory. One context per
// thread and level is reused instead.
thread_local! {
    static CCTX: RefCell<HashMap<i32, zstd::bulk::Compressor<'static>>> = RefCell::new(HashMap::new());
    static DCTX: RefCell<Option<zstd::bulk::Decompressor<'static>>> = const { RefCell::new(None) };
    static DCTX_DICT: RefCell<Option<zstd::bulk::Decompressor<'static>>> = const { RefCell::new(None) };
}

fn zcompress(data: &[u8], level: i32) -> std::io::Result<Vec<u8>> {
    CCTX.with(|c| {
        let mut m = c.borrow_mut();
        if !m.contains_key(&level) {
            if m.len() >= 3 {
                m.clear();
            }
            m.insert(level, zstd::bulk::Compressor::new(level)?);
        }
        let mut out = m.get_mut(&level).unwrap().compress(data)?;
        out.shrink_to_fit();
        Ok(out)
    })
}

/// Compresses `data` using `dict` (the delta base) as a raw-content dictionary.
fn zcompress_dict(data: &[u8], level: i32, dict: &[u8]) -> std::io::Result<Vec<u8>> {
    CCTX.with(|c| {
        let mut m = c.borrow_mut();
        // A separate key space for dictionary compressors (negative = delta contexts).
        let key = -level - 1000;
        if !m.contains_key(&key) {
            if m.len() >= 3 {
                m.clear();
            }
            m.insert(key, zstd::bulk::Compressor::new(level)?);
        }
        let comp = m.get_mut(&key).unwrap();
        comp.set_dictionary(level, dict)?;
        let mut out = comp.compress(data)?;
        out.shrink_to_fit();
        Ok(out)
    })
}

fn decode_zstd(payload: &[u8], logical_len: u64, dict: Option<&[u8]>) -> Result<Vec<u8>> {
    let out = match dict {
        None => DCTX.with(|d| {
            let mut d = d.borrow_mut();
            if d.is_none() {
                *d = Some(zstd::bulk::Decompressor::new()?);
            }
            d.as_mut().unwrap().decompress(payload, logical_len as usize)
        }),
        Some(dict) => DCTX_DICT.with(|d| {
            let mut d = d.borrow_mut();
            if d.is_none() {
                *d = Some(zstd::bulk::Decompressor::new()?);
            }
            let z = d.as_mut().unwrap();
            z.set_dictionary(dict)?;
            z.decompress(payload, logical_len as usize)
        }),
    }
    .map_err(|e| Error::corrupt(format!("zstd decode failed: {e}")))?;
    if out.len() as u64 != logical_len {
        return Err(Error::corrupt("decoded length differs from record"));
    }
    Ok(out)
}

fn decode_group(payload: &[u8], expected_len: u64) -> Result<Group> {
    let cap = zstd::bulk::Decompressor::upper_bound(payload)
        .ok_or_else(|| Error::corrupt("group frame has no content size"))?;
    if cap > MAX_GROUP_BYTES || (expected_len != 0 && expected_len as usize != cap) {
        return Err(Error::corrupt("group size out of range"));
    }
    let data = decode_zstd(payload, cap as u64, None)?;
    let mut d = Decoder::new(&data);
    let n = d.varint()? as usize;
    if n == 0 || n > MAX_GROUP_MEMBERS {
        return Err(Error::corrupt("group member count out of range"));
    }
    let mut metas = Vec::with_capacity(n);
    for _ in 0..n {
        let ty = ObjectType::from_u8(d.u8()?)?;
        let len = d.varint()? as usize;
        metas.push((ty, len));
    }
    let start = data.len() - d.remaining();
    let mut off = start;
    let mut members = Vec::with_capacity(n);
    for (ty, len) in metas {
        if off + len > data.len() {
            return Err(Error::corrupt("group member exceeds group"));
        }
        members.push((ty, off..off + len));
        off += len;
    }
    if off != data.len() {
        return Err(Error::corrupt("group has trailing bytes"));
    }
    Ok(Group { members, data })
}

impl Store {
    /// Creates an empty store in `dir` (the directory is created if needed).
    pub fn create(dir: &Path, opts: StoreOptions) -> Result<Store> {
        fs::create_dir_all(dir.join(OBJECTS_DIR))?;
        if dir.join("index.sqlite").exists() {
            return Err(Error::Unsupported("store already exists".into()));
        }
        Self::open(dir, opts)
    }

    /// Opens a store, creating it if absent. Rebuilds the index if it is missing
    /// and finishes any append that was durable in a pack but not yet indexed.
    pub fn open(dir: &Path, opts: StoreOptions) -> Result<Store> {
        fs::create_dir_all(dir.join(OBJECTS_DIR))?;
        let db_path = dir.join("index.sqlite");
        let fresh = !db_path.exists();
        let db = Connection::open(&db_path)?;
        db.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v);
             CREATE TABLE IF NOT EXISTS packs(
                 id INTEGER PRIMARY KEY, name TEXT NOT NULL, indexed_len INTEGER NOT NULL,
                 last_write INTEGER NOT NULL, retired_at INTEGER);
             CREATE TABLE IF NOT EXISTS objects(
                 id BLOB PRIMARY KEY, ty INTEGER NOT NULL, pack INTEGER NOT NULL,
                 off INTEGER NOT NULL, stored_len INTEGER NOT NULL, enc INTEGER NOT NULL,
                 logical_len INTEGER NOT NULL, base BLOB, depth INTEGER NOT NULL,
                 member INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;",
        )?;
        let ver: Option<i64> = db
            .query_row("SELECT v FROM meta WHERE k='index_version'", [], |r| {
                r.get(0)
            })
            .optional()?;
        match ver {
            None => {
                db.execute(
                    "INSERT INTO meta(k,v) VALUES('index_version',?)",
                    [INDEX_VERSION],
                )?;
            }
            Some(v) if v == INDEX_VERSION => {}
            Some(v) if v < INDEX_VERSION => {
                // The index is only a cache: drop it and rebuild from the packs.
                db.execute_batch("DROP TABLE objects; DROP TABLE packs; DROP TABLE meta;")?;
                drop(db);
                fs::remove_file(&db_path)?;
                return Self::open(dir, opts);
            }
            Some(v) => {
                return Err(Error::Unsupported(format!(
                    "index version {v} is newer than this build supports"
                )));
            }
        }
        let placeholder = Active {
            id: 0,
            file: tempfile_placeholder()?,
            len: 0,
            buf: Vec::new(),
        };
        let mut store = Store {
            dir: dir.to_path_buf(),
            db,
            opts,
            active: RefCell::new(placeholder),
            readers: RefCell::new(HashMap::new()),
            in_txn: Cell::new(false),
            groups: RefCell::new(HashMap::new()),
            warnings: Vec::new(),
        };
        let packs_on_disk = store.list_pack_files()?;
        let indexed: i64 = store
            .db
            .query_row("SELECT count(*) FROM packs", [], |r| r.get(0))?;
        if indexed == 0 && !packs_on_disk.is_empty() {
            store
                .warnings
                .push("index was missing or empty; rebuilt from packs".into());
            let rep = store.reindex()?;
            store.warnings.extend(rep.problems);
        } else {
            store.recover_tails(&packs_on_disk)?;
        }
        store.ensure_active()?;
        store.commit_index()?;
        let _ = fresh;
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn set_max_delta_depth(&mut self, d: u32) {
        self.opts.max_delta_depth = d.clamp(1, FORMAT_MAX_DELTA_DEPTH);
    }

    pub fn options(&self) -> &StoreOptions {
        &self.opts
    }

    fn list_pack_files(&self) -> Result<Vec<(i64, PathBuf)>> {
        let mut v = Vec::new();
        for e in fs::read_dir(self.dir.join(OBJECTS_DIR))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(num) = name
                .strip_prefix("pack-")
                .and_then(|n| n.strip_suffix(".rgpk"))
            {
                if let Ok(id) = num.parse::<i64>() {
                    v.push((id, e.path()));
                }
            }
        }
        v.sort();
        Ok(v)
    }

    fn begin(&self) -> Result<()> {
        if !self.in_txn.get() {
            self.db.execute_batch("BEGIN IMMEDIATE")?;
            self.in_txn.set(true);
        }
        Ok(())
    }

    /// Indexes complete records that were appended to packs but never committed to
    /// the index (a crash between pack fsync and index commit), and quarantines a
    /// torn trailing record.
    fn recover_tails(&mut self, on_disk: &[(i64, PathBuf)]) -> Result<()> {
        let known: HashMap<i64, (i64, Option<i64>)> = {
            let mut st = self
                .db
                .prepare("SELECT id, indexed_len, retired_at FROM packs")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    (r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?),
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let last_id = on_disk.last().map(|p| p.0);
        for (id, path) in on_disk {
            let len = fs::metadata(path)?.len();
            match known.get(id) {
                None => {
                    // Not in the index: either an interrupted repack or a new pack whose
                    // creating transaction never committed. Keep the data, set aside.
                    let orphan = path.with_extension("orphan");
                    fs::rename(path, &orphan)?;
                    self.warnings.push(format!(
                        "set aside pack {} that the index does not know (interrupted repack?)",
                        path.display()
                    ));
                }
                Some((indexed_len, retired)) => {
                    let indexed_len = *indexed_len as u64;
                    if len < indexed_len {
                        self.warnings.push(format!(
                            "pack {id} is {len} bytes but the index expects {indexed_len}: data may be lost"
                        ));
                    } else if len > indexed_len && retired.is_none() {
                        let final_pack = Some(*id) == last_id;
                        self.scan_pack(*id, indexed_len.max(MAGIC.len() as u64), final_pack)?;
                    }
                }
            }
        }
        if self.in_txn.get() {
            self.commit_index()?;
        }
        Ok(())
    }

    fn commit_index(&self) -> Result<()> {
        if self.in_txn.get() {
            self.db.execute_batch("COMMIT")?;
            self.in_txn.set(false);
        }
        Ok(())
    }

    /// Scans records of a pack starting at `start`, indexing each. Returns the end of
    /// the valid region. A torn tail of `quarantine_tail` packs is moved aside.
    fn scan_pack(&mut self, pack_id: i64, start: u64, quarantine_tail: bool) -> Result<(u64, u64)> {
        let path = pack_path(&self.dir, pack_id);
        let file_len = fs::metadata(&path)?.len();
        let mut f = File::open(&path)?;
        let mut head = [0u8; 8];
        if file_len >= 8 {
            read_at(&f, &mut head, 0)?;
        }
        if head != MAGIC {
            self.warnings
                .push(format!("pack {pack_id} has a bad header; skipped"));
            return Ok((start, 0));
        }
        f.seek(SeekFrom::Start(start))?;
        let mut r = BufReader::with_capacity(1 << 20, f);
        let mut pos = start;
        let mut count = 0;
        self.begin()?;
        loop {
            match read_record(&mut r) {
                Ok(ReadOutcome::Eof) => break,
                Ok(ReadOutcome::Torn) => {
                    if quarantine_tail {
                        let tail_len = file_len - pos;
                        let mut tail = vec![0u8; tail_len as usize];
                        let rf = File::open(&path)?;
                        read_at(&rf, &mut tail, pos)?;
                        let q = self
                            .dir
                            .join(OBJECTS_DIR)
                            .join(format!("pack-{pack_id:06}.tail-{pos}"));
                        fs::write(&q, &tail)?;
                        let wf = OpenOptions::new().write(true).open(&path)?;
                        wf.set_len(pos)?;
                        wf.sync_all()?;
                        self.warnings.push(format!(
                            "truncated {tail_len} bytes of a torn record in pack {pack_id} (kept in {})",
                            q.display()
                        ));
                    } else {
                        self.warnings
                            .push(format!("pack {pack_id} ends in a torn record at {pos}"));
                    }
                    break;
                }
                Ok(ReadOutcome::Record {
                    head,
                    payload,
                    payload_off,
                    len,
                }) => {
                    let off = pos + payload_off as u64;
                    match self.index_record(pack_id, off, &head, &payload) {
                        Ok(n) => count += n,
                        Err(e) => {
                            self.warnings
                                .push(format!("pack {pack_id} offset {pos}: {e}; scan stopped"));
                            break;
                        }
                    }
                    pos += len;
                }
                Err(e) => {
                    self.warnings
                        .push(format!("pack {pack_id} offset {pos}: {e}; scan stopped"));
                    break;
                }
            }
        }
        self.db.execute(
            "UPDATE packs SET indexed_len=?, last_write=? WHERE id=?",
            params![pos as i64, now(), pack_id],
        )?;
        Ok((pos, count))
    }

    /// Decodes one record read during a scan and adds index rows for its objects.
    fn index_record(
        &self,
        pack_id: i64,
        off: u64,
        head: &RecordHead,
        payload: &[u8],
    ) -> Result<u64> {
        if head.enc == Encoding::Group {
            let g = decode_group(payload, head.logical_len)?;
            let total = g.members.len().max(1) as u64;
            for (i, (ty, range)) in g.members.iter().enumerate() {
                let id = hash_object(*ty, &g.data[range.clone()]);
                let row = Row {
                    ty: *ty,
                    pack: pack_id,
                    off,
                    stored_len: head.stored_len,
                    enc: Encoding::Group,
                    logical_len: range.len() as u64,
                    base: None,
                    depth: 0,
                    member: i as u32,
                };
                self.insert_row(&id, &row, false)?;
            }
            let _ = total;
            return Ok(g.members.len() as u64);
        }
        let logical = match head.enc {
            Encoding::Raw => payload.to_vec(),
            Encoding::Zstd => decode_zstd(payload, head.logical_len, None)?,
            Encoding::Delta => {
                let base = head
                    .base
                    .as_ref()
                    .ok_or_else(|| Error::corrupt("delta without base"))?;
                let (_, base_bytes) = self.get_depth(base, 1)?;
                decode_zstd(payload, head.logical_len, Some(&base_bytes))?
            }
            Encoding::Group => unreachable!(),
        };
        let id = hash_object(head.ty, &logical);
        let depth = match &head.base {
            Some(b) => match self.lookup(b)? {
                Some(br) => br.depth + 1,
                None => return Err(Error::corrupt("delta base missing")),
            },
            None => 0,
        };
        if depth > FORMAT_MAX_DELTA_DEPTH {
            return Err(Error::corrupt("delta chain too deep"));
        }
        let row = Row {
            ty: head.ty,
            pack: pack_id,
            off,
            stored_len: head.stored_len,
            enc: head.enc,
            logical_len: head.logical_len,
            base: head.base,
            depth,
            member: 0,
        };
        self.insert_row(&id, &row, false)?;
        Ok(1)
    }

    fn insert_row(&self, id: &ObjectId, r: &Row, replace: bool) -> Result<()> {
        let sql = if replace {
            "INSERT OR REPLACE INTO objects(id,ty,pack,off,stored_len,enc,logical_len,base,depth,member) VALUES(?,?,?,?,?,?,?,?,?,?)"
        } else {
            "INSERT OR IGNORE INTO objects(id,ty,pack,off,stored_len,enc,logical_len,base,depth,member) VALUES(?,?,?,?,?,?,?,?,?,?)"
        };
        self.db.prepare_cached(sql)?.execute(params![
            &id.0[..],
            r.ty as u8,
            r.pack,
            r.off as i64,
            r.stored_len as i64,
            r.enc as u8,
            r.logical_len as i64,
            r.base.as_ref().map(|b| b.0.to_vec()),
            r.depth,
            r.member
        ])?;
        Ok(())
    }

    fn lookup(&self, id: &ObjectId) -> Result<Option<Row>> {
        let mut st = self.db.prepare_cached(
            "SELECT ty,pack,off,stored_len,enc,logical_len,base,depth,member FROM objects WHERE id=?",
        )?;
        let raw = st
            .query_row([&id.0[..]], |r| {
                Ok((
                    r.get::<_, u8>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, u8>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, Option<Vec<u8>>>(6)?,
                    r.get::<_, u32>(7)?,
                    r.get::<_, u32>(8)?,
                ))
            })
            .optional()?;
        match raw {
            None => Ok(None),
            Some((ty, pack, off, stored_len, enc, logical_len, base, depth, member)) => {
                Ok(Some(Row {
                    ty: ObjectType::from_u8(ty)?,
                    pack,
                    off: off as u64,
                    stored_len: stored_len as u64,
                    enc: Encoding::from_u8(enc)?,
                    logical_len: logical_len as u64,
                    base: base.map(|b| ObjectId::from_slice(&b)).transpose()?,
                    depth,
                    member,
                }))
            }
        }
    }

    fn ensure_active(&mut self) -> Result<()> {
        let cur: Option<(i64, i64)> = self
            .db
            .query_row(
                "SELECT id, indexed_len FROM packs WHERE retired_at IS NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match cur {
            Some((id, _)) if fs::metadata(pack_path(&self.dir, id)).is_ok() => self.open_active(id),
            _ => self.new_active(),
        }
    }

    fn open_active(&mut self, id: i64) -> Result<()> {
        let path = pack_path(&self.dir, id);
        let file = OpenOptions::new().append(true).open(&path)?;
        let len = file.metadata()?.len();
        *self.active.borrow_mut() = Active {
            id,
            file,
            len,
            buf: Vec::new(),
        };
        if len >= self.opts.pack_max {
            self.new_active()?;
        }
        Ok(())
    }

    fn new_active(&mut self) -> Result<()> {
        let next: i64 = self
            .db
            .query_row("SELECT COALESCE(MAX(id),0)+1 FROM packs", [], |r| r.get(0))?;
        let disk_max = self.list_pack_files()?.last().map(|p| p.0).unwrap_or(0);
        let id = next.max(disk_max + 1);
        let path = pack_path(&self.dir, id);
        let mut file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        file.write_all(&MAGIC)?;
        file.sync_all()?;
        sync_dir(&self.dir.join(OBJECTS_DIR));
        self.begin()?;
        self.db.execute(
            "INSERT INTO packs(id,name,indexed_len,last_write) VALUES(?,?,?,?)",
            params![
                id,
                path.file_name().unwrap().to_string_lossy(),
                MAGIC.len() as i64,
                now()
            ],
        )?;
        *self.active.borrow_mut() = Active {
            id,
            file,
            len: MAGIC.len() as u64,
            buf: Vec::new(),
        };
        Ok(())
    }

    pub fn has(&self, id: &ObjectId) -> Result<bool> {
        Ok(self.lookup(id)?.is_some())
    }

    pub fn object_type(&self, id: &ObjectId) -> Result<Option<ObjectType>> {
        Ok(self.lookup(id)?.map(|r| r.ty))
    }

    pub fn logical_len(&self, id: &ObjectId) -> Result<Option<u64>> {
        Ok(self.lookup(id)?.map(|r| r.logical_len))
    }

    /// Stores an object. `hint` names a similar earlier object (for example the
    /// previous version of the same chunk) to try a delta against.
    pub fn put(
        &mut self,
        ty: ObjectType,
        data: &[u8],
        hint: Option<&ObjectId>,
    ) -> Result<ObjectId> {
        if data.len() as u64 > MAX_OBJECT {
            return Err(Error::Limit(format!(
                "object of {} bytes exceeds {MAX_OBJECT}",
                data.len()
            )));
        }
        let id = hash_object(ty, data);
        if self.lookup(&id)?.is_some() {
            return Ok(id);
        }
        self.begin()?;
        let cands: Option<ObjectId> = hint.copied();
        let (enc, base, payload, depth) = self.encode_single(
            ty,
            data,
            cands.as_slice(),
            self.opts.zstd_level,
            self.opts.delta_level,
            true,
            &|_| true,
        )?;
        self.append(
            &id,
            ty,
            enc,
            base,
            data.len() as u64,
            depth,
            &payload,
            false,
        )?;
        Ok(id)
    }

    /// Picks the smallest physical form for one object: raw, zstd, or a delta against the
    /// hint or one of its own delta bases (so that a chain that reached the depth limit
    /// still finds a usable base). Returns (encoding, base, payload, depth).
    #[allow(clippy::too_many_arguments)]
    fn encode_single(
        &self,
        ty: ObjectType,
        data: &[u8],
        cands: &[ObjectId],
        level: i32,
        delta_level: i32,
        allow_delta: bool,
        base_ok: &dyn Fn(&Row) -> bool,
    ) -> Result<(Encoding, Option<ObjectId>, Vec<u8>, u32)> {
        let mut best_enc = Encoding::Raw;
        let mut best: Vec<u8> = Vec::new();
        let mut best_len = data.len();
        let try_delta = allow_delta && data.len() >= 256 && !cands.is_empty();
        // With candidate bases, estimate the full size cheaply first; the expensive
        // high-level compression only runs if no delta wins.
        let first_level = if try_delta { level.min(3) } else { level };
        if data.len() >= 32 {
            let z = zcompress(data, first_level)?;
            if z.len() < data.len() {
                best_len = z.len();
                best_enc = Encoding::Zstd;
                best = z;
            }
        }
        if try_delta {
            // Try the preferred bases in order, then the bases those were themselves
            // deltas of, so a chain that reached the depth limit still finds a usable base.
            let mut queue: Vec<ObjectId> = cands.to_vec();
            let mut i = 0;
            while i < queue.len() && i < 6 {
                let c = queue[i];
                i += 1;
                let Some(brow) = self.lookup(&c)? else {
                    continue;
                };
                if brow.ty == ty
                    && brow.logical_len >= 64
                    && brow.depth < self.opts.max_delta_depth
                    && base_ok(&brow)
                {
                    let (_, base_bytes) = self.get(&c)?;
                    let d = zcompress_dict(data, delta_level, &base_bytes)?;
                    if (d.len() + 32) * 10 <= best_len * 9 {
                        return Ok((Encoding::Delta, Some(c), d, brow.depth + 1));
                    }
                }
                if let Some(b) = brow.base {
                    if !queue.contains(&b) {
                        queue.push(b);
                    }
                }
            }
        }
        if try_delta && first_level < level && data.len() >= 32 {
            let z = zcompress(data, level)?;
            if z.len() < best_len {
                best_enc = Encoding::Zstd;
                best = z;
            }
        }
        if best_enc == Encoding::Raw {
            best = data.to_vec();
        }
        Ok((best_enc, None, best, 0))
    }

    #[allow(clippy::too_many_arguments)]
    fn append(
        &self,
        id: &ObjectId,
        ty: ObjectType,
        enc: Encoding,
        base: Option<ObjectId>,
        logical_len: u64,
        depth: u32,
        payload: &[u8],
        replace: bool,
    ) -> Result<()> {
        let head = RecordHead {
            ty,
            enc,
            logical_len,
            stored_len: payload.len() as u64,
            base,
        };
        let (bytes, payload_off) = encode_record(&head, payload);
        let mut a = self.active.borrow_mut();
        let off = a.len + payload_off as u64;
        a.len += bytes.len() as u64;
        a.buf.extend_from_slice(&bytes);
        if a.buf.len() >= 1 << 20 {
            a.flush_buf()?;
        }
        let row = Row {
            ty,
            pack: a.id,
            off,
            stored_len: payload.len() as u64,
            enc,
            logical_len,
            base,
            depth,
            member: 0,
        };
        drop(a);
        self.insert_row(id, &row, replace)
    }

    /// Writes several small objects as one compressed group. Falls back to individual
    /// records when grouping does not save space.
    fn append_group(
        &self,
        members: &[(ObjectId, ObjectType, Vec<u8>)],
        level: i32,
    ) -> Result<bool> {
        let mut table = Encoder::new();
        table.varint(members.len() as u64);
        let mut total = 0usize;
        for (_, ty, d) in members {
            table.u8(*ty as u8).varint(d.len() as u64);
            total += d.len();
        }
        let mut buf = table.finish();
        buf.reserve(total);
        for (_, _, d) in members {
            buf.extend_from_slice(d);
        }
        let payload = zcompress(&buf, level)?;
        if payload.len() >= buf.len() || buf.len() > MAX_GROUP_BYTES {
            return Ok(false);
        }
        let head = RecordHead {
            ty: ObjectType::Chunk,
            enc: Encoding::Group,
            logical_len: buf.len() as u64,
            stored_len: payload.len() as u64,
            base: None,
        };
        let (bytes, payload_off) = encode_record(&head, &payload);
        let mut a = self.active.borrow_mut();
        let off = a.len + payload_off as u64;
        a.len += bytes.len() as u64;
        a.buf.extend_from_slice(&bytes);
        if a.buf.len() >= 1 << 20 {
            a.flush_buf()?;
        }
        let pack = a.id;
        drop(a);
        for (i, (id, ty, d)) in members.iter().enumerate() {
            let row = Row {
                ty: *ty,
                pack,
                off,
                stored_len: payload.len() as u64,
                enc: Encoding::Group,
                logical_len: d.len() as u64,
                base: None,
                depth: 0,
                member: i as u32,
            };
            self.insert_row(id, &row, true)?;
        }
        Ok(true)
    }

    pub fn get(&self, id: &ObjectId) -> Result<(ObjectType, Vec<u8>)> {
        self.get_depth(id, 0)
    }

    fn get_depth(&self, id: &ObjectId, depth: u32) -> Result<(ObjectType, Vec<u8>)> {
        if depth > FORMAT_MAX_DELTA_DEPTH + 1 {
            return Err(Error::corrupt(format!(
                "delta chain too deep or cyclic at {id}"
            )));
        }
        let row = self.lookup(id)?.ok_or(Error::NotFound(*id))?;
        let data = if row.enc == Encoding::Group {
            let g = self.load_group(row.pack, row.off, row.stored_len)?;
            let (ty, range) = g.members.get(row.member as usize).ok_or_else(|| {
                Error::corrupt(format!("group member {} missing for {id}", row.member))
            })?;
            if *ty != row.ty || range.len() as u64 != row.logical_len {
                return Err(Error::corrupt(format!("group member mismatch for {id}")));
            }
            g.data[range.clone()].to_vec()
        } else {
            let payload = self.read_payload(row.pack, row.off, row.stored_len)?;
            match row.enc {
                Encoding::Raw => payload,
                Encoding::Zstd => decode_zstd(&payload, row.logical_len, None)?,
                Encoding::Delta => {
                    let base = row
                        .base
                        .ok_or_else(|| Error::corrupt("delta without base"))?;
                    let (_, bb) = self.get_depth(&base, depth + 1)?;
                    decode_zstd(&payload, row.logical_len, Some(&bb))?
                }
                Encoding::Group => unreachable!(),
            }
        };
        if self.opts.verify_on_read && hash_object(row.ty, &data) != *id {
            return Err(Error::corrupt(format!(
                "object {id} does not match its hash"
            )));
        }
        Ok((row.ty, data))
    }

    fn load_group(&self, pack: i64, off: u64, stored_len: u64) -> Result<Arc<Group>> {
        if let Some(g) = self.groups.borrow().get(&(pack, off)) {
            return Ok(g.clone());
        }
        let payload = self.read_payload(pack, off, stored_len)?;
        let g = Arc::new(decode_group(&payload, 0)?);
        let mut cache = self.groups.borrow_mut();
        if cache.len() >= 16 {
            cache.clear();
        }
        cache.insert((pack, off), g.clone());
        Ok(g)
    }

    fn read_payload(&self, pack: i64, off: u64, len: u64) -> Result<Vec<u8>> {
        {
            let mut a = self.active.borrow_mut();
            if a.id == pack {
                a.flush_buf()?;
            }
        }
        let mut readers = self.readers.borrow_mut();
        if !readers.contains_key(&pack) {
            let f = File::open(pack_path(&self.dir, pack))
                .map_err(|e| Error::corrupt(format!("cannot open pack {pack}: {e}")))?;
            readers.insert(pack, f);
        }
        let f = &readers[&pack];
        let mut buf = vec![0u8; len as usize];
        read_at(f, &mut buf, off)
            .map_err(|e| Error::corrupt(format!("short read in pack {pack} at {off}: {e}")))?;
        Ok(buf)
    }

    /// Makes everything written so far durable: pack fsync, then index commit.
    pub fn flush(&mut self) -> Result<()> {
        if self.active.borrow().id == 0 {
            return self.commit_index();
        }
        {
            let mut a = self.active.borrow_mut();
            a.flush_buf()?;
            a.file.sync_all()?;
            if self.in_txn.get() {
                self.db.execute(
                    "UPDATE packs SET indexed_len=?, last_write=? WHERE id=?",
                    params![a.len as i64, now(), a.id],
                )?;
            }
        }
        self.commit_index()?;
        let roll = self.active.borrow().len >= self.opts.pack_max;
        if roll {
            self.new_active()?;
            self.commit_index()?;
        }
        Ok(())
    }

    pub fn ids(&self) -> Result<Vec<ObjectId>> {
        let mut st = self.db.prepare("SELECT id FROM objects")?;
        let rows = st.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(ObjectId::from_slice(&r?)?);
        }
        Ok(v)
    }

    pub fn stats(&self) -> Result<StoreStats> {
        let (objects, logical, deltas): (i64, i64, i64) = self.db.query_row(
            "SELECT count(*), COALESCE(sum(logical_len),0), COALESCE(sum(enc=2),0) FROM objects",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let singles: i64 = self.db.query_row(
            "SELECT COALESCE(sum(stored_len),0) FROM objects WHERE enc != 3",
            [],
            |r| r.get(0),
        )?;
        let grouped: i64 = self.db.query_row(
            "SELECT COALESCE(sum(l),0) FROM (SELECT max(stored_len) AS l FROM objects WHERE enc = 3 GROUP BY pack, off)",
            [],
            |r| r.get(0),
        )?;
        let packs: i64 = self.db.query_row(
            "SELECT count(*) FROM packs WHERE retired_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        let mut pack_bytes = 0;
        for (_, p) in self.list_pack_files()? {
            pack_bytes += fs::metadata(p)?.len();
        }
        Ok(StoreStats {
            objects: objects as u64,
            logical_bytes: logical as u64,
            stored_bytes: (singles + grouped) as u64,
            packs: packs as u64,
            pack_file_bytes: pack_bytes,
            index_file_bytes: fs::metadata(self.dir.join("index.sqlite"))?.len(),
            delta_objects: deltas as u64,
        })
    }

    /// Re-reads every object, checks hashes, delta depth and the index database.
    pub fn verify_physical(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let qc: String = self.db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if qc != "ok" {
            problems.push(format!("index database check failed: {qc}"));
        }
        // Streamed in pack order: no list of all ids in memory, and every member of a
        // compressed group is checked while its group is still cached.
        let mut st = self
            .db
            .prepare("SELECT id FROM objects ORDER BY pack, off, member")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let id = ObjectId::from_slice(&r.get::<_, Vec<u8>>(0)?)?;
            let row = match self.lookup(&id) {
                Ok(Some(r)) => r,
                _ => {
                    problems.push(format!("{id}: unreadable index row"));
                    continue;
                }
            };
            if row.depth > FORMAT_MAX_DELTA_DEPTH {
                problems.push(format!("{id}: delta depth {} exceeds limit", row.depth));
            }
            match self.get(&id) {
                Ok((_, data)) => {
                    if data.len() as u64 != row.logical_len {
                        problems.push(format!("{id}: length differs from index"));
                    }
                }
                Err(e) => problems.push(format!("{id}: {e}")),
            }
        }
        Ok(problems)
    }

    /// Rebuilds the index from pack files. Cannot recreate content that is not in a pack.
    pub fn reindex(&mut self) -> Result<ReindexReport> {
        self.flush()?;
        self.readers.borrow_mut().clear();
        let before = self.warnings.len();
        self.begin()?;
        self.db
            .execute_batch("DELETE FROM objects; DELETE FROM packs;")?;
        let files = self.list_pack_files()?;
        let last = files.last().map(|p| p.0);
        let mut rep = ReindexReport::default();
        for (id, path) in &files {
            let mtime = fs::metadata(path)?
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or_else(now);
            self.db.execute(
                "INSERT INTO packs(id,name,indexed_len,last_write) VALUES(?,?,?,?)",
                params![id, path.file_name().unwrap().to_string_lossy(), 0, mtime],
            )?;
            let (_, n) = self.scan_pack(*id, MAGIC.len() as u64, Some(*id) == last)?;
            rep.packs += 1;
            rep.objects += n;
        }
        self.commit_index()?;
        rep.problems = self.warnings[before..].to_vec();
        self.ensure_active()?;
        Ok(rep)
    }

    /// Starts a fresh active pack so the current one can be repacked.
    pub fn roll(&mut self) -> Result<()> {
        self.flush()?;
        self.new_active()?;
        self.commit_index()
    }

    /// Rewrites packs idle for at least `opts.grace_secs`, keeping only objects in `items`
    /// (plus anything they physically depend on). `items` must list a delta hint before
    /// the objects that use it. Small objects are regrouped and compressed together;
    /// larger ones are recompressed or delta-encoded against their hint. Objects in packs
    /// written more recently than the grace period are left untouched.
    pub fn repack(
        &mut self,
        items: &[(ObjectId, Option<ObjectId>)],
        opts: &RepackOptions,
    ) -> Result<RepackStats> {
        self.roll()?;
        let cutoff = now() - opts.grace_secs as i64;
        let active_id = self.active.borrow().id;
        self.begin()?;
        self.db.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS retiring(id INTEGER PRIMARY KEY); DELETE FROM retiring;",
        )?;
        self.db.execute(
            "INSERT INTO retiring SELECT id FROM packs WHERE retired_at IS NULL AND id != ? AND last_write <= ?",
            params![active_id, cutoff],
        )?;
        let n: i64 = self
            .db
            .query_row("SELECT count(*) FROM retiring", [], |r| r.get(0))?;
        let mut stats = RepackStats::default();
        if n == 0 {
            self.commit_index()?;
            return Ok(stats);
        }
        stats.retired_packs = n as u64;
        // Bases in retiring packs that rows outside them still depend on must not change
        // depth, so they are re-encoded without a delta.
        let pinned: HashSet<ObjectId> = {
            let mut st = self.db.prepare(
                "SELECT DISTINCT o.base FROM objects o JOIN objects b ON b.id=o.base
                 WHERE o.pack NOT IN (SELECT id FROM retiring) AND b.pack IN (SELECT id FROM retiring)",
            )?;
            let rows: Vec<Vec<u8>> = st
                .query_map([], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            rows.iter()
                .map(|b| ObjectId::from_slice(b))
                .collect::<Result<_>>()?
        };
        let mut pending: Vec<Pending> = Vec::new();
        let mut lineage = Lineage::default();
        for (id, hint) in items {
            self.rewrite(
                id,
                hint.as_ref(),
                &pinned,
                opts,
                &mut pending,
                &mut lineage,
                &mut stats,
            )?;
        }
        for b in &pinned {
            self.rewrite(
                b,
                None,
                &pinned,
                opts,
                &mut pending,
                &mut lineage,
                &mut stats,
            )?;
        }
        self.write_groups(pending, opts, &mut stats)?;
        let (dropped, dropped_bytes): (i64, i64) = self.db.query_row(
            "SELECT count(*), COALESCE(sum(logical_len),0) FROM objects WHERE pack IN (SELECT id FROM retiring)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        stats.dropped = dropped as u64;
        stats.dropped_logical_bytes = dropped_bytes as u64;
        self.db.execute(
            "DELETE FROM objects WHERE pack IN (SELECT id FROM retiring)",
            [],
        )?;
        self.db.execute(
            "UPDATE packs SET retired_at=? WHERE id IN (SELECT id FROM retiring)",
            [now()],
        )?;
        self.groups.borrow_mut().clear();
        self.flush()?;
        Ok(stats)
    }

    fn is_retiring(&self, pack: i64) -> Result<bool> {
        Ok(self
            .db
            .query_row("SELECT 1 FROM retiring WHERE id=?", [pack], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Moves one live object out of a retiring pack (no-op if it is elsewhere).
    fn rewrite(
        &mut self,
        id: &ObjectId,
        hint: Option<&ObjectId>,
        pinned: &HashSet<ObjectId>,
        opts: &RepackOptions,
        pending: &mut Vec<Pending>,
        lineage: &mut Lineage,
        stats: &mut RepackStats,
    ) -> Result<()> {
        let row = match self.lookup(id)? {
            Some(r) if self.is_retiring(r.pack)? => r,
            _ => return Ok(()),
        };
        if row.logical_len as usize <= opts.group_object_max {
            // Only metadata is queued; the bytes are read again when the group is written, so
            // memory does not grow with the amount of small-object data.
            pending.push((*id, row.ty, row.logical_len as u32, hint.copied()));
            return Ok(());
        }
        let (ty, data) = self.get(id)?;
        let retiring: HashSet<i64> = {
            let mut st = self.db.prepare("SELECT id FROM retiring")?;
            let v: Vec<i64> = st
                .query_map([], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            v.into_iter().collect()
        };
        let allow_delta = !pinned.contains(id);
        // Versions of one thing form a lineage. Version k deltas against version k - lowbit(k)
        // (a skip-delta tree): chain depth stays logarithmic and each delta spans few edits,
        // which a plain "previous version" chain cannot do under the depth limit.
        let root = hint
            .and_then(|h| lineage.root_of.get(h).copied())
            .unwrap_or(*id);
        let chain = lineage.chains.entry(root).or_default();
        let k = chain.len();
        let mut cands: Vec<ObjectId> = Vec::new();
        if k >= 2 && self.opts.max_delta_depth <= 8 {
            cands.push(chain[k - (k & k.wrapping_neg())]);
        }
        cands.extend(hint.copied());
        chain.push(*id);
        lineage.root_of.insert(*id, root);
        let (enc, base, payload, depth) = self.encode_single(
            ty,
            &data,
            &cands,
            opts.level,
            opts.delta_level,
            allow_delta,
            &|r: &Row| !retiring.contains(&r.pack),
        )?;
        if enc == Encoding::Delta {
            stats.new_deltas += 1;
        }
        let _ = row;
        self.append(id, ty, enc, base, data.len() as u64, depth, &payload, true)?;
        stats.moved += 1;
        Ok(())
    }

    /// Orders small objects so versions of the same thing are adjacent (so one zstd window
    /// sees both) and writes them in groups of about `opts.group_target` bytes.
    fn write_groups(
        &mut self,
        pending: Vec<Pending>,
        opts: &RepackOptions,
        stats: &mut RepackStats,
    ) -> Result<()> {
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let pending: Vec<_> = pending.into_iter().filter(|p| seen.insert(p.0)).collect();
        let index: HashMap<ObjectId, usize> =
            pending.iter().enumerate().map(|(i, p)| (p.0, i)).collect();
        let mut root: Vec<usize> = Vec::with_capacity(pending.len());
        for (i, p) in pending.iter().enumerate() {
            let r = match p.3.and_then(|h| index.get(&h).copied()) {
                Some(hi) if hi < i => root[hi],
                _ => i,
            };
            root.push(r);
        }
        let mut order: Vec<usize> = (0..pending.len()).collect();
        // Group by type first (trees with trees, commits with commits, chunks with chunks),
        // then by version chain.
        order.sort_by_key(|&i| (pending[i].1 as u8, root[i], i));
        let mut batch: Vec<(ObjectId, ObjectType)> = Vec::new();
        let mut batch_bytes = 0usize;
        let flush = |s: &mut Store,
                     batch: &mut Vec<(ObjectId, ObjectType)>,
                     stats: &mut RepackStats|
         -> Result<()> {
            if batch.is_empty() {
                return Ok(());
            }
            let mut members: Vec<(ObjectId, ObjectType, Vec<u8>)> = Vec::with_capacity(batch.len());
            for (id, ty) in batch.drain(..) {
                members.push((id, ty, s.get(&id)?.1));
            }
            if members.len() > 1 && s.append_group(&members, opts.level)? {
                stats.groups += 1;
                stats.grouped_objects += members.len() as u64;
                stats.moved += members.len() as u64;
            } else {
                for (id, ty, d) in members {
                    let (enc, base, payload, depth) =
                        s.encode_single(ty, &d, &[], opts.level, opts.delta_level, false, &|_| {
                            false
                        })?;
                    s.append(&id, ty, enc, base, d.len() as u64, depth, &payload, true)?;
                    stats.moved += 1;
                }
            }
            Ok(())
        };
        for i in order {
            let (id, ty, len, _) = pending[i];
            batch_bytes += len as usize;
            batch.push((id, ty));
            if batch_bytes >= opts.group_target || batch.len() >= MAX_GROUP_MEMBERS {
                flush(self, &mut batch, stats)?;
                batch_bytes = 0;
            }
        }
        flush(self, &mut batch, stats)?;
        Ok(())
    }

    /// Deletes retired packs (and set-aside files) older than `grace_secs`.
    pub fn prune(&mut self, grace_secs: u64) -> Result<u64> {
        self.flush()?;
        let cutoff = now() - grace_secs as i64;
        let mut freed = 0;
        let mut st = self
            .db
            .prepare("SELECT id FROM packs WHERE retired_at IS NOT NULL AND retired_at <= ?")?;
        let ids: Vec<i64> = st
            .query_map([cutoff], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        for id in ids {
            let p = pack_path(&self.dir, id);
            if let Ok(m) = fs::metadata(&p) {
                freed += m.len();
                fs::remove_file(&p)?;
            }
            self.readers.borrow_mut().remove(&id);
            self.db.execute("DELETE FROM packs WHERE id=?", [id])?;
        }
        for e in fs::read_dir(self.dir.join(OBJECTS_DIR))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".orphan") || name.contains(".tail-") {
                let old = e
                    .metadata()?
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64 <= cutoff)
                    .unwrap_or(false);
                if old {
                    freed += e.metadata()?.len();
                    fs::remove_file(e.path())?;
                }
            }
        }
        Ok(freed)
    }

    /// Total bytes of every file under the store directory.
    pub fn disk_usage(&self) -> Result<u64> {
        fn walk(p: &Path) -> std::io::Result<u64> {
            let mut t = 0;
            for e in fs::read_dir(p)? {
                let e = e?;
                let m = e.metadata()?;
                t += if m.is_dir() {
                    walk(&e.path())?
                } else {
                    m.len()
                };
            }
            Ok(t)
        }
        Ok(walk(&self.dir)?)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Uncommitted index rows are discarded; pack bytes already written remain and
        // are re-indexed on the next open only if they were flushed (fsynced).
        if self.in_txn.get() {
            let _ = self.db.execute_batch("ROLLBACK");
        }
    }
}

fn tempfile_placeholder() -> Result<File> {
    #[cfg(unix)]
    let f = File::open("/dev/null")?;
    #[cfg(windows)]
    let f = File::open("NUL")?;
    Ok(f)
}
