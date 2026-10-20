//! The client side of the protocol: connect, list refs, push, fetch, watch, clone.
//!
//! A [`Client`] drives one connection through the state machine described in
//! docs/protocol.md. It never changes local state in a way that could lose work: fetch adds
//! objects and moves only `remotes/<remote>/...` refs; push moves nothing local except the
//! remote-tracking ref after the host has acknowledged durably.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rustgit_core::objects::references;
use rustgit_core::{Flat, Repo};
use rustgit_store::{ObjectId, ObjectType};

use crate::error::{Detail, Error, ErrorCode, Result, WireError};
use crate::failpoint;
use crate::frame::Channel;
use crate::msg::*;
use crate::identity::{DeviceKey, Fingerprint};
use crate::plan;
use crate::secure::{connect_secure, HostAuth};
use crate::transport::Transport;

pub const PROTOCOL_MIN_VERSION: u16 = 1;
pub const PROTOCOL_MAX_VERSION: u16 = 1;

/// How the client protects the connection.
#[derive(Clone)]
pub enum Security {
    /// Plain, unauthenticated bytes. Only for loopback or hosts started with
    /// `--insecure-open-access`; anyone on the path can read and alter the traffic.
    Plaintext,
    /// Noise XX channel: the host must prove the pinned identity, and this device proves
    /// `identity`. Addresses and discovery are only hints; `host` is the trust anchor.
    Secure {
        identity: Arc<DeviceKey>,
        host: Fingerprint,
    },
}

#[derive(Clone)]
pub struct ClientConfig {
    pub device: String,
    pub security: Security,
    /// Repository UUID the caller expects the host to serve (pinned per remote).
    pub project: Option<String>,
    /// Encoded [`Credential`] (empty unless enrolling). Secret; never print it.
    pub credential: Vec<u8>,
    pub connect_timeout: Duration,
    /// Maximum stall inside a frame, and the wait for most replies.
    pub io_timeout: Duration,
    /// Wait for replies that need server-side work (Plan, UpdateRef).
    pub slow_call_timeout: Duration,
    pub max_retries: u32,
}

impl ClientConfig {
    pub fn secure(mut self, identity: Arc<DeviceKey>, host: Fingerprint) -> Self {
        self.security = Security::Secure { identity, host };
        self
    }

    /// Present an invitation's enrollment secret in `Hello`.
    pub fn enrolling(mut self, invitation: [u8; 16], secret: [u8; 32]) -> Self {
        self.credential = Credential::Enroll { invitation, secret }.encode();
        self
    }

    pub fn new(device: impl Into<String>) -> Self {
        ClientConfig {
            device: device.into(),
            security: Security::Plaintext,
            project: None,
            credential: Vec::new(),
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(30),
            slow_call_timeout: Duration::from_secs(300),
            max_retries: 4,
        }
    }
}

/// Shared flag a UI or signal handler sets to stop a transfer between batches.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, Default)]
pub struct TransferStats {
    /// Objects whose bytes were sent or received.
    pub objects_transferred: u64,
    /// Offered objects the receiver already had completely.
    pub objects_already_complete: u64,
    /// Objects a previous interrupted attempt had already staged on the host.
    pub objects_already_staged: u64,
    /// Of those, objects carried as a delta against a similar object the receiver holds.
    pub objects_as_delta: u64,
    /// Logical (uncompressed) bytes of the objects transferred.
    pub logical_bytes: u64,
    /// Bytes on the transport (framing and compression included) over this connection.
    pub wire_bytes_sent: u64,
    pub wire_bytes_received: u64,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
}

#[derive(Clone, Debug)]
pub struct PushOptions {
    /// Local branch to push.
    pub branch: String,
    /// Branch name on the host (defaults to the same).
    pub remote_branch: String,
    /// Remote name used for the remote-tracking ref.
    pub remote: String,
    pub force: bool,
}

#[derive(Clone, Debug)]
pub enum PushOutcome {
    UpToDate,
    Pushed {
        new_head: ObjectId,
        generation: u64,
        /// The host had already applied this exact update on an earlier attempt.
        already_applied: bool,
    },
}

#[derive(Clone, Debug)]
pub struct FetchedRef {
    pub branch: String,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
}

#[derive(Clone, Debug, Default)]
pub struct FetchOutcome {
    pub updated: Vec<FetchedRef>,
    pub up_to_date: bool,
}

