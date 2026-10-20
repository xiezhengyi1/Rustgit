//! Protocol messages and their canonical binary encoding.
//!
//! A message is one frame payload: a one-byte tag followed by fields encoded with the same
//! primitives as the repository format (`varint`, `bytes`, `string`, 32-byte ids). Requests
//! have tags below 100, responses 100 and above. The conversation is strictly
//! request/response: the client sends one request and reads one response.

use rustgit_store::codec::{Decoder, Encoder};
use rustgit_store::{ObjectId, ObjectType};

use crate::error::{Error, Result, WireError};

pub const MAX_IDS_PER_MESSAGE: usize = 65_536;
pub const MAX_REF_RECORDS: usize = 100_000;
pub const MAX_NAME: usize = 1024;
/// Hard ceiling for the decompressed size of one object batch, whatever the peer claims.
pub const MAX_BATCH_RAW: u64 = 64 * 1024 * 1024;

/// Opaque id of one upload. The client derives it so a restarted client reuses the same
/// staging area (see docs/protocol.md section "Sessions and resume").
pub type SessionId = [u8; 16];

#[derive(Clone, PartialEq, Eq)]
pub struct Hello {
    pub min_version: u16,
    pub max_version: u16,
    /// Self-declared device name, for display. On the secure channel the host authorizes the
    /// *authenticated device key*, never this name.
    pub device: String,
    /// Project the client expects (repository UUID); `None` accepts whatever is served.
    pub project: Option<String>,
    /// Encoded [`Credential`]: empty for an already enrolled device, or an enrollment
    /// presentation. Secret, so it is never printed.
    pub credential: Vec<u8>,
}

impl std::fmt::Debug for Hello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hello")
            .field("versions", &(self.min_version, self.max_version))
            .field("device", &self.device)
            .field("project", &self.project)
            .field("credential", &format_args!("<{} bytes, redacted>", self.credential.len()))
            .finish()
    }
}

/// What a device presents in `Hello.credential`.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// Nothing: the device must already be enrolled.
    None,
    /// Redeem an invitation: its id and enrollment secret.
    Enroll { invitation: [u8; 16], secret: [u8; 32] },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::None => f.write_str("Credential::None"),
            Credential::Enroll { .. } => f.write_str("Credential::Enroll(<redacted>)"),
        }
    }
}

