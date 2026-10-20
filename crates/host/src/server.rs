//! Connection handling: one thread per connection, strictly request/response.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use rustgit_protocol::secure::accept_secure;
use rustgit_protocol::{
    Channel, Duplex, ErrorCode, HelloOk, Listener, Policy, Request, Response, SessionId,
    WireError, PROTOCOL_MAX_VERSION, PROTOCOL_MIN_VERSION,
};
use rustgit_protocol::{Detail, Error as ProtoError};
use rustgit_store::ObjectId;

use crate::auth::{AdmitRequest, Op};
use crate::host::{internal, io_wire, Host};

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// A host accepting connections on a background thread.
pub struct RunningHost {
    pub host: Arc<Host>,
    pub addr: String,
    thread: Option<JoinHandle<()>>,
}

impl RunningHost {
    pub fn stop(mut self) {
        self.host
            .shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for RunningHost {
    fn drop(&mut self) {
        self.host
            .shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts the accept loop on its own thread.
pub fn spawn(host: Arc<Host>, listener: Box<dyn Listener>) -> RunningHost {
    let addr = listener.local_addr();
    let h = host.clone();
    let thread = std::thread::spawn(move || run(h, listener));
    RunningHost {
        host,
        addr,
        thread: Some(thread),
    }
}

/// Accept loop; returns after `shutdown` is set and all connection threads have finished.
pub fn run(host: Arc<Host>, listener: Box<dyn Listener>) {
    let mut handlers: Vec<JoinHandle<()>> = Vec::new();
    let mut last_sweep = std::time::Instant::now();
    while !host.shutdown.load(Ordering::SeqCst) {
        if host.poisoned() {
            eprintln!("host state poisoned by a panic; exiting so recovery can run on restart");
            std::process::exit(70);
        }
        match listener.accept(Duration::from_millis(100)) {
            Ok(Some(io)) => {
                handlers.retain(|h| !h.is_finished());
                let h = host.clone();
                handlers.push(std::thread::spawn(move || handle_connection(h, io)));
            }
            Ok(None) => {}
            Err(e) => {
                host.log(format!("accept failed: {e}"));
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if last_sweep.elapsed() > Duration::from_secs(600) {
            host.sweep_staging();
            last_sweep = std::time::Instant::now();
        }
    }
    for h in handlers {
        let _ = h.join();
    }
}

struct Conn {
    id: u64,
    /// The authorizer's name for this connection (`Admission::principal`).
    device: String,
    credential: Vec<u8>,
    plan: Vec<ObjectId>,
    /// object -> a similar object the client will already hold when it is sent
    plan_hints: std::collections::HashMap<ObjectId, ObjectId>,
}

fn send(ch: &mut Channel, r: &Response) -> Result<(), ProtoError> {
    ch.send(&r.encode())
}

fn err(code: ErrorCode, m: impl Into<String>) -> Response {
    Response::Error(WireError::new(code, m))
}

/// Counts connections that have not been admitted yet, so a flood of half-open handshakes
/// cannot exhaust threads.
struct PreAuth<'a>(&'a Host, bool);

impl<'a> PreAuth<'a> {
    fn enter(host: &'a Host) -> Option<PreAuth<'a>> {
        let n = host.preauth.fetch_add(1, Ordering::SeqCst);
        let g = PreAuth(host, true);
        (n < host.cfg.max_unauthenticated).then_some(g)
    }
    fn done(&mut self) {
        if self.1 {
            self.1 = false;
            self.0.preauth.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for PreAuth<'_> {
    fn drop(&mut self) {
        self.done();
    }
}

fn handle_connection(host: Arc<Host>, io: Box<dyn Duplex>) {
    let id = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
    let peer = io.peer();
    let Some(mut pre) = PreAuth::enter(&host) else {
        host.log(format!("{peer}: too many unauthenticated connections; dropped"));
        return;
    };
    let (io, peer_key) = match host.auth.host_identity() {
        Some((key, chain)) => match accept_secure(io, &key, &chain, host.cfg.io_timeout) {
            Ok((io, k)) => (io, Some(k)),
            Err(e) => {
                host.log(format!("{peer}: secure handshake failed: {e}"));
                return;
            }
        },
        None => (io, None),
    };
    let mut ch = Channel::new(io, Some(host.cfg.io_timeout)).with_peer_key(peer_key);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        serve_connection(&host, &mut ch, id, &mut pre)
    }));
    host.release_conn_sessions(id);
    match result {
        Ok((device, Ok(()))) => {
            host.log(format!("{peer} ({device}) closed"));
        }
        Ok((device, Err(e))) => host.log(format!("{peer} ({device}) ended: {e}")),
        Err(_) => host.log(format!("{peer}: connection handler panicked")),
    }
    ch.shutdown();
}

fn serve_connection(
    host: &Arc<Host>,
    ch: &mut Channel,
    id: u64,
    pre: &mut PreAuth<'_>,
) -> (String, Result<(), ProtoError>) {
    let mut device = String::from("?");
    let r = (|| -> Result<(), ProtoError> {
        // ---- handshake ----
        let first = ch.recv(Some(host.cfg.io_timeout))?;
        let hello = match Request::decode(&first)? {
            Request::Hello(h) => h,
            other => {
                send(
                    ch,
                    &err(
                        ErrorCode::BadRequest,
                        format!("expected Hello, got {}", other.name()),
                    ),
                )?;
                return Ok(());
            }
        };
        device = hello.device.clone();
        let lo = hello.min_version.max(PROTOCOL_MIN_VERSION);
        let hi = hello.max_version.min(PROTOCOL_MAX_VERSION);
        if lo > hi || hello.min_version > hello.max_version {
            send(
                ch,
                &Response::Error(
                    WireError::new(
                        ErrorCode::VersionMismatch,
                        format!(
                            "client speaks protocol {}..{}, this host speaks {}..{}",
                            hello.min_version, hello.max_version, PROTOCOL_MIN_VERSION, PROTOCOL_MAX_VERSION
                        ),
                    )
                    .with_detail(Detail::Versions {
                        min: PROTOCOL_MIN_VERSION,
                        max: PROTOCOL_MAX_VERSION,
                    }),
                ),
            )?;
            return Ok(());
        }
        if hello.device.is_empty() || hello.device.len() > 128 {
            send(ch, &err(ErrorCode::BadRequest, "device name must be 1..128 bytes"))?;
            return Ok(());
        }
        if let Some(p) = &hello.project {
            if p != host.uuid() {
                send(
                    ch,
                    &err(
                        ErrorCode::ProjectMismatch,
                        format!("this host serves project {}, not {p}", host.uuid()),
                    ),
                )?;
                return Ok(());
            }
        }
        let admission = match host.auth.admit(&AdmitRequest {
            peer_key: ch.peer_key(),
            device_name: &hello.device,
            credential: &hello.credential,
        }) {
            Ok(a) => a,
            Err(e) => {
                host.log(format!("{} refused: {}", hello.device, e));
                send(ch, &Response::Error(e))?;
                return Ok(());
            }
        };
        pre.done();
        device = admission.display.clone();
        {
            let mut c = host.conns.lock().unwrap();
            let per = c.per_device.get(&admission.principal).copied().unwrap_or(0);
            if c.total >= host.cfg.max_connections || per >= host.cfg.max_connections_per_device {
                drop(c);
                send(
                    ch,
                    &err(ErrorCode::Busy, "too many connections; try again shortly"),
                )?;
                return Ok(());
            }
            c.total += 1;
            *c.per_device.entry(admission.principal.clone()).or_insert(0) += 1;
        }
        let _guard = ConnGuard {
            host: host.clone(),
            device: admission.principal.clone(),
        };
        send(
            ch,
            &Response::HelloOk(HelloOk {
                version: hi,
                repo_uuid: host.uuid().to_string(),
                limits: host.cfg.limits,
                policy: Policy {
                    allow_force: host.cfg.allow_force,
                    allow_delete: host.cfg.allow_delete,
                    default_branch: host.default_branch.clone(),
                },
                ref_seq: host.ref_seq(),
                role: Some(admission.role),
            }),
        )?;
        ch.set_max_frame(host.cfg.limits.max_frame);

        let mut conn = Conn {
            id,
            device: admission.principal,
            credential: Vec::new(),
            plan: Vec::new(),
            plan_hints: std::collections::HashMap::new(),
        };

        // ---- request loop ----
        let mut idle = Duration::ZERO;
        let slice = Duration::from_secs(1);
        let mut last_touch = std::time::Instant::now();
        loop {
            if host.shutdown.load(Ordering::SeqCst) {
                let _ = send(ch, &err(ErrorCode::ShuttingDown, "host is shutting down"));
                return Ok(());
            }
            let payload = match ch.recv(Some(slice)) {
                Ok(p) => p,
                Err(ProtoError::Timeout) => {
                    // an idle connection of a revoked device ends within a second
                    if let Err(e) = host.auth.authorize(&conn.device, &conn.credential, Op::Read) {
                        let _ = send(ch, &Response::Error(e));
                        return Ok(());
                    }
                    idle += slice;
                    if idle >= host.cfg.idle_timeout {
                        let _ = send(ch, &err(ErrorCode::Timeout, "idle timeout"));
                        return Ok(());
                    }
                    continue;
                }
                Err(ProtoError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(())
                }
                Err(e) => return Err(e),
            };
            idle = Duration::ZERO;
            let req = match Request::decode(&payload) {
                Ok(r) => r,
                Err(e) => {
                    let _ = send(ch, &err(ErrorCode::BadRequest, e.to_string()));
                    return Err(e);
                }
            };
            if matches!(req, Request::Bye) {
                return Ok(());
            }
            let resp = dispatch(host, &mut conn, req);
            send(ch, &resp)?;
            if let Response::Error(w) = &resp {
                if matches!(
                    w.code,
                    ErrorCode::DeviceRevoked | ErrorCode::UnknownDevice | ErrorCode::Unauthorized
                ) {
                    return Ok(());
                }
            }
            if last_touch.elapsed() >= Duration::from_secs(60) {
                host.auth.touch(&conn.device);
                last_touch = std::time::Instant::now();
            }
        }
    })();
    (device, r)
}

struct ConnGuard {
    host: Arc<Host>,
    device: String,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let mut c = self.host.conns.lock().unwrap();
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per_device.get_mut(&self.device) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.per_device.remove(&self.device);
            }
        }
    }
}

fn owns(host: &Host, conn: &Conn, s: &SessionId) -> bool {
    host.sessions
        .lock()
        .unwrap()
        .get(s)
        .is_some_and(|st| st.owner == conn.id)
}

fn dispatch(host: &Arc<Host>, conn: &mut Conn, req: Request) -> Response {
    let r: Result<Response, WireError> = (|| {
        // Every request re-checks the device against the live authorization records, so a
        // revocation or role change applies to connections that are already open.
        let gate = match &req {
            Request::ListRefs { .. }
            | Request::Have { .. }
            | Request::Plan { .. }
            | Request::PlanPage { .. }
            | Request::Get { .. }
            | Request::Watch { .. } => Some(Op::Read),
            Request::Put { .. } | Request::Cancel { .. } => Some(Op::Upload),
            // Begin / UpdateRef are checked in the host with the branch name
            Request::Begin(_) | Request::UpdateRef(_) | Request::Hello(_) | Request::Bye => None,
        };
        if let Some(op) = gate {
            host.auth.authorize(&conn.device, &conn.credential, op)?;
        }
        match req {
            Request::Hello(_) => Err(WireError::new(ErrorCode::BadRequest, "already greeted")),
            Request::Bye => unreachable!(),
            Request::ListRefs { prefix } => {
                host.auth.authorize(&conn.device, &conn.credential, Op::Read)?;
                let seq = host.ref_seq();
                Ok(Response::Refs {
                    records: host.list_refs(&prefix)?,
                    seq,
                })
            }
            Request::Have { session, ids } => {
                check_ids(host, &ids)?;
                Ok(Response::HaveResult(host.have(session.as_ref(), &ids)))
            }
            Request::Begin(b) => {
                let staged = host
                    .staging
                    .stat(&b.session)
                    .map_err(|e| io_wire(&e))?;
                host.begin_checks(
                    &conn.device,
                    &conn.credential,
                    &b.ref_name,
                    b.expected,
                    b.expected_gen,
                    b.force,
                    b.object_count,
                    b.byte_estimate,
                    staged.1,
                )?;
                // Claim first: opening the session cleans interrupted writes, which must
                // not happen under another connection that is still writing there.
                host.claim_session(conn.id, &conn.device, b.session, staged)?;
                let info = format!(
                    "device={}\nref={}\nnew_head={}\n",
                    conn.device,
                    b.ref_name,
                    b.new_head.to_hex()
                );
                let staged = host
                    .staging
                    .open_session(&b.session, &info)
                    .map_err(|e| io_wire(&e))?;
                Ok(Response::BeginOk {
                    staged_objects: staged.0,
                    staged_bytes: staged.1,
                })
            }
            Request::Put { session, objects } => {
                if !owns(host, conn, &session) {
                    return Err(WireError::new(
                        ErrorCode::UnknownSession,
                        "Put without Begin on this connection",
                    ));
                }
                if objects.len() as u32 > host.cfg.limits.max_batch_objects {
                    return Err(WireError::new(
                        ErrorCode::BadRequest,
                        format!(
                            "batch of {} objects exceeds the limit {}",
                            objects.len(),
                            host.cfg.limits.max_batch_objects
                        ),
                    ));
                }
                let now_bytes = host
                    .sessions
                    .lock()
                    .unwrap()
                    .get(&session)
                    .map(|s| s.bytes)
                    .unwrap_or(0);
                let (fresh, dup, bytes) = host.stage_batch(&session, &objects, now_bytes)?;
                let mut m = host.sessions.lock().unwrap();
                let st = m.get_mut(&session).ok_or_else(|| internal("session lost"))?;
                st.objects += fresh as u64;
                st.bytes += bytes;
                Ok(Response::PutAck {
                    accepted: fresh,
                    already_staged: dup,
                    staged_objects: st.objects,
                    staged_bytes: st.bytes,
                })
            }
            Request::UpdateRef(u) => {
                let (generation, already_applied) =
                    host.update_ref(&conn.device, &conn.credential, &u)?;
                host.release_session(&u.session);
                Ok(Response::RefUpdated {
                    generation,
                    already_applied,
                })
            }
            Request::Cancel { session, discard } => {
                if owns(host, conn, &session) {
                    host.release_session(&session);
                    if discard {
                        host.discard_session(&session);
                    }
                }
                Ok(Response::Cancelled)
            }
            Request::Plan { wants, haves } => {
                host.auth.authorize(&conn.device, &conn.credential, Op::Read)?;
                check_ids(host, &wants)?;
                check_ids(host, &haves)?;
                let (objs, commits) = host.plan_fetch(&wants, &haves)?;
                let n = objs.len() as u64;
                conn.plan_hints = objs
                    .iter()
                    .filter_map(|o| o.hint.map(|h| (o.id, h)))
                    .collect();
                conn.plan = objs.iter().map(|o| o.id).collect();
                Ok(Response::PlanOk {
                    objects: n,
                    commits,
                })
            }
            Request::PlanPage { cursor, max } => {
                let max = max.min(host.cfg.limits.max_ids_per_request) as usize;
                let start = (cursor as usize).min(conn.plan.len());
                let end = (start + max).min(conn.plan.len());
                Ok(Response::PlanPage {
                    ids: conn.plan[start..end].to_vec(),
                    end: end >= conn.plan.len(),
                })
            }
            Request::Get { ids, allow_delta } => {
                host.auth.authorize(&conn.device, &conn.credential, Op::Read)?;
                check_ids(host, &ids)?;
                let hints = if allow_delta { Some(&conn.plan_hints) } else { None };
                let (objects, remaining) = host.get_objects(&ids, hints)?;
                Ok(Response::Objects { objects, remaining })
            }
            Request::Watch { since, timeout_ms } => {
                let t = Duration::from_millis(timeout_ms as u64).min(host.cfg.max_watch);
                let deadline = std::time::Instant::now() + t;
                loop {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    let (seq, changed) = host.wait_seq(since, left.min(Duration::from_secs(1)));
                    if changed || left <= Duration::from_secs(1) {
                        return Ok(Response::WatchResult { seq, changed });
                    }
                    // a long poll must not outlive a revocation
                    host.auth.authorize(&conn.device, &conn.credential, Op::Read)?;
                }
            }
        }
    })();
    match r {
        Ok(r) => r,
        Err(w) => {
            host.log(format!("{} -> {}", conn.device, w));
            Response::Error(w)
        }
    }
}

fn check_ids(host: &Host, ids: &[ObjectId]) -> Result<(), WireError> {
    if ids.len() as u32 > host.cfg.limits.max_ids_per_request.max(4096) {
        return Err(WireError::new(
            ErrorCode::BadRequest,
            format!(
                "{} ids in one request exceeds the limit {}",
                ids.len(),
                host.cfg.limits.max_ids_per_request
            ),
        ));
    }
    Ok(())
}