type ProgressFn = Box<dyn FnMut(&Progress) + Send>;

pub struct Client {
    ch: Channel,
    pub hello: HelloOk,
    /// Set on the secure channel: the host identity proven during the handshake.
    pub host_auth: Option<HostAuth>,
    pub cancel: CancelToken,
    pub stats: TransferStats,
    cfg: ClientConfig,
    progress: Option<ProgressFn>,
    /// Cleared if the peer ever reports it cannot resolve a delta; everything is then whole.
    use_delta: bool,
}

pub fn session_id(device: &str, ref_name: &str, new_head: &ObjectId) -> SessionId {
    let mut h = blake3::Hasher::new_derive_key("rustgit protocol v1 upload session");
    h.update(device.as_bytes());
    h.update(&[0]);
    h.update(ref_name.as_bytes());
    h.update(&[0]);
    h.update(&new_head.0);
    h.finalize().as_bytes()[..16].try_into().unwrap()
}

fn wire(code: ErrorCode, msg: impl Into<String>) -> Error {
    Error::Remote(WireError::new(code, msg))
}

impl Client {
    pub fn connect(t: &dyn Transport, cfg: &ClientConfig, cancel: CancelToken) -> Result<Client> {
        let io = t.connect(cfg.connect_timeout)?;
        let (io, host_auth) = match &cfg.security {
            Security::Plaintext => (io, None),
            Security::Secure { identity, host } => {
                let (io, a) = connect_secure(io, identity, host, cfg.io_timeout)?;
                (io, Some(a))
            }
        };
        let mut ch = Channel::new(io, Some(cfg.io_timeout));
        let hello = Request::Hello(Hello {
            min_version: PROTOCOL_MIN_VERSION,
            max_version: PROTOCOL_MAX_VERSION,
            device: cfg.device.clone(),
            project: cfg.project.clone(),
            credential: cfg.credential.clone(),
        });
        ch.send(&hello.encode())?;
        let reply = Response::decode(&ch.recv(Some(cfg.io_timeout))?)?;
        let ok = match reply {
            Response::HelloOk(h) => h,
            Response::Error(w) => return Err(Error::Remote(w)),
            other => {
                return Err(Error::protocol(format!(
                    "expected HelloOk, got {}",
                    other.name()
                )))
            }
        };
        if ok.version < PROTOCOL_MIN_VERSION || ok.version > PROTOCOL_MAX_VERSION {
            return Err(Error::protocol(format!(
                "host chose unsupported protocol version {}",
                ok.version
            )));
        }
        ch.set_max_frame(ok.limits.max_frame);
        Ok(Client {
            ch,
            hello: ok,
            host_auth,
            cancel,
            stats: TransferStats::default(),
            cfg: cfg.clone(),
            progress: None,
            use_delta: true,
        })
    }

    pub fn on_progress(&mut self, f: impl FnMut(&Progress) + Send + 'static) {
        self.progress = Some(Box::new(f));
    }

    fn report(&mut self, phase: &'static str, done: u64, total: u64) {
        if let Some(f) = self.progress.as_mut() {
            f(&Progress { phase, done, total });
        }
    }

    pub fn wire_bytes(&self) -> (u64, u64) {
        self.ch.wire_counts()
    }

    fn sync_stats(&mut self) {
        (self.stats.wire_bytes_sent, self.stats.wire_bytes_received) = self.ch.wire_counts();
    }

    fn call_with(&mut self, req: &Request, wait: Duration) -> Result<Response> {
        self.ch.send(&req.encode())?;
        let resp = Response::decode(&self.ch.recv(Some(wait))?)?;
        self.sync_stats();
        match resp {
            Response::Error(w) => Err(Error::Remote(w)),
            r => Ok(r),
        }
    }

    fn call(&mut self, req: &Request) -> Result<Response> {
        let w = self.cfg.io_timeout * 2;
        self.call_with(req, w)
    }

    fn unexpected(&self, got: &Response, wanted: &str) -> Error {
        Error::protocol(format!("expected {wanted}, got {}", got.name()))
    }

    pub fn close(mut self) {
        let _ = self.ch.send(&Request::Bye.encode());
        self.ch.shutdown();
    }

    pub fn list_refs(&mut self, prefix: &str) -> Result<(Vec<RefRecord>, u64)> {
        match self.call(&Request::ListRefs {
            prefix: prefix.into(),
        })? {
            Response::Refs { records, seq } => Ok((records, seq)),
            r => Err(self.unexpected(&r, "Refs")),
        }
    }