impl Credential {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Credential::None => Vec::new(),
            Credential::Enroll { invitation, secret } => {
                let mut v = Vec::with_capacity(49);
                v.push(1);
                v.extend_from_slice(invitation);
                v.extend_from_slice(secret);
                v
            }
        }
    }

    /// Strict: anything but empty or a well-formed enrollment is an error.
    pub fn decode(b: &[u8]) -> Result<Credential> {
        match b.first() {
            None => Ok(Credential::None),
            Some(1) if b.len() == 49 => Ok(Credential::Enroll {
                invitation: b[1..17].try_into().unwrap(),
                secret: b[17..49].try_into().unwrap(),
            }),
            _ => Err(Error::protocol("malformed credential")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_frame: u32,
    pub max_batch_objects: u32,
    pub max_batch_bytes: u32,
    pub max_object: u32,
    pub max_ids_per_request: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_frame: 16 * 1024 * 1024,
            max_batch_objects: 1024,
            max_batch_bytes: 4 * 1024 * 1024,
            max_object: 8 * 1024 * 1024,
            max_ids_per_request: 8192,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub allow_force: bool,
    pub allow_delete: bool,
    pub default_branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelloOk {
    pub version: u16,
    pub repo_uuid: String,
    pub limits: Limits,
    pub policy: Policy,
    /// Increases whenever any ref changes; `Watch` waits on it.
    pub ref_seq: u64,
    /// The role the host granted this connection. `None` from hosts that predate roles.
    pub role: Option<crate::identity::Role>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefRecord {
    pub name: String,
    /// `None` is a deletion tombstone.
    pub commit: Option<ObjectId>,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum HaveState {
    Missing = 0,
    /// Present and everything it depends on is present too; the sender may skip it and
    /// everything below it.
    Complete = 1,
    /// Received in this upload session and durably staged; do not resend, but its
    /// dependencies are not implied.
    Staged = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeginReq {
    pub session: SessionId,
    pub ref_name: String,
    pub expected: Option<ObjectId>,
    pub expected_gen: u64,
    pub new_head: ObjectId,
    pub force: bool,
    /// Declared totals, checked against quotas before any byte is accepted.
    pub object_count: u64,
    pub byte_estimate: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateRefReq {
    pub session: SessionId,
    pub ref_name: String,
    pub expected: Option<ObjectId>,
    pub expected_gen: u64,
    /// `None` deletes the ref (leaves a tombstone).
    pub new: Option<ObjectId>,
    pub force: bool,
}

/// One object on the wire. Without `base`, `data` is the object's logical bytes. With
/// `base`, `data` is those bytes compressed using the logical bytes of object `base` as a
/// zstd raw-content dictionary: a delta that is only an encoding of this message. The
/// receiver reconstructs the logical bytes and verifies them against `id`, so what is
/// stored and hashed never depends on how an object was transmitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireObject {
    pub ty: ObjectType,
    pub id: ObjectId,
    pub data: Vec<u8>,
    pub base: Option<ObjectId>,
}

impl WireObject {
    pub fn full(ty: ObjectType, id: ObjectId, data: Vec<u8>) -> Self {
        WireObject {
            ty,
            id,
            data,
            base: None,
        }
    }

    /// Tries to re-encode `full` as a delta against `base_bytes`. Returns `None` unless the
    /// delta is clearly smaller.
    pub fn delta(
        ty: ObjectType,
        id: ObjectId,
        full: &[u8],
        base: ObjectId,
        base_bytes: &[u8],
    ) -> Option<Self> {
        if full.len() < 32 || base_bytes.is_empty() {
            return None;
        }
        let mut c = zstd::bulk::Compressor::with_dictionary(3, base_bytes).ok()?;
        let payload = c.compress(full).ok()?;
        if payload.len() + 32 >= full.len() - full.len() / 10 {
            return None;
        }
        Some(WireObject {
            ty,
            id,
            data: payload,
            base: Some(base),
        })
    }

    /// The logical bytes of this object. `base_bytes` must be the bytes of `self.base`
    /// when it is set; `max_len` bounds the size of the result.
    pub fn logical(&self, base_bytes: Option<&[u8]>, max_len: usize) -> Result<Vec<u8>> {
        match (self.base, base_bytes) {
            (None, _) => Ok(self.data.clone()),
            (Some(_), Some(b)) => {
                let mut d = zstd::bulk::Decompressor::with_dictionary(b)
                    .map_err(|e| Error::protocol(format!("bad delta base: {e}")))?;
                d.decompress(&self.data, max_len)
                    .map_err(|e| Error::protocol(format!("bad delta payload: {e}")))
            }
            (Some(_), None) => Err(Error::protocol("delta base not supplied")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Hello(Hello),
    ListRefs {
        prefix: String,
    },
    Have {
        session: Option<SessionId>,
        ids: Vec<ObjectId>,
    },
    Begin(BeginReq),
    Put {
        session: SessionId,
        objects: Vec<WireObject>,
    },
    UpdateRef(UpdateRefReq),
    Cancel {
        session: SessionId,
        /// Delete the staged data too (otherwise it is kept so the upload can resume).
        discard: bool,
    },
    /// Fetch: compute what a client holding `haves` needs to have `wants` complete.
    Plan {
        wants: Vec<ObjectId>,
        haves: Vec<ObjectId>,
    },
    PlanPage {
        cursor: u64,
        max: u32,
    },
    Get {
        ids: Vec<ObjectId>,
        /// The client can reconstruct delta-encoded objects from objects it already holds.
        allow_delta: bool,
    },
    Watch {
        since: u64,
        timeout_ms: u32,
    },
    Bye,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    HelloOk(HelloOk),
    Refs {
        records: Vec<RefRecord>,
        seq: u64,
    },
    HaveResult(Vec<HaveState>),
    BeginOk {
        staged_objects: u64,
        staged_bytes: u64,
    },
    PutAck {
        accepted: u32,
        already_staged: u32,
        staged_objects: u64,
        staged_bytes: u64,
    },
    RefUpdated {
        generation: u64,
        /// The update had been applied by an earlier attempt; nothing changed this time.
        already_applied: bool,
    },
    Cancelled,
    PlanOk {
        objects: u64,
        commits: u64,
    },
    PlanPage {
        ids: Vec<ObjectId>,
        end: bool,
    },
    Objects {
        objects: Vec<WireObject>,
        /// How many requested ids did not fit the batch limits and must be requested again.
        remaining: u32,
    },
    WatchResult {
        seq: u64,
        changed: bool,
    },
    Error(WireError),
}

// ---- encoding helpers ------------------------------------------------------------------

fn opt_id(e: &mut Encoder, id: &Option<ObjectId>) {
    match id {
        Some(i) => {
            e.u8(1).id(i);
        }
        None => {
            e.u8(0);
        }
    }
}

fn get_opt_id(d: &mut Decoder) -> Result<Option<ObjectId>> {
    match d.u8()? {
        0 => Ok(None),
        1 => Ok(Some(d.id()?)),
        _ => Err(Error::protocol("bad option tag")),
    }
}

fn put_ids(e: &mut Encoder, ids: &[ObjectId]) {
    e.varint(ids.len() as u64);
    for i in ids {
        e.id(i);
    }
}

fn get_ids(d: &mut Decoder) -> Result<Vec<ObjectId>> {
    let n = d.varint()?;
    if n > MAX_IDS_PER_MESSAGE as u64 || (n as usize) * 32 > d.remaining() {
        return Err(Error::protocol(format!("id list of {n} entries rejected")));
    }
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        v.push(d.id()?);
    }
    Ok(v)
}

fn get_session(d: &mut Decoder) -> Result<SessionId> {
    let b = d.bytes(16)?;
    b.try_into()
        .map_err(|_| Error::protocol("session id must be 16 bytes"))
}

fn put_objects(e: &mut Encoder, objects: &[WireObject]) {
    let mut raw = Encoder::new();
    for o in objects {
        raw.u8(o.ty as u8).id(&o.id);
        match &o.base {
            Some(b) => {
                raw.u8(1).id(b);
            }
            None => {
                raw.u8(0);
            }
        }
        raw.bytes(&o.data);
    }
    let raw = raw.finish();
    e.varint(objects.len() as u64);
    // Compression is a property of this message only; it says nothing about how either
    // side stores the objects.
    let compressed = if raw.len() >= 256 {
        zstd::bulk::compress(&raw, 3).ok().filter(|c| c.len() < raw.len())
    } else {
        None
    };
    match compressed {
        Some(c) => {
            e.u8(1).varint(raw.len() as u64).bytes(&c);
        }
        None => {
            e.u8(0).varint(raw.len() as u64).bytes(&raw);
        }
    }
}

fn get_objects(d: &mut Decoder) -> Result<Vec<WireObject>> {
    let count = d.varint()?;
    let mode = d.u8()?;
    let raw_len = d.varint()?;
    if raw_len > MAX_BATCH_RAW || count > raw_len / 35 + 1 {
        return Err(Error::protocol("object batch size out of range"));
    }
    let body = d.bytes(MAX_BATCH_RAW as usize)?;
    let raw: std::borrow::Cow<[u8]> = match mode {
        0 => {
            if body.len() as u64 != raw_len {
                return Err(Error::protocol("object batch length mismatch"));
            }
            body.into()
        }
        1 => {
            let out = zstd::bulk::decompress(body, raw_len as usize)
                .map_err(|e| Error::protocol(format!("bad compressed batch: {e}")))?;
            if out.len() as u64 != raw_len {
                return Err(Error::protocol("object batch length mismatch"));
            }
            out.into()
        }
        _ => return Err(Error::protocol("unknown batch compression")),
    };
    let mut r = Decoder::new(&raw);
    let mut out = Vec::with_capacity(count.min(4096) as usize);
    for _ in 0..count {
        let ty = ObjectType::from_u8(r.u8()?).map_err(|e| Error::protocol(e.to_string()))?;
        let id = r.id()?;
        let base = match r.u8()? {
            0 => None,
            1 => Some(r.id()?),
            _ => return Err(Error::protocol("bad object encoding tag")),
        };
        let data = r.bytes(MAX_BATCH_RAW as usize)?.to_vec();
        out.push(WireObject { ty, id, data, base });
    }
    r.finish()?;
    Ok(out)
}

// ---- Request ----------------------------------------------------------------------------

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match self {
            Request::Hello(h) => {
                e.u8(1)
                    .varint(h.min_version as u64)
                    .varint(h.max_version as u64)
                    .string(&h.device);
                match &h.project {
                    Some(p) => {
                        e.u8(1).string(p);
                    }
                    None => {
                        e.u8(0);
                    }
                }
                e.bytes(&h.credential);
            }
            Request::ListRefs { prefix } => {
                e.u8(2).string(prefix);
            }
            Request::Have { session, ids } => {
                e.u8(3);
                match session {
                    Some(s) => {
                        e.bytes(s);
                    }
                    None => {
                        e.bytes(&[]);
                    }
                }
                put_ids(&mut e, ids);
            }
            Request::Begin(b) => {
                e.u8(4)
                    .bytes(&b.session)
                    .string(&b.ref_name)
                    .varint(b.expected_gen)
                    .id(&b.new_head)
                    .u8(b.force as u8)
                    .varint(b.object_count)
                    .varint(b.byte_estimate);
                opt_id(&mut e, &b.expected);
            }
            Request::Put { session, objects } => {
                e.u8(5).bytes(session);
                put_objects(&mut e, objects);
            }
            Request::UpdateRef(u) => {
                e.u8(6)
                    .bytes(&u.session)
                    .string(&u.ref_name)
                    .varint(u.expected_gen)
                    .u8(u.force as u8);
                opt_id(&mut e, &u.expected);
                opt_id(&mut e, &u.new);
            }
            Request::Cancel { session, discard } => {
                e.u8(7).bytes(session).u8(*discard as u8);
            }
            Request::Plan { wants, haves } => {
                e.u8(8);
                put_ids(&mut e, wants);
                put_ids(&mut e, haves);
            }
            Request::PlanPage { cursor, max } => {
                e.u8(9).varint(*cursor).varint(*max as u64);
            }
            Request::Get { ids, allow_delta } => {
                e.u8(10).u8(*allow_delta as u8);
                put_ids(&mut e, ids);
            }
            Request::Watch { since, timeout_ms } => {
                e.u8(11).varint(*since).varint(*timeout_ms as u64);
            }
            Request::Bye => {
                e.u8(12);
            }
        }
        e.finish()
    }

    pub fn decode(payload: &[u8]) -> Result<Request> {
        let mut d = Decoder::new(payload);
        let tag = d.u8()?;
        let r = match tag {
            1 => {
                let min_version = d.varint()?.min(u16::MAX as u64) as u16;
                let max_version = d.varint()?.min(u16::MAX as u64) as u16;
                let device = d.string(256)?.to_string();
                let project = match d.u8()? {
                    0 => None,
                    1 => Some(d.string(256)?.to_string()),
                    _ => return Err(Error::protocol("bad option tag")),
                };
                let credential = d.bytes(4096)?.to_vec();
                Request::Hello(Hello {
                    min_version,
                    max_version,
                    device,
                    project,
                    credential,
                })
            }
            2 => Request::ListRefs {
                prefix: d.string(MAX_NAME)?.to_string(),
            },
            3 => {
                let s = d.bytes(16)?;
                let session = match s.len() {
                    0 => None,
                    16 => Some(s.try_into().unwrap()),
                    _ => return Err(Error::protocol("session id must be 16 bytes")),
                };
                Request::Have {
                    session,
                    ids: get_ids(&mut d)?,
                }
            }
            4 => {
                let session = get_session(&mut d)?;
                let ref_name = d.string(MAX_NAME)?.to_string();
                let expected_gen = d.varint()?;
                let new_head = d.id()?;
                let force = d.u8()? != 0;
                let object_count = d.varint()?;
                let byte_estimate = d.varint()?;
                let expected = get_opt_id(&mut d)?;
                Request::Begin(BeginReq {
                    session,
                    ref_name,
                    expected,
                    expected_gen,
                    new_head,
                    force,
                    object_count,
                    byte_estimate,
                })
            }
            5 => {
                let session = get_session(&mut d)?;
                Request::Put {
                    session,
                    objects: get_objects(&mut d)?,
                }
            }
            6 => {
                let session = get_session(&mut d)?;
                let ref_name = d.string(MAX_NAME)?.to_string();
                let expected_gen = d.varint()?;
                let force = d.u8()? != 0;
                let expected = get_opt_id(&mut d)?;
                let new = get_opt_id(&mut d)?;
                Request::UpdateRef(UpdateRefReq {
                    session,
                    ref_name,
                    expected,
                    expected_gen,
                    new,
                    force,
                })
            }
            7 => Request::Cancel {
                session: get_session(&mut d)?,
                discard: d.u8()? != 0,
            },
            8 => Request::Plan {
                wants: get_ids(&mut d)?,
                haves: get_ids(&mut d)?,
            },
            9 => Request::PlanPage {
                cursor: d.varint()?,
                max: d.varint()?.min(u32::MAX as u64) as u32,
            },
            10 => {
                let allow_delta = d.u8()? != 0;
                Request::Get {
                    ids: get_ids(&mut d)?,
                    allow_delta,
                }
            }
            11 => Request::Watch {
                since: d.varint()?,
                timeout_ms: d.varint()?.min(u32::MAX as u64) as u32,
            },
            12 => Request::Bye,
            t => return Err(Error::protocol(format!("unknown request tag {t}"))),
        };
        d.finish()?;
        Ok(r)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Request::Hello(_) => "Hello",
            Request::ListRefs { .. } => "ListRefs",
            Request::Have { .. } => "Have",
            Request::Begin(_) => "Begin",
            Request::Put { .. } => "Put",
            Request::UpdateRef(_) => "UpdateRef",
            Request::Cancel { .. } => "Cancel",
            Request::Plan { .. } => "Plan",
            Request::PlanPage { .. } => "PlanPage",
            Request::Get { .. } => "Get",
            Request::Watch { .. } => "Watch",
            Request::Bye => "Bye",
        }
    }
}

// ---- Response ---------------------------------------------------------------------------

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match self {
            Response::HelloOk(h) => {
                e.u8(101)
                    .varint(h.version as u64)
                    .string(&h.repo_uuid)
                    .varint(h.limits.max_frame as u64)
                    .varint(h.limits.max_batch_objects as u64)
                    .varint(h.limits.max_batch_bytes as u64)
                    .varint(h.limits.max_object as u64)
                    .varint(h.limits.max_ids_per_request as u64)
                    .u8(h.policy.allow_force as u8)
                    .u8(h.policy.allow_delete as u8)
                    .string(&h.policy.default_branch)
                    .varint(h.ref_seq)
                    .u8(h.role.map(|r| r as u8).unwrap_or(0));
            }
            Response::Refs { records, seq } => {
                e.u8(102).varint(*seq).varint(records.len() as u64);
                for r in records {
                    e.string(&r.name).varint(r.generation);
                    opt_id(&mut e, &r.commit);
                }
            }
            Response::HaveResult(states) => {
                e.u8(103).varint(states.len() as u64);
                for s in states {
                    e.u8(*s as u8);
                }
            }
            Response::BeginOk {
                staged_objects,
                staged_bytes,
            } => {
                e.u8(104).varint(*staged_objects).varint(*staged_bytes);
            }
            Response::PutAck {
                accepted,
                already_staged,
                staged_objects,
                staged_bytes,
            } => {
                e.u8(105)
                    .varint(*accepted as u64)
                    .varint(*already_staged as u64)
                    .varint(*staged_objects)
                    .varint(*staged_bytes);
            }
            Response::RefUpdated {
                generation,
                already_applied,
            } => {
                e.u8(106).varint(*generation).u8(*already_applied as u8);
            }
            Response::Cancelled => {
                e.u8(107);
            }
            Response::PlanOk { objects, commits } => {
                e.u8(108).varint(*objects).varint(*commits);
            }
            Response::PlanPage { ids, end } => {
                e.u8(109).u8(*end as u8);
                put_ids(&mut e, ids);
            }
            Response::Objects { objects, remaining } => {
                e.u8(110).varint(*remaining as u64);
                put_objects(&mut e, objects);
            }
            Response::WatchResult { seq, changed } => {
                e.u8(111).varint(*seq).u8(*changed as u8);
            }
            Response::Error(w) => {
                e.u8(112);
                w.encode(&mut e);
            }
        }
        e.finish()
    }

    pub fn decode(payload: &[u8]) -> Result<Response> {
        let mut d = Decoder::new(payload);
        let tag = d.u8()?;
        let r = match tag {
            101 => {
                let version = d.varint()?.min(u16::MAX as u64) as u16;
                let repo_uuid = d.string(256)?.to_string();
                let mut lim = [0u32; 5];
                for l in lim.iter_mut() {
                    *l = d.varint()?.min(u32::MAX as u64) as u32;
                }
                let allow_force = d.u8()? != 0;
                let allow_delete = d.u8()? != 0;
                let default_branch = d.string(256)?.to_string();
                Response::HelloOk(HelloOk {
                    version,
                    repo_uuid,
                    limits: Limits {
                        max_frame: lim[0],
                        max_batch_objects: lim[1],
                        max_batch_bytes: lim[2],
                        max_object: lim[3],
                        max_ids_per_request: lim[4],
                    },
                    policy: Policy {
                        allow_force,
                        allow_delete,
                        default_branch,
                    },
                    ref_seq: d.varint()?,
                    // trailing field; absent in hosts that predate roles
                    role: if d.remaining() > 0 {
                        match d.u8()? {
                            0 => None,
                            r => Some(
                                crate::identity::Role::from_u8(r)
                                    .ok_or_else(|| Error::protocol("unknown role"))?,
                            ),
                        }
                    } else {
                        None
                    },
                })
            }
            102 => {
                let seq = d.varint()?;
                let n = d.varint()?;
                if n > MAX_REF_RECORDS as u64 {
                    return Err(Error::protocol("too many ref records"));
                }
                let mut records = Vec::with_capacity(n.min(1024) as usize);
                for _ in 0..n {
                    let name = d.string(MAX_NAME)?.to_string();
                    let generation = d.varint()?;
                    let commit = get_opt_id(&mut d)?;
                    records.push(RefRecord {
                        name,
                        commit,
                        generation,
                    });
                }
                Response::Refs { records, seq }
            }
            103 => {
                let n = d.varint()?;
                if n > MAX_IDS_PER_MESSAGE as u64 || n as usize > d.remaining() {
                    return Err(Error::protocol("have-result too long"));
                }
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    v.push(match d.u8()? {
                        0 => HaveState::Missing,
                        1 => HaveState::Complete,
                        2 => HaveState::Staged,
                        _ => return Err(Error::protocol("bad have state")),
                    });
                }
                Response::HaveResult(v)
            }
            104 => Response::BeginOk {
                staged_objects: d.varint()?,
                staged_bytes: d.varint()?,
            },
            105 => Response::PutAck {
                accepted: d.varint()?.min(u32::MAX as u64) as u32,
                already_staged: d.varint()?.min(u32::MAX as u64) as u32,
                staged_objects: d.varint()?,
                staged_bytes: d.varint()?,
            },
            106 => Response::RefUpdated {
                generation: d.varint()?,
                already_applied: d.u8()? != 0,
            },
            107 => Response::Cancelled,
            108 => Response::PlanOk {
                objects: d.varint()?,
                commits: d.varint()?,
            },
            109 => {
                let end = d.u8()? != 0;
                Response::PlanPage {
                    ids: get_ids(&mut d)?,
                    end,
                }
            }
            110 => {
                let remaining = d.varint()?.min(u32::MAX as u64) as u32;
                Response::Objects {
                    objects: get_objects(&mut d)?,
                    remaining,
                }
            }
            111 => Response::WatchResult {
                seq: d.varint()?,
                changed: d.u8()? != 0,
            },
            112 => Response::Error(WireError::decode(&mut d)?),
            t => return Err(Error::protocol(format!("unknown response tag {t}"))),
        };
        d.finish()?;
        Ok(r)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Response::HelloOk(_) => "HelloOk",
            Response::Refs { .. } => "Refs",
            Response::HaveResult(_) => "HaveResult",
            Response::BeginOk { .. } => "BeginOk",
            Response::PutAck { .. } => "PutAck",
            Response::RefUpdated { .. } => "RefUpdated",
            Response::Cancelled => "Cancelled",
            Response::PlanOk { .. } => "PlanOk",
            Response::PlanPage { .. } => "PlanPage",
            Response::Objects { .. } => "Objects",
            Response::WatchResult { .. } => "WatchResult",
            Response::Error(_) => "Error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgit_store::hash_object;

    fn id(n: u8) -> ObjectId {
        ObjectId([n; 32])
    }

    fn obj(data: &[u8]) -> WireObject {
        WireObject::full(ObjectType::Chunk, hash_object(ObjectType::Chunk, data), data.to_vec())
    }

    #[test]
    fn requests_roundtrip() {
        let reqs = vec![
            Request::Hello(Hello {
                min_version: 1,
                max_version: 2,
                device: "laptop".into(),
                project: Some("uuid".into()),
                credential: vec![1, 2, 3],
            }),
            Request::ListRefs {
                prefix: "heads/".into(),
            },
            Request::Have {
                session: Some([3; 16]),
                ids: vec![id(1), id(2)],
            },
            Request::Have {
                session: None,
                ids: vec![],
            },
            Request::Begin(BeginReq {
                session: [9; 16],
                ref_name: "heads/main".into(),
                expected: Some(id(4)),
                expected_gen: 3,
                new_head: id(5),
                force: false,
                object_count: 10,
                byte_estimate: 1000,
            }),
            Request::Put {
                session: [1; 16],
                objects: vec![obj(b"hello"), obj(&vec![b'a'; 5000])],
            },
            Request::UpdateRef(UpdateRefReq {
                session: [1; 16],
                ref_name: "heads/x".into(),
                expected: None,
                expected_gen: 0,
                new: Some(id(8)),
                force: true,
            }),
            Request::Cancel {
                session: [2; 16],
                discard: true,
            },
            Request::Plan {
                wants: vec![id(1)],
                haves: vec![id(2), id(3)],
            },
            Request::PlanPage { cursor: 7, max: 100 },
            Request::Get {
                ids: vec![id(1)],
                allow_delta: true,
            },
            Request::Watch {
                since: 5,
                timeout_ms: 1000,
            },
            Request::Bye,
        ];
        for r in reqs {
            assert_eq!(Request::decode(&r.encode()).unwrap(), r, "{}", r.name());
        }
    }

    #[test]
    fn responses_roundtrip() {
        let resps = vec![
            Response::HelloOk(HelloOk {
                version: 1,
                repo_uuid: "u".into(),
                limits: Limits::default(),
                policy: Policy {
                    allow_force: false,
                    allow_delete: true,
                    default_branch: "main".into(),
                },
                ref_seq: 4,
                role: Some(crate::identity::Role::Write),
            }),
            Response::Refs {
                records: vec![
                    RefRecord {
                        name: "heads/a".into(),
                        commit: Some(id(1)),
                        generation: 2,
                    },
                    RefRecord {
                        name: "heads/b".into(),
                        commit: None,
                        generation: 5,
                    },
                ],
                seq: 9,
            },
            Response::HaveResult(vec![
                HaveState::Missing,
                HaveState::Complete,
                HaveState::Staged,
            ]),
            Response::BeginOk {
                staged_objects: 1,
                staged_bytes: 2,
            },
            Response::PutAck {
                accepted: 3,
                already_staged: 1,
                staged_objects: 4,
                staged_bytes: 5,
            },
            Response::RefUpdated {
                generation: 3,
                already_applied: true,
            },
            Response::Cancelled,
            Response::PlanOk {
                objects: 5,
                commits: 1,
            },
            Response::PlanPage {
                ids: vec![id(1), id(2)],
                end: true,
            },
            Response::Objects {
                objects: vec![obj(b"x"), obj(&vec![b'z'; 4000])],
                remaining: 2,
            },
            Response::WatchResult {
                seq: 1,
                changed: false,
            },
            Response::Error(WireError::new(crate::ErrorCode::Busy, "later")),
        ];
        for r in resps {
            assert_eq!(Response::decode(&r.encode()).unwrap(), r, "{}", r.name());
        }
    }

    #[test]
    fn malformed_messages_are_rejected_not_panicked() {
        assert!(Request::decode(&[]).is_err());
        assert!(Request::decode(&[99]).is_err());
        assert!(Response::decode(&[1]).is_err());
        // id list claiming more entries than bytes present
        assert!(Request::decode(&[10, 0xff, 0xff, 0x03]).is_err());
        // trailing bytes
        let mut b = Request::Bye.encode();
        b.push(0);
        assert!(Request::decode(&b).is_err());
        // truncation of every valid message must be an error
        let full = Request::Put {
            session: [1; 16],
            objects: vec![obj(b"hello world")],
        }
        .encode();
        for n in 0..full.len() {
            assert!(Request::decode(&full[..n]).is_err(), "prefix {n}");
        }
    }

    #[test]
    fn delta_objects_roundtrip_and_resolve() {
        let base: Vec<u8> = (0..4000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        let mut newer = base.clone();
        newer[1500] ^= 0xff;
        let bid = hash_object(ObjectType::Chunk, &base);
        let nid = hash_object(ObjectType::Chunk, &newer);
        let w = WireObject::delta(ObjectType::Chunk, nid, &newer, bid, &base).expect("worth it");
        assert!(w.data.len() < 200, "{}", w.data.len());
        let req = Request::Put {
            session: [1; 16],
            objects: vec![w.clone()],
        };
        let Request::Put { objects, .. } = Request::decode(&req.encode()).unwrap() else {
            panic!()
        };
        assert_eq!(objects[0], w);
        let back = objects[0].logical(Some(&base), 1 << 20).unwrap();
        assert_eq!(back, newer);
        assert_eq!(hash_object(ObjectType::Chunk, &back), nid);
        // Wrong base never yields bytes that pass the id check; missing base is an error.
        let wrong = vec![7u8; base.len()];
        if let Ok(b) = objects[0].logical(Some(&wrong), 1 << 20) {
            assert_ne!(hash_object(ObjectType::Chunk, &b), nid);
        }
        assert!(objects[0].logical(None, 1 << 20).is_err());
        // The decoder honors the size cap.
        assert!(objects[0].logical(Some(&base), 100).is_err());
        // Incompressible-against-base data is not sent as a delta.
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let random: Vec<u8> = (0..4000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        assert!(WireObject::delta(ObjectType::Chunk, nid, &random, bid, &base).is_none());
    }

    #[test]
    fn decompression_bomb_is_bounded() {
        // A batch that claims a small raw length but decompresses larger must fail.
        let big = vec![0u8; 1 << 20];
        let c = zstd::bulk::compress(&big, 3).unwrap();
        let mut e = Encoder::new();
        e.u8(5).bytes(&[0; 16]).varint(1).u8(1).varint(100).bytes(&c);
        assert!(Request::decode(&e.finish()).is_err());
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    #[test]
    fn credential_roundtrip_and_strictness() {
        let c = Credential::Enroll { invitation: [1; 16], secret: [2; 32] };
        assert_eq!(Credential::decode(&c.encode()).unwrap(), c);
        assert_eq!(Credential::decode(&[]).unwrap(), Credential::None);
        assert!(Credential::decode(&[1, 2, 3]).is_err());
        assert!(Credential::decode(&[9; 49]).is_err());
        assert!(!format!("{c:?}").contains('2'));
    }

    #[test]
    fn hello_debug_hides_the_credential() {
        let h = Hello {
            min_version: 1,
            max_version: 1,
            device: "d".into(),
            project: None,
            credential: Credential::Enroll { invitation: [1; 16], secret: [0x5a; 32] }.encode(),
        };
        let s = format!("{h:?}");
        assert!(s.contains("redacted") && !s.contains("90") && !s.contains("5a"));
    }
}
