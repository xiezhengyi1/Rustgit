//! The hosted project: one bare repository, one writer, durable publication.
//!
//! Everything that changes the repository happens in [`Host::update_ref`] (and the
//! directory-level helpers) while holding the repository mutex, so writes are serialized.
//! The order inside `update_ref` is the durability contract:
//!
//! 1. check authorization, policy, idempotency key, and the expected head/generation;
//! 2. verify the new head's dependency closure against staged + stored objects;
//! 3. publish: put staged objects into the pack store and `flush` (pack fsync, then index
//!    commit);
//! 4. enforce the fast-forward rule;
//! 5. compare-and-swap the ref (expected head **and** generation) in one SQLite
//!    transaction that also records the idempotency key;
//! 6. only then does the caller send the acknowledgement.
//!
//! Crash after 2/3: staged data remains, store may hold unreferenced objects; a retry
//! repeats the work. Crash after 3, before 5: objects are durable but unreferenced; a
//! retry finds them present. Crash after 5, before the reply: the idempotency key makes
//! the retry return success with `already_applied`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use rusqlite::{params, Connection};
use rustgit_core::objects::references;
use rustgit_core::repo::validate_ref_name;
use rustgit_core::Repo;
use rustgit_protocol::failpoint;
use rustgit_protocol::{
    Detail, ErrorCode, RefRecord, SessionId, UpdateRefReq, WireError, WireObject,
};
use rustgit_store::{hash_object, ObjectId, ObjectType};

use crate::auth::{Authorizer, Op};
use crate::config::HostConfig;
use crate::staging::Staging;