    pub fn have(&mut self, session: Option<SessionId>, ids: &[ObjectId]) -> Result<Vec<HaveState>> {
        let mut out = Vec::with_capacity(ids.len());
        let step = self.hello.limits.max_ids_per_request.max(1) as usize;
        for chunk in ids.chunks(step) {
            match self.call(&Request::Have {
                session,
                ids: chunk.to_vec(),
            })? {
                Response::HaveResult(v) if v.len() == chunk.len() => out.extend(v),
                Response::HaveResult(_) => {
                    return Err(Error::protocol("HaveResult length does not match query"))
                }
                r => return Err(self.unexpected(&r, "HaveResult")),
            }
        }
        Ok(out)
    }

    /// Blocks until the host's ref sequence exceeds `since` or `timeout` passes.
    pub fn watch(&mut self, since: u64, timeout: Duration) -> Result<(u64, bool)> {
        let wait = timeout + self.cfg.io_timeout;
        match self.call_with(
            &Request::Watch {
                since,
                timeout_ms: timeout.as_millis().min(u32::MAX as u128) as u32,
            },
            wait,
        )? {
            Response::WatchResult { seq, changed } => Ok((seq, changed)),
            r => Err(self.unexpected(&r, "WatchResult")),
        }
    }

    // ------------------------------------------------------------------------------------
    // push
    // ------------------------------------------------------------------------------------

    /// Pushes a local branch. Fails without touching local history when the host's ref has
    /// moved (the error says fetch and merge are required).
    pub fn push(&mut self, repo: &mut Repo, opts: &PushOptions) -> Result<PushOutcome> {
        let ref_name = format!("heads/{}", opts.remote_branch);
        let head = repo
            .get_ref(&format!("heads/{}", opts.branch))?
            .and_then(|r| r.commit)
            .ok_or_else(|| Error::Other(format!("branch '{}' has no commits", opts.branch)))?;
        let (records, _) = self.list_refs("heads/")?;
        let remote = records.iter().find(|r| r.name == ref_name);
        let (expected, expected_gen) = match remote {
            Some(r) => (r.commit, r.generation),
            None => (None, 0),
        };
        let tracking = format!("remotes/{}/{}", opts.remote, opts.remote_branch);
        if expected == Some(head) {
            self.set_tracking(repo, &tracking, Some(head))?;
            return Ok(PushOutcome::UpToDate);
        }
        // Early local check so nothing is uploaded for a push that cannot succeed. The host
        // repeats the check and does not rely on this one.
        if let Some(rc) = expected {
            let has = repo.store.has(&rc)?;
            if (!has || !repo.is_ancestor(&rc, &head)?) && !opts.force {
                return Err(Error::Remote(
                    WireError::new(
                        ErrorCode::NonFastForward,
                        format!(
                            "the host's '{}' (at {}) has commits you do not have; run fetch and merge, then push again",
                            opts.remote_branch,
                            rc.short()
                        ),
                    )
                    .with_detail(Detail::Ref {
                        name: ref_name.clone(),
                        current: Some(rc),
                        generation: expected_gen,
                    }),
                ));
            }
        }

        // Which objects does the host lack? Start from commits the host is known to hold
        // completely (its ref tips that we also hold), then ask about what remains.
        let known_tips: Vec<ObjectId> = records
            .iter()
            .filter_map(|r| r.commit)
            .filter(|c| repo.store.has(c).unwrap_or(false))
            .collect();
        let exclude = plan::reachable_commits(repo, &known_tips)?;
        let commits = plan::new_commits(repo, &[head], &exclude)?;
        let objects = plan::enumerate_hinted(repo, &commits)?;
        let session = session_id(&self.cfg.device, &ref_name, &head);
        self.report("negotiating", 0, objects.len() as u64);
        let ids: Vec<ObjectId> = objects.iter().map(|o| o.id).collect();
        let states = self.have(Some(session), &ids)?;
        let mut to_send: Vec<plan::PlanObject> = Vec::new();
        for (o, st) in objects.iter().zip(&states) {
            match st {
                HaveState::Missing => to_send.push(*o),
                HaveState::Complete => self.stats.objects_already_complete += 1,
                HaveState::Staged => self.stats.objects_already_staged += 1,
            }
        }
        // Stable by type: chunks, manifests, trees, commits. Within a type the oldest-first
        // order of the plan is kept, which is what makes every delta hint arrive before use.
        to_send.sort_by_key(|o| o.ty as u8);
        let mut sizes = Vec::with_capacity(to_send.len());
        let mut total_bytes = 0u64;
        for id in to_send.iter().map(|o| &o.id) {
            let n = repo.store.logical_len(id)?.unwrap_or(0);
            if n > self.hello.limits.max_object as u64 {
                return Err(wire(
                    ErrorCode::ObjectTooLarge,
                    format!(
                        "object {} is {n} bytes; the host accepts at most {}",
                        id.short(),
                        self.hello.limits.max_object
                    ),
                ));
            }
            total_bytes += n;
            sizes.push(n);
        }

        self.call(&Request::Begin(BeginReq {
            session,
            ref_name: ref_name.clone(),
            expected,
            expected_gen,
            new_head: head,
            force: opts.force,
            object_count: to_send.len() as u64,
            byte_estimate: total_bytes,
        }))
        .and_then(|r| match r {
            Response::BeginOk { .. } => Ok(()),
            r => Err(self.unexpected(&r, "BeginOk")),
        })?;

        self.upload(repo, session, &to_send, &sizes)?;

        let mut attempts = 0;
        loop {
            let req = Request::UpdateRef(UpdateRefReq {
                session,
                ref_name: ref_name.clone(),
                expected,
                expected_gen,
                new: Some(head),
                force: opts.force,
            });
            let wait = self.cfg.slow_call_timeout;
            match self.call_with(&req, wait) {
                Ok(Response::RefUpdated {
                    generation,
                    already_applied,
                }) => {
                    self.set_tracking(repo, &tracking, Some(head))?;
                    return Ok(PushOutcome::Pushed {
                        new_head: head,
                        generation,
                        already_applied,
                    });
                }
                Ok(r) => return Err(self.unexpected(&r, "RefUpdated")),
                Err(Error::Remote(WireError {
                    code: ErrorCode::MissingObjects | ErrorCode::CorruptObject,
                    detail: Detail::Ids(ids),
                    ..
                })) if attempts < 3 => {
                    // The host's own check found objects our `Have` round believed present
                    // (for example after a crash mid-publish), or found a staged file
                    // damaged on its disk and dropped it. Send them again and ask again.
                    attempts += 1;
                    let mut extra = Vec::new();
                    let mut sz = Vec::new();
                    for id in ids {
                        let ty = repo.store.object_type(&id)?.ok_or_else(|| {
                            Error::Other(format!(
                                "host is missing {id}, which this repository does not have either"
                            ))
                        })?;
                        sz.push(repo.store.logical_len(&id)?.unwrap_or(0));
                        extra.push(plan::PlanObject { ty, id, hint: None });
                    }
                    self.upload(repo, session, &extra, &sz)?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn upload(
        &mut self,
        repo: &Repo,
        session: SessionId,
        items: &[plan::PlanObject],
        sizes: &[u64],
    ) -> Result<()> {
        let lim = self.hello.limits;
        let total = items.len() as u64;
        let mut done = 0u64;
        let mut idx: Vec<usize> = Vec::new();
        let mut batch_bytes = 0u64;
        for i in 0..=items.len() {
            let flush = if i == items.len() {
                !idx.is_empty()
            } else {
                !idx.is_empty()
                    && (idx.len() as u32 >= lim.max_batch_objects
                        || batch_bytes + sizes[i] > lim.max_batch_bytes as u64)
            };
            if flush {
                if self.cancel.is_cancelled() {
                    let _ = self.call(&Request::Cancel {
                        session,
                        discard: false,
                    });
                    return Err(Error::Cancelled);
                }
                self.send_batch(repo, session, &idx, items)?;
                done += idx.len() as u64;
                idx.clear();
                batch_bytes = 0;
                self.report("uploading", done, total);
                failpoint::hit("client_put_acked");
            }
            if i < items.len() {
                idx.push(i);
                batch_bytes += sizes[i];
            }
        }
        Ok(())
    }

    fn send_batch(
        &mut self,
        repo: &Repo,
        session: SessionId,
        idx: &[usize],
        items: &[plan::PlanObject],
    ) -> Result<()> {
        let mut with_delta = self.use_delta;
        loop {
            let mut objects = Vec::with_capacity(idx.len());
            let mut logical = 0u64;
            let mut deltas = 0u64;
            for &i in idx {
                let it = items[i];
                let (got_ty, data) = repo.store.get(&it.id)?;
                if got_ty != it.ty {
                    return Err(Error::Other(format!("object {} changed type locally", it.id)));
                }
                logical += data.len() as u64;
                let mut wo = None;
                if let (true, Some(h)) = (with_delta, it.hint) {
                    if let Ok((_, base)) = repo.store.get(&h) {
                        wo = WireObject::delta(it.ty, it.id, &data, h, &base);
                    }
                }
                match wo {
                    Some(w) => {
                        deltas += 1;
                        objects.push(w);
                    }
                    None => objects.push(WireObject::full(it.ty, it.id, data)),
                }
            }
            let n = objects.len() as u64;
            match self.call(&Request::Put { session, objects }) {
                Ok(Response::PutAck { .. }) => {
                    self.stats.objects_transferred += n;
                    self.stats.objects_as_delta += deltas;
                    self.stats.logical_bytes += logical;
                    return Ok(());
                }
                Ok(r) => return Err(self.unexpected(&r, "PutAck")),
                Err(Error::Remote(w)) if w.code == ErrorCode::UnknownBase && with_delta => {
                    // The host lacks a base we assumed it had: resend this batch whole and
                    // stop using deltas on this connection.
                    with_delta = false;
                    self.use_delta = false;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Deletes a branch on the host (leaves a tombstone). Hosts refuse by default.
    pub fn delete_remote_branch(&mut self, repo: &mut Repo, remote: &str, branch: &str) -> Result<u64> {
        let ref_name = format!("heads/{branch}");
        let (records, _) = self.list_refs("heads/")?;
        let rec = records
            .iter()
            .find(|r| r.name == ref_name && r.commit.is_some())
            .ok_or_else(|| wire(ErrorCode::NotFound, format!("host has no branch '{branch}'")))?;
        let req = Request::UpdateRef(UpdateRefReq {
            session: [0; 16],
            ref_name,
            expected: rec.commit,
            expected_gen: rec.generation,
            new: None,
            force: false,
        });
        match self.call(&req)? {
            Response::RefUpdated { generation, .. } => {
                self.set_tracking(repo, &format!("remotes/{remote}/{branch}"), None)?;
                Ok(generation)
            }
            r => Err(self.unexpected(&r, "RefUpdated")),
        }
    }

    fn set_tracking(&self, repo: &Repo, name: &str, to: Option<ObjectId>) -> Result<()> {
        for _ in 0..3 {
            let cur = repo.get_ref(name)?.and_then(|r| r.commit);
            if cur == to {
                return Ok(());
            }
            match repo.cas_ref(name, cur, to) {
                Ok(_) => return Ok(()),
                Err(rustgit_core::Error::RefConflict { .. }) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::Other(format!("could not update {name}")))
    }

    // ------------------------------------------------------------------------------------
    // fetch
    // ------------------------------------------------------------------------------------

    /// Downloads the objects behind the host's branches and moves `remotes/<remote>/<branch>`.
    /// Objects are made durable (flushed) before any tracking ref moves, so an interruption
    /// never leaves a ref pointing at missing history, and a rerun skips what already
    /// arrived.
    pub fn fetch(&mut self, repo: &mut Repo, remote: &str) -> Result<FetchOutcome> {
        let (records, _) = self.list_refs("heads/")?;
        let mut wants: Vec<ObjectId> = Vec::new();
        let mut todo: Vec<(String, Option<ObjectId>, Option<ObjectId>)> = Vec::new();
        for r in &records {
            let branch = r.name.strip_prefix("heads/").unwrap_or(&r.name).to_string();
            let tracking = format!("remotes/{remote}/{branch}");
            let old = repo.get_ref(&tracking)?.and_then(|x| x.commit);
            if old != r.commit {
                if let Some(c) = r.commit {
                    wants.push(c);
                }
                todo.push((branch, old, r.commit));
            }
        }
        if todo.is_empty() {
            return Ok(FetchOutcome {
                updated: vec![],
                up_to_date: true,
            });
        }
        wants.sort();
        wants.dedup();

        // Tips we hold completely are valid boundaries; add recent ancestors so the host can
        // find a common commit even when our tips are local-only.
        let mut tips: Vec<ObjectId> = Vec::new();
        for prefix in ["heads/", "remotes/", "tags/"] {
            tips.extend(repo.list_refs(prefix)?.into_iter().map(|(_, c)| c));
        }
        tips.sort();
        tips.dedup();
        let local_complete = plan::reachable_commits(repo, &tips)?;
        let haves = collect_haves(repo, &tips, 4096);

        if !wants.is_empty() {
            self.fetch_objects(repo, &wants, &haves)?;
            let new = plan::new_commits(repo, &wants, &local_complete)?;
            let missing = plan::verify_present(repo, &new).map_err(|e| {
                Error::Verification(format!("received history is incomplete: {e}"))
            })?;
            if !missing.is_empty() {
                return Err(Error::Verification(format!(
                    "received history is incomplete: {} object(s) missing, first {}",
                    missing.len(),
                    missing[0]
                )));
            }
        }
        let mut out = FetchOutcome::default();
        for (branch, old, new) in todo {
            self.set_tracking(repo, &format!("remotes/{remote}/{branch}"), new)?;
            out.updated.push(FetchedRef { branch, old, new });
        }
        Ok(out)
    }

    fn fetch_objects(&mut self, repo: &mut Repo, wants: &[ObjectId], haves: &[ObjectId]) -> Result<()> {
        let slow = self.cfg.slow_call_timeout;
        let (total, _commits) = match self.call_with(
            &Request::Plan {
                wants: wants.to_vec(),
                haves: haves.to_vec(),
            },
            slow,
        )? {
            Response::PlanOk { objects, commits } => (objects, commits),
            r => return Err(self.unexpected(&r, "PlanOk")),
        };
        let lim = self.hello.limits;
        let mut cursor = 0u64;
        let mut seen = 0u64;
        loop {
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let (ids, end) = match self.call(&Request::PlanPage {
                cursor,
                max: lim.max_ids_per_request,
            })? {
                Response::PlanPage { ids, end } => (ids, end),
                r => return Err(self.unexpected(&r, "PlanPage")),
            };
            cursor += ids.len() as u64;
            seen += ids.len() as u64;
            let mut need: Vec<ObjectId> = Vec::new();
            for id in &ids {
                if repo.store.has(id)? {
                    self.stats.objects_already_complete += 1;
                } else {
                    need.push(*id);
                }
            }
            let mut pending: Vec<ObjectId> = need;
            while !pending.is_empty() {
                if self.cancel.is_cancelled() {
                    repo.flush()?;
                    return Err(Error::Cancelled);
                }
                let ask: Vec<ObjectId> = pending.iter().take(lim.max_ids_per_request as usize).copied().collect();
                let (objects, _remaining) = match self.call(&Request::Get {
                    ids: ask.clone(),
                    allow_delta: self.use_delta,
                })? {
                    Response::Objects { objects, remaining } => (objects, remaining),
                    r => return Err(self.unexpected(&r, "Objects")),
                };
                if objects.is_empty() {
                    return Err(Error::protocol("host returned no objects for a Get"));
                }
                let asked: HashSet<ObjectId> = ask.iter().copied().collect();
                let mut got: HashSet<ObjectId> = HashSet::new();
                for o in objects {
                    if !asked.contains(&o.id) {
                        return Err(Error::protocol("host sent an object that was not requested"));
                    }
                    match self.accept_object(repo, o, &mut got) {
                        Ok(()) => {}
                        Err(e) if e.code() == Some(ErrorCode::UnknownBase) => {
                            // Cannot rebuild a delta here: ask again for whole objects.
                            self.use_delta = false;
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
                pending.retain(|i| !got.contains(i));
                // Make each batch durable before asking for the next, so an interruption
                // loses at most one batch and a rerun skips everything already here.
                repo.flush()?;
                failpoint::hit("client_get_done");
            }
            repo.flush()?;
            self.report("downloading", seen, total);
            failpoint::hit("client_page_done");
            if end {
                break;
            }
        }
        Ok(())
    }

    fn accept_object(&mut self, repo: &mut Repo, o: WireObject, got: &mut HashSet<ObjectId>) -> Result<()> {
        let max = self.hello.limits.max_object as usize;
        let data = match o.base {
            None => {
                if o.data.len() > max {
                    return Err(Error::Verification(format!("object {} is too large", o.id)));
                }
                o.data.clone()
            }
            Some(b) => {
                let base = match repo.store.get(&b) {
                    Ok((_, bytes)) => bytes,
                    Err(_) => {
                        return Err(wire(ErrorCode::UnknownBase, format!("missing delta base {b}")))
                    }
                };
                o.logical(Some(&base), max)
                    .map_err(|e| Error::Verification(format!("object {}: {e}", o.id)))?
            }
        };
        if o.ty != ObjectType::Chunk {
            references(o.ty, &data)
                .map_err(|e| Error::Verification(format!("object {} is malformed: {e}", o.id)))?;
        }
        // Verify before storing: a corrupt object must not enter the store at all.
        let actual = rustgit_store::hash_object(o.ty, &data);
        if actual != o.id {
            return Err(Error::Verification(format!(
                "object claimed to be {} hashes to {}",
                o.id, actual
            )));
        }
        repo.store.put(o.ty, &data, o.base.as_ref())?;
        self.stats.objects_transferred += 1;
        if o.base.is_some() {
            self.stats.objects_as_delta += 1;
        }
        self.stats.logical_bytes += data.len() as u64;
        got.insert(o.id);
        Ok(())
    }
}

fn collect_haves(repo: &Repo, tips: &[ObjectId], cap: usize) -> Vec<ObjectId> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut out = Vec::new();
    let mut queue: std::collections::VecDeque<ObjectId> = tips.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        if out.len() >= cap {
            break;
        }
        if !seen.insert(id) {
            continue;
        }
        if let Ok(c) = repo.read_commit(&id) {
            out.push(id);
            queue.extend(c.parents);
        }
    }
    out
}

/// Runs `op` (which should open its own connection) again after retryable failures, with
/// exponential backoff. Because pushes are idempotent and uploads resume, repeating the
/// whole operation is safe.
pub fn with_retries<T>(
    cfg: &ClientConfig,
    cancel: &CancelToken,
    mut op: impl FnMut() -> Result<T>,
) -> Result<T> {
    let mut attempt = 0u32;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if e.retryable() && attempt < cfg.max_retries && !cancel.is_cancelled() => {
                let backoff = Duration::from_millis(200u64 << attempt.min(5));
                std::thread::sleep(backoff);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Creates `dest` as a new working copy of the host's project: init, fetch everything,
/// create the default branch and check it out. Removes `dest` again if anything fails.
pub fn clone_into(
    t: &dyn Transport,
    url: &str,
    cfg: &ClientConfig,
    dest: &Path,
    remote: &str,
    cancel: CancelToken,
) -> Result<Repo> {
    let existed = dest.exists();
    if existed && std::fs::read_dir(dest)?.next().is_some() {
        return Err(Error::Other(format!(
            "destination {} exists and is not empty",
            dest.display()
        )));
    }
    std::fs::create_dir_all(dest)?;
    let result = (|| -> Result<Repo> {
        let mut repo = Repo::init(&std::fs::canonicalize(dest)?, "main")?;
        let mut client = Client::connect(t, cfg, cancel)?;
        let default_branch = client.hello.policy.default_branch.clone();
        repo.set_config(&format!("remote.{remote}.url"), url)?;
        repo.set_config(&format!("remote.{remote}.project"), &client.hello.repo_uuid)?;
        repo.set_config("repo.uuid", &client.hello.repo_uuid)?;
        if let Some(a) = &client.host_auth {
            repo.set_config(
                &format!("remote.{remote}.host_key"),
                &a.host_key.fingerprint().to_hex(),
            )?;
        }
        let outcome = client.fetch(&mut repo, remote)?;
        let chosen = outcome
            .updated
            .iter()
            .find(|f| f.branch == default_branch && f.new.is_some())
            .or_else(|| outcome.updated.iter().find(|f| f.new.is_some()));
        if let Some(f) = chosen {
            let commit = f.new.unwrap();
            repo.cas_ref(&format!("heads/{}", f.branch), None, Some(commit))?;
            repo.set_config("head", &f.branch)?;
            let flat = repo.commit_flat(&commit)?;
            repo.checkout_flat(&Flat::new(), &flat, false)?;
        } else {
            repo.set_config("head", &default_branch)?;
        }
        repo.flush()?;
        client.close();
        Ok(repo)
    })();
    if result.is_err() {
        if existed {
            // Leave the (empty) directory the user gave us, but remove what we added.
            if let Ok(rd) = std::fs::read_dir(dest) {
                for e in rd.flatten() {
                    let p = e.path();
                    let _ = if p.is_dir() {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_file(&p)
                    };
                }
            }
        } else {
            let _ = std::fs::remove_dir_all(dest);
        }
    }
    result
}