pub const HOSTED_MARKER: &str = "HOSTED";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Core(#[from] rustgit_core::Error),
    #[error("{0}")]
    Store(#[from] rustgit_store::Error),
    #[error("host database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0} is already being served by another process")]
    Locked(PathBuf),
    #[error("{0} is not a hosted repository (create one with `rgit serve --init`)")]
    NotHosted(PathBuf),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn is_no_space(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull
        || matches!(e.raw_os_error(), Some(28) | Some(112) | Some(122))
}

pub(crate) fn io_wire(e: &io::Error) -> WireError {
    if is_no_space(e) {
        WireError::new(
            ErrorCode::OutOfSpace,
            "the host has run out of disk space; previously acknowledged history is intact",
        )
    } else {
        WireError::new(ErrorCode::Internal, format!("host i/o error: {e}"))
    }
}

pub(crate) fn store_wire(e: &rustgit_store::Error) -> WireError {
    match e {
        rustgit_store::Error::Io(io) => io_wire(io),
        rustgit_store::Error::Sql(s) if s.to_string().contains("disk is full") => WireError::new(
            ErrorCode::OutOfSpace,
            "the host has run out of disk space; previously acknowledged history is intact",
        ),
        other => WireError::new(ErrorCode::Internal, format!("host storage error: {other}")),
    }
}

pub(crate) fn internal(e: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::Internal, e.to_string())
}

fn core_wire(e: rustgit_core::Error) -> WireError {
    match e {
        rustgit_core::Error::Store(s) => store_wire(&s),
        rustgit_core::Error::Io(i) => io_wire(&i),
        other => internal(other),
    }
}

pub(crate) struct SessionState {
    pub owner: u64,
    pub device: String,
    pub objects: u64,
    pub bytes: u64,
}

#[derive(Default)]
pub(crate) struct ConnCounts {
    pub total: usize,
    pub per_device: HashMap<String, usize>,
}

pub struct Host {
    pub cfg: HostConfig,
    pub(crate) dir: PathBuf,
    repo: Mutex<Repo>,
    db: Mutex<Connection>,
    pub staging: Staging,
    pub(crate) uuid: String,
    pub(crate) default_branch: String,
    pub(crate) auth: Arc<dyn Authorizer>,
    seq: Mutex<u64>,
    seq_cv: Condvar,
    pub(crate) sessions: Mutex<HashMap<SessionId, SessionState>>,
    pub(crate) conns: Mutex<ConnCounts>,
    /// Connections that have not finished the handshake and `Hello` yet.
    pub(crate) preauth: std::sync::atomic::AtomicUsize,
    pub(crate) shutdown: AtomicBool,
    /// Running total of staged bytes (approximate: re-scanned at start-up).
    staged_total: std::sync::atomic::AtomicU64,
    _lock: File,
}

/// Result of closure verification.
struct Closure {
    /// Staged objects to publish, dependencies before dependents.
    publish: Vec<(ObjectType, ObjectId)>,
    /// Commits/trees/manifests whose complete closure is now known to be present.
    mark: Vec<ObjectId>,
}

impl Host {
    /// Creates an empty hosted repository in `dir` (which must not contain a project).
    pub fn init(dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let repo = Repo::init(&fs::canonicalize(dir)?, "main")?;
        fs::write(
            repo.root.join(".rgit").join(HOSTED_MARKER),
            "{\"hosted\":1}\n",
        )?;
        Ok(())
    }

    pub fn is_hosted(dir: &Path) -> bool {
        dir.join(".rgit").join(HOSTED_MARKER).is_file()
    }

    /// Opens a hosted repository for serving and takes its exclusive lock. A second process
    /// (or a second `open` in this one) fails with [`Error::Locked`].
    pub fn open(dir: &Path, cfg: HostConfig, auth: Arc<dyn Authorizer>) -> Result<Arc<Host>> {
        let dir = fs::canonicalize(dir)?;
        if !Self::is_hosted(&dir) {
            return Err(Error::NotHosted(dir));
        }
        let hostdir = dir.join(".rgit").join("host");
        fs::create_dir_all(&hostdir)?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(hostdir.join("LOCK"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(Error::Locked(dir)),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        let repo = Repo::open(&dir)?;
        let uuid = repo.uuid()?;
        let default_branch = repo.current_branch()?;
        let db = Connection::open(hostdir.join("host.sqlite"))?;
        db.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS closed(id BLOB PRIMARY KEY) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )?;
        // `closed` records objects whose dependencies were all present when it was written.
        // A garbage collection can later drop part of an unreachable closed subtree, so the
        // set is discarded whenever the repository's maintenance epoch has moved on, and
        // rebuilt from the ref tips below.
        let epoch = repo.get_config("maintenance.epoch")?.unwrap_or_else(|| "0".into());
        let saved: Option<String> = db
            .query_row("SELECT v FROM kv WHERE k='epoch'", [], |r| r.get(0))
            .ok();
        if saved.as_deref() != Some(epoch.as_str()) {
            db.execute("DELETE FROM closed", [])?;
            db.execute(
                "INSERT OR REPLACE INTO kv(k, v) VALUES('epoch', ?)",
                [&epoch],
            )?;
        }
        let staging = Staging::open(hostdir.join("staging"))?;
        let seq = Mutex::new(1);
        let host = Arc::new(Host {
            cfg,
            dir,
            repo: Mutex::new(repo),
            db: Mutex::new(db),
            staging,
            uuid,
            default_branch,
            auth,
            seq,
            seq_cv: Condvar::new(),
            sessions: Mutex::new(HashMap::new()),
            conns: Mutex::new(ConnCounts::default()),
            preauth: std::sync::atomic::AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
            staged_total: std::sync::atomic::AtomicU64::new(0),
            _lock: lock,
        });
        host.sweep_staging();
        host.staged_total.store(
            host.staging.scan_total_bytes(),
            std::sync::atomic::Ordering::SeqCst,
        );
        host.ensure_closed();
        Ok(host)
    }

    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// How many objects are currently recorded as complete (for tests and diagnostics).
    pub fn closed_count(&self) -> u64 {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT COUNT(*) FROM closed", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn log(&self, msg: impl AsRef<str>) {
        if self.cfg.log {
            eprintln!("[host] {}", msg.as_ref());
        }
    }

    /// The repository, for reads and for tests. Holding the guard blocks all writers.
    pub fn repo(&self) -> MutexGuard<'_, Repo> {
        self.repo.lock().unwrap()
    }

    pub(crate) fn poisoned(&self) -> bool {
        self.repo.is_poisoned()
    }

    // ---- ref change notification --------------------------------------------------------

    pub fn ref_seq(&self) -> u64 {
        *self.seq.lock().unwrap()
    }

    fn bump_seq(&self) {
        *self.seq.lock().unwrap() += 1;
        self.seq_cv.notify_all();
    }

    /// Waits until the sequence exceeds `since`, the timeout passes, or the host stops.
    pub fn wait_seq(&self, since: u64, timeout: std::time::Duration) -> (u64, bool) {
        let deadline = std::time::Instant::now() + timeout;
        let mut g = self.seq.lock().unwrap();
        loop {
            if *g > since {
                return (*g, true);
            }
            let now = std::time::Instant::now();
            if now >= deadline || self.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                return (*g, false);
            }
            let slice = (deadline - now).min(std::time::Duration::from_millis(250));
            g = self.seq_cv.wait_timeout(g, slice).unwrap().0;
        }
    }

    // ---- closed set ---------------------------------------------------------------------

    fn is_closed(&self, id: &ObjectId) -> bool {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT 1 FROM closed WHERE id=?", [&id.0[..]], |_| Ok(()))
            .is_ok()
    }

    fn mark_closed(&self, ids: &[ObjectId]) -> std::result::Result<(), WireError> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction().map_err(internal)?;
        {
            let mut st = tx
                .prepare("INSERT OR IGNORE INTO closed(id) VALUES(?)")
                .map_err(internal)?;
            for id in ids {
                st.execute(params![&id.0[..]]).map_err(internal)?;
            }
        }
        tx.commit().map_err(internal)
    }

    /// An object counts as complete on this host when it is stored and everything it
    /// depends on is stored: chunks trivially, other types once `closed` says so.
    pub fn is_complete(&self, repo: &Repo, id: &ObjectId) -> bool {
        match repo.store.object_type(id) {
            Ok(Some(ObjectType::Chunk)) => true,
            Ok(Some(_)) => self.is_closed(id),
            _ => false,
        }
    }

    /// Verifies every ref tip's closure that is not yet recorded as closed (first start
    /// after hosting an existing repository, or after the host database was lost).
    fn ensure_closed(&self) {
        let repo = self.repo.lock().unwrap();
        let tips = repo.ref_records("heads/").unwrap_or_default();
        let session: SessionId = [0; 16];
        for (name, v) in tips {
            let Some(tip) = v.commit else { continue };
            if self.is_closed(&tip) && repo.store.has(&tip).unwrap_or(false) {
                continue;
            }
            match self.verify_closure(&repo, &session, &tip) {
                Ok(c) if c.publish.is_empty() => {
                    if let Err(e) = self.mark_closed(&c.mark) {
                        self.log(format!("cannot record closure of {name}: {e}"));
                    }
                }
                Ok(_) | Err(_) => self.log(format!(
                    "WARNING: {name} points at history that is incomplete in this repository"
                )),
            }
        }
    }

    pub fn sweep_staging(&self) -> usize {
        let sessions = self.sessions.lock().unwrap();
        let live: HashSet<String> = sessions
            .keys()
            .map(|k| crate::staging::hex(k))
            .collect();
        drop(sessions);
        let n = self
            .staging
            .sweep(self.cfg.staging_ttl, &|n| live.contains(n));
        if n > 0 {
            self.staged_total.store(
                self.staging.scan_total_bytes(),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        n
    }

    fn staged_total(&self) -> u64 {
        self.staged_total.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Deletes a staging session and keeps the running total in step.
    pub(crate) fn discard_session(&self, s: &SessionId) {
        let freed = self.staging.discard(s);
        use std::sync::atomic::Ordering::SeqCst;
        let mut cur = self.staged_total.load(SeqCst);
        loop {
            match self.staged_total.compare_exchange(cur, cur.saturating_sub(freed), SeqCst, SeqCst) {
                Ok(_) => break,
                Err(now) => cur = now,
            }
        }
    }

    // ---- sessions and connection accounting --------------------------------------------

    pub(crate) fn claim_session(
        &self,
        conn: u64,
        device: &str,
        s: SessionId,
        staged: (u64, u64),
    ) -> std::result::Result<(), WireError> {
        let mut m = self.sessions.lock().unwrap();
        if let Some(st) = m.get(&s) {
            if st.owner != conn {
                return Err(WireError::new(
                    ErrorCode::Busy,
                    "this upload session is in use by another connection; retry shortly",
                ));
            }
            return Ok(());
        }
        let open = m.values().filter(|x| x.device == device).count();
        if open >= self.cfg.max_open_sessions_per_device {
            return Err(WireError::new(
                ErrorCode::Busy,
                format!(
                    "device '{device}' already has {open} open upload sessions (limit {})",
                    self.cfg.max_open_sessions_per_device
                ),
            ));
        }
        m.insert(
            s,
            SessionState {
                owner: conn,
                device: device.to_string(),
                objects: staged.0,
                bytes: staged.1,
            },
        );
        Ok(())
    }

    pub(crate) fn release_conn_sessions(&self, conn: u64) {
        self.sessions.lock().unwrap().retain(|_, v| v.owner != conn);
    }

    pub(crate) fn release_session(&self, s: &SessionId) {
        self.sessions.lock().unwrap().remove(s);
    }

    // ---- reads --------------------------------------------------------------------------

    pub fn list_refs(&self, prefix: &str) -> std::result::Result<Vec<RefRecord>, WireError> {
        let repo = self.repo.lock().unwrap();
        Ok(repo
            .ref_records(prefix)
            .map_err(core_wire)?
            .into_iter()
            .map(|(name, v)| RefRecord {
                name,
                commit: v.commit,
                generation: v.generation,
            })
            .collect())
    }

    /// Haves: complete on the host, staged in `session`, or missing.
    pub fn have(
        &self,
        session: Option<&SessionId>,
        ids: &[ObjectId],
    ) -> Vec<rustgit_protocol::HaveState> {
        use rustgit_protocol::HaveState;
        let repo = self.repo.lock().unwrap();
        ids.iter()
            .map(|id| {
                if self.is_complete(&repo, id) {
                    HaveState::Complete
                } else if session.is_some_and(|s| self.staging.has(s, id)) {
                    HaveState::Staged
                } else {
                    HaveState::Missing
                }
            })
            .collect()
    }

    /// Computes the objects a client holding `haves` needs to have `wants` complete.
    pub fn plan_fetch(
        &self,
        wants: &[ObjectId],
        haves: &[ObjectId],
    ) -> std::result::Result<(Vec<rustgit_protocol::plan::PlanObject>, u64), WireError> {
        use rustgit_protocol::plan;
        let repo = self.repo.lock().unwrap();
        for w in wants {
            if repo.store.object_type(w).ok().flatten() != Some(ObjectType::Commit)
                || !self.is_complete(&repo, w)
            {
                return Err(WireError::new(
                    ErrorCode::NotFound,
                    format!("commit {} is not available on this host", w.short()),
                ));
            }
        }
        let common: Vec<ObjectId> = haves
            .iter()
            .filter(|h| {
                repo.store.object_type(h).ok().flatten() == Some(ObjectType::Commit)
                    && self.is_complete(&repo, h)
            })
            .copied()
            .collect();
        let to_wire = |e: rustgit_protocol::Error| internal(e);
        let exclude = plan::reachable_commits(&repo, &common).map_err(to_wire)?;
        let commits = plan::new_commits(&repo, wants, &exclude).map_err(to_wire)?;
        let objects = plan::enumerate_hinted(&repo, &commits).map_err(to_wire)?;
        if objects.len() as u64 > self.cfg.max_plan_objects {
            return Err(WireError::new(
                ErrorCode::QuotaExceeded,
                format!(
                    "fetch would enumerate {} objects (limit {}); fetch in smaller steps",
                    objects.len(),
                    self.cfg.max_plan_objects
                ),
            )
            .with_detail(Detail::Limit {
                what: "plan objects".into(),
                limit: self.cfg.max_plan_objects,
            }));
        }
        let n = commits.len() as u64;
        Ok((objects, n))
    }

    /// Reads objects for `Get`, stopping when the batch limits are reached. With `hints`
    /// (object -> similar object the client is known to hold) an object is sent as a delta
    /// when that is clearly smaller.
    pub fn get_objects(
        &self,
        ids: &[ObjectId],
        hints: Option<&std::collections::HashMap<ObjectId, ObjectId>>,
    ) -> std::result::Result<(Vec<WireObject>, u32), WireError> {
        let lim = &self.cfg.limits;
        let repo = self.repo.lock().unwrap();
        let mut out = Vec::new();
        let mut bytes = 0u64;
        for (i, id) in ids.iter().enumerate() {
            if out.len() as u32 >= lim.max_batch_objects {
                return Ok((out, (ids.len() - i) as u32));
            }
            let len = repo
                .store
                .logical_len(id)
                .map_err(|e| store_wire(&e))?
                .ok_or_else(|| {
                    WireError::new(ErrorCode::NotFound, format!("object {id} not found"))
                })?;
            if len > lim.max_object as u64 {
                return Err(WireError::new(
                    ErrorCode::ObjectTooLarge,
                    format!("object {id} is {len} bytes, above the transfer limit"),
                ));
            }
            if !out.is_empty() && bytes + len > lim.max_batch_bytes as u64 {
                return Ok((out, (ids.len() - i) as u32));
            }
            let (ty, data) = repo.store.get(id).map_err(|e| store_wire(&e))?;
            let mut wo = None;
            if let Some(h) = hints.and_then(|m| m.get(id)) {
                if let Ok((_, base)) = repo.store.get(h) {
                    wo = WireObject::delta(ty, *id, &data, *h, &base);
                }
            }
            let wo = wo.unwrap_or_else(|| WireObject::full(ty, *id, data));
            bytes += wo.data.len() as u64;
            out.push(wo);
        }
        Ok((out, 0))
    }

    // ---- uploads ------------------------------------------------------------------------

    /// Checks everything that can be checked before any byte is uploaded.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_checks(
        &self,
        device: &str,
        credential: &[u8],
        ref_name: &str,
        expected: Option<ObjectId>,
        expected_gen: u64,
        force: bool,
        object_count: u64,
        byte_estimate: u64,
        staged_bytes_so_far: u64,
    ) -> std::result::Result<(), WireError> {
        self.auth
            .authorize(device, credential, Op::Push { ref_name })?;
        if force {
            self.auth
                .authorize(device, credential, Op::Force { ref_name })?;
        }
        self.check_ref_name(ref_name)?;
        if force && !self.cfg.allow_force {
            return Err(WireError::new(
                ErrorCode::ForcePushDisabled,
                "force push is disabled on this host",
            ));
        }
        {
            let repo = self.repo.lock().unwrap();
            let cur = repo.get_ref(ref_name).map_err(core_wire)?;
            let (c, g) = cur
                .map(|v| (v.commit, v.generation))
                .unwrap_or((None, 0));
            if c != expected || g != expected_gen {
                return Err(stale(ref_name, c, g));
            }
        }
        let lim = &self.cfg.limits;
        let projected = staged_bytes_so_far.max(byte_estimate);
        if projected > self.cfg.max_session_bytes {
            return Err(quota("session bytes", self.cfg.max_session_bytes));
        }
        if object_count > 100_000_000 {
            return Err(quota("objects per push", 100_000_000));
        }
        let _ = lim;
        let total = self.staged_total();
        if total.saturating_add(byte_estimate.saturating_sub(staged_bytes_so_far))
            > self.cfg.max_staging_bytes
        {
            return Err(quota("staging bytes", self.cfg.max_staging_bytes));
        }
        if let Some(max) = self.cfg.max_repo_bytes {
            let used = self.repo.lock().unwrap().store.disk_usage().unwrap_or(0);
            if used.saturating_add(byte_estimate) > max {
                return Err(quota("repository bytes", max));
            }
        }
        Ok(())
    }

    pub(crate) fn check_ref_name(&self, ref_name: &str) -> std::result::Result<(), WireError> {
        if !ref_name.starts_with("heads/") || validate_ref_name(ref_name).is_err() {
            return Err(WireError::new(
                ErrorCode::InvalidRefName,
                format!("'{ref_name}' is not a valid branch ref (must be heads/<name>)"),
            ));
        }
        Ok(())
    }

    /// Verifies and stages one batch. All objects are verified before any is written, so a
    /// corrupt object rejects the whole batch. Returns (newly staged, already staged,
    /// bytes newly staged).
    pub fn stage_batch(
        &self,
        session: &SessionId,
        objects: &[WireObject],
        session_bytes_now: u64,
    ) -> std::result::Result<(u32, u32, u64), WireError> {
        let lim = &self.cfg.limits;
        // Reconstruct logical bytes (deltas are only a wire encoding), then verify each
        // object against the id the sender claimed.
        let mut resolved: Vec<(ObjectType, ObjectId, Vec<u8>)> = Vec::with_capacity(objects.len());
        let mut in_batch: HashMap<ObjectId, usize> = HashMap::new();
        let mut corrupt = Vec::new();
        for o in objects {
            let data = match o.base {
                None => {
                    if o.data.len() as u64 > lim.max_object as u64 {
                        return Err(WireError::new(
                            ErrorCode::ObjectTooLarge,
                            format!("object {} exceeds the {} byte limit", o.id, lim.max_object),
                        ));
                    }
                    o.data.clone()
                }
                Some(b) => {
                    let base = if let Some(&i) = in_batch.get(&b) {
                        Some(resolved[i].2.clone())
                    } else if let Ok(Some((_, d))) = self.staging.read(session, &b) {
                        Some(d)
                    } else {
                        let repo = self.repo.lock().unwrap();
                        repo.store.get(&b).ok().map(|(_, d)| d)
                    };
                    let Some(base) = base else {
                        return Err(WireError::new(
                            ErrorCode::UnknownBase,
                            format!("object {} is a delta against {b}, which this host does not have", o.id),
                        )
                        .with_detail(Detail::Ids(vec![b])));
                    };
                    match o.logical(Some(&base), lim.max_object as usize) {
                        Ok(d) => d,
                        Err(_) => {
                            corrupt.push(o.id);
                            Vec::new()
                        }
                    }
                }
            };
            if hash_object(o.ty, &data) != o.id && !corrupt.contains(&o.id) {
                corrupt.push(o.id);
            }
            in_batch.insert(o.id, resolved.len());
            resolved.push((o.ty, o.id, data));
        }
        if !corrupt.is_empty() {
            return Err(WireError::new(
                ErrorCode::CorruptObject,
                format!(
                    "{} object(s) do not match their ids; nothing from this batch was kept",
                    corrupt.len()
                ),
            )
            .with_detail(Detail::Ids(corrupt)));
        }
        for (ty, id, data) in &resolved {
            if *ty != ObjectType::Chunk {
                if let Err(e) = references(*ty, data) {
                    return Err(WireError::new(
                        ErrorCode::InvalidObject,
                        format!("object {id} is malformed: {e}"),
                    )
                    .with_detail(Detail::Ids(vec![*id])));
                }
            }
        }
        let new_bytes: u64 = resolved
            .iter()
            .filter(|(_, id, _)| !self.staging.has(session, id))
            .map(|(_, _, d)| d.len() as u64)
            .sum();
        if session_bytes_now + new_bytes > self.cfg.max_session_bytes {
            return Err(quota("session bytes", self.cfg.max_session_bytes));
        }
        if self.staged_total() + new_bytes > self.cfg.max_staging_bytes {
            return Err(quota("staging bytes", self.cfg.max_staging_bytes));
        }
        let (mut fresh, mut dup, mut bytes) = (0u32, 0u32, 0u64);
        for (ty, id, data) in &resolved {
            match self.staging.put(session, *ty, id, data) {
                Ok(true) => {
                    fresh += 1;
                    bytes += data.len() as u64;
                }
                Ok(false) => dup += 1,
                Err(e) => return Err(io_wire(&e)),
            }
            failpoint::hit("stage_object");
        }
        self.staging.commit_batch(session).map_err(|e| io_wire(&e))?;
        self.staged_total
            .fetch_add(bytes, std::sync::atomic::Ordering::SeqCst);
        Ok((fresh, dup, bytes))
    }

    // ---- publication and ref update -----------------------------------------------------

    fn applied_key(req: &UpdateRefReq) -> Vec<u8> {
        let mut h = blake3::Hasher::new_derive_key("rustgit protocol v1 applied update");
        h.update(&req.session);
        h.update(req.ref_name.as_bytes());
        h.update(&[0]);
        match &req.expected {
            Some(e) => h.update(&[1]).update(&e.0),
            None => h.update(&[0]),
        };
        h.update(&req.expected_gen.to_le_bytes());
        match &req.new {
            Some(n) => h.update(&[1]).update(&n.0),
            None => h.update(&[0]),
        };
        h.finalize().as_bytes().to_vec()
    }

    /// Applies a ref update. Returns `(generation, already_applied)`.
    pub fn update_ref(
        &self,
        device: &str,
        credential: &[u8],
        req: &UpdateRefReq,
    ) -> std::result::Result<(u64, bool), WireError> {
        let op = if req.new.is_none() {
            Op::Delete { ref_name: &req.ref_name }
        } else {
            Op::Push { ref_name: &req.ref_name }
        };
        self.auth.authorize(device, credential, op)?;
        if req.force {
            self.auth.authorize(
                device,
                credential,
                Op::Force { ref_name: &req.ref_name },
            )?;
        }
        self.check_ref_name(&req.ref_name)?;
        if req.force && !self.cfg.allow_force {
            return Err(WireError::new(
                ErrorCode::ForcePushDisabled,
                "force push is disabled on this host",
            ));
        }
        if req.new.is_none() && !self.cfg.allow_delete {
            return Err(WireError::new(
                ErrorCode::DeleteDisabled,
                "deleting branches is disabled on this host",
            ));
        }
        let key = Self::applied_key(req);

        // Serialized section: one writer at a time.
        let mut repo = self.repo.lock().unwrap();
        if let Some((name, commit, gen)) = repo.applied_update(&key).map_err(core_wire)? {
            if name == req.ref_name && commit == req.new {
                self.discard_session(&req.session);
                return Ok((gen, true));
            }
        }
        let cur = repo.get_ref(&req.ref_name).map_err(core_wire)?;
        let (cur_commit, cur_gen) = cur.map(|v| (v.commit, v.generation)).unwrap_or((None, 0));
        if cur_commit != req.expected || cur_gen != req.expected_gen {
            return Err(stale(&req.ref_name, cur_commit, cur_gen));
        }

        if req.new.is_none() && cur_commit.is_none() {
            return Err(WireError::new(
                ErrorCode::NotFound,
                format!("'{}' does not exist on this host", req.ref_name),
            ));
        }
        if let Some(new) = req.new {
            let closure = self.verify_closure(&repo, &req.session, &new)?;
            failpoint::hit("before_publish");
            self.publish(&mut repo, &req.session, &closure)?;
            failpoint::hit("after_publish");
            if let Some(c) = cur_commit {
                if !req.force {
                    let ok = repo.is_ancestor(&c, &new).map_err(core_wire)?;
                    if !ok {
                        return Err(WireError::new(
                            ErrorCode::NonFastForward,
                            format!(
                                "'{}' is at {}, which is not an ancestor of {}; fetch and merge first (force push is {})",
                                req.ref_name,
                                c.short(),
                                new.short(),
                                if self.cfg.allow_force { "allowed with --force" } else { "disabled" }
                            ),
                        )
                        .with_detail(Detail::Ref {
                            name: req.ref_name.clone(),
                            current: Some(c),
                            generation: cur_gen,
                        }));
                    }
                }
            }
            self.mark_closed(&closure.mark)?;
        }

        let gen = match repo.cas_ref_checked(
            &req.ref_name,
            req.expected,
            Some(req.expected_gen),
            req.new,
            Some(&key),
        ) {
            Ok(g) => g,
            Err(rustgit_core::Error::RefConflict { .. }) => {
                let now = repo.get_ref(&req.ref_name).map_err(core_wire)?;
                let (c, g) = now.map(|v| (v.commit, v.generation)).unwrap_or((None, 0));
                return Err(stale(&req.ref_name, c, g));
            }
            Err(e) => return Err(core_wire(e)),
        };
        failpoint::hit("after_ref_update");
        drop(repo);
        self.bump_seq();
        self.discard_session(&req.session);
        Ok((gen, false))
    }

    fn publish(
        &self,
        repo: &mut Repo,
        session: &SessionId,
        c: &Closure,
    ) -> std::result::Result<(), WireError> {
        for (ty, id) in &c.publish {
            let (sty, data) = self
                .staging
                .read(session, id)
                .map_err(|e| io_wire(&e))?
                .ok_or_else(|| {
                    WireError::new(ErrorCode::MissingObjects, "staged object vanished")
                        .with_detail(Detail::Ids(vec![*id]))
                })?;
            if sty != *ty {
                return Err(WireError::new(
                    ErrorCode::InvalidObject,
                    format!("object {id} has an unexpected type"),
                ));
            }
            let stored = repo.store.put(*ty, &data, None).map_err(|e| store_wire(&e))?;
            if stored != *id {
                // The staged file no longer matches its name: on-disk corruption.
                self.staging.remove_object(session, id);
                return Err(WireError::new(
                    ErrorCode::CorruptObject,
                    format!("staged object {id} was damaged on the host; resend it"),
                )
                .with_detail(Detail::Ids(vec![*id])));
            }
        }
        failpoint::hit("mid_publish");
        repo.store.flush().map_err(|e| store_wire(&e))?;
        Ok(())
    }

    /// Walks the dependency graph from `root`, reading staged and stored objects, and fails
    /// with `MissingObjects` if anything is absent. Objects already recorded as closed (and
    /// still stored) end a branch of the walk.
    fn verify_closure(
        &self,
        repo: &Repo,
        session: &SessionId,
        root: &ObjectId,
    ) -> std::result::Result<Closure, WireError> {
        let mut visited: HashSet<ObjectId> = HashSet::new();
        let mut missing: Vec<ObjectId> = Vec::new();
        let mut publish = Vec::new();
        let mut mark = Vec::new();
        let mut staged_nodes: HashSet<ObjectId> = HashSet::new();
        let mut stack: Vec<(ObjectType, ObjectId, bool)> = vec![(ObjectType::Commit, *root, false)];
        while let Some((ty, id, expanded)) = stack.pop() {
            if expanded {
                mark.push(id);
                if staged_nodes.contains(&id) {
                    publish.push((ty, id));
                }
                continue;
            }
            if !visited.insert(id) {
                continue;
            }
            let stored = repo.store.object_type(&id).map_err(|e| store_wire(&e))?;
            if let Some(sty) = stored {
                if sty != ty {
                    return Err(type_mismatch(&id));
                }
                if ty == ObjectType::Chunk || self.is_closed(&id) {
                    continue;
                }
            } else if ty == ObjectType::Chunk {
                match self.staging.staged_type(session, &id).map_err(|e| io_wire(&e))? {
                    Some(ObjectType::Chunk) => publish.push((ty, id)),
                    Some(_) => return Err(type_mismatch(&id)),
                    None => missing.push(id),
                }
                if missing.len() >= 1024 {
                    break;
                }
                continue;
            }
            // Commit, tree or manifest that is not yet known to be closed.
            let (staged, data) = if stored.is_some() {
                let (_, d) = repo.store.get(&id).map_err(|e| store_wire(&e))?;
                (false, d)
            } else {
                match self.staging.read(session, &id).map_err(|e| io_wire(&e))? {
                    Some((sty, d)) => {
                        if sty != ty {
                            return Err(type_mismatch(&id));
                        }
                        if hash_object(ty, &d) != id {
                            self.staging.remove_object(session, &id);
                            return Err(WireError::new(
                                ErrorCode::CorruptObject,
                                format!("staged object {id} was damaged on the host; resend it"),
                            )
                            .with_detail(Detail::Ids(vec![id])));
                        }
                        (true, d)
                    }
                    None => {
                        missing.push(id);
                        if missing.len() >= 1024 {
                            break;
                        }
                        continue;
                    }
                }
            };
            let children = references(ty, &data).map_err(|e| {
                WireError::new(
                    ErrorCode::InvalidObject,
                    format!("object {id} is malformed: {e}"),
                )
                .with_detail(Detail::Ids(vec![id]))
            })?;
            // Marker entry: runs after all children.
            stack.push((ty, id, true));
            if staged {
                staged_nodes.insert(id);
            }
            for (cty, cid) in children {
                if !visited.contains(&cid) {
                    stack.push((cty, cid, false));
                }
            }
        }
        if !missing.is_empty() {
            return Err(WireError::new(
                ErrorCode::MissingObjects,
                format!("{} object(s) the new head depends on are missing", missing.len()),
            )
            .with_detail(Detail::Ids(missing)));
        }
        Ok(Closure { publish, mark })
    }
}

fn type_mismatch(id: &ObjectId) -> WireError {
    WireError::new(
        ErrorCode::InvalidObject,
        format!("object {id} is not of the type its parent says"),
    )
    .with_detail(Detail::Ids(vec![*id]))
}

pub(crate) fn stale(name: &str, c: Option<ObjectId>, g: u64) -> WireError {
    WireError::new(
        ErrorCode::StaleRef,
        format!(
            "'{name}' changed on the host (now {}, generation {g}); fetch and merge, then push again. Your local commits are untouched.",
            c.map(|c| c.short()).unwrap_or_else(|| "absent".into())
        ),
    )
    .with_detail(Detail::Ref {
        name: name.to_string(),
        current: c,
        generation: g,
    })
}

pub(crate) fn quota(what: &str, limit: u64) -> WireError {
    WireError::new(
        ErrorCode::QuotaExceeded,
        format!("{what} quota exceeded (limit {limit})"),
    )
    .with_detail(Detail::Limit {
        what: what.to_string(),
        limit,
    })
}

/// Takes the host's exclusive lock for offline maintenance (`gc`, `reindex`) so it cannot
/// run while a `serve` process is writing. Fails with [`Error::Locked`] if one is.
pub fn maintenance_lock(dir: &Path) -> Result<File> {
    let hostdir = dir.join(".rgit").join("host");
    fs::create_dir_all(&hostdir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(hostdir.join("LOCK"))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::Locked(dir.to_path_buf())),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}
