//! Host-side authorization records: enrolled devices, invitations and the host key's public
//! side. Kept in `.rgit/host/authz.sqlite`, separate from the immutable repository content, so
//! `rgit invite|devices` can change it while `rgit serve` is running (SQLite serializes the
//! writers across processes) and the server re-reads it on every request.
//!
//! Secrets are never stored: an invitation keeps only a domain-separated hash of its
//! enrollment secret, and devices are identified by public key.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use rustgit_protocol::identity::random_bytes;
use rustgit_protocol::{
    EnrollmentSecret, ErrorCode, Fingerprint, PublicKey, Role, Succession, WireError,
};

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("authorization database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Ambiguous(String),
    #[error("{0}")]
    Invalid(String),
}

pub type AuthResult<T> = Result<T, AuthError>;

#[derive(Clone, Debug)]
pub struct DeviceRecord {
    pub key: PublicKey,
    pub fingerprint: Fingerprint,
    pub name: String,
    pub role: Role,
    pub added: u64,
    pub last_seen: Option<u64>,
    pub revoked_at: Option<u64>,
    /// The invitation this device enrolled with.
    pub invitation: Option<[u8; 16]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvitationStatus {
    Active,
    Expired,
    Used,
    Revoked,
}

impl InvitationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InvitationStatus::Active => "active",
            InvitationStatus::Expired => "expired",
            InvitationStatus::Used => "used",
            InvitationStatus::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Debug)]
pub struct InvitationRecord {
    pub id: [u8; 16],
    pub role: Role,
    pub created: u64,
    pub expires: u64,
    pub max_uses: u64,
    pub uses: u64,
    pub revoked_at: Option<u64>,
    pub label: String,
}

impl InvitationRecord {
    pub fn status(&self, now: u64) -> InvitationStatus {
        if self.revoked_at.is_some() {
            InvitationStatus::Revoked
        } else if self.uses >= self.max_uses {
            InvitationStatus::Used
        } else if now >= self.expires {
            InvitationStatus::Expired
        } else {
            InvitationStatus::Active
        }
    }

    pub fn id_hex(&self) -> String {
        rustgit_protocol::identity::hex_of(&self.id)
    }
}

/// secret hash, role, expires, max uses, uses, revoked-at
type InvitationRow = (Vec<u8>, i64, i64, i64, i64, Option<i64>);

pub struct AuthStore {
    db: Mutex<Connection>,
    path: PathBuf,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS devices(
    fp TEXT PRIMARY KEY,
    key BLOB NOT NULL,
    name TEXT NOT NULL,
    role INTEGER NOT NULL,
    added INTEGER NOT NULL,
    last_seen INTEGER,
    revoked_at INTEGER,
    invitation BLOB
);
CREATE TABLE IF NOT EXISTS invitations(
    id BLOB PRIMARY KEY,
    secret_hash BLOB NOT NULL,
    role INTEGER NOT NULL,
    created INTEGER NOT NULL,
    expires INTEGER NOT NULL,
    max_uses INTEGER NOT NULL,
    uses INTEGER NOT NULL DEFAULT 0,
    revoked_at INTEGER,
    label TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS successions(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    link BLOB NOT NULL
);
";

fn wire(code: ErrorCode, m: &str) -> WireError {
    WireError::new(code, m)
}

fn clean_name(n: &str) -> String {
    let n: String = n.chars().filter(|c| !c.is_control()).take(128).collect();
    let n = n.trim();
    if n.is_empty() { "device".into() } else { n.into() }
}

impl AuthStore {
    /// Opens (creating if needed) `<repo root>/.rgit/host/authz.sqlite`.
    pub fn open(repo_root: &Path) -> AuthResult<AuthStore> {
        let dir = repo_root.join(".rgit").join("host");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("authz.sqlite");
        let db = Connection::open(&path)?;
        db.busy_timeout(std::time::Duration::from_secs(10))?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
        db.execute_batch(SCHEMA)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(AuthStore {
            db: Mutex::new(db),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // ---- host key (public side) ------------------------------------------------------------

    pub fn host_public(&self) -> AuthResult<Option<PublicKey>> {
        let db = self.db.lock().unwrap();
        let v: Option<Vec<u8>> = db
            .query_row("SELECT v FROM meta WHERE k='host_pub'", [], |r| r.get(0))
            .optional()?;
        Ok(v.and_then(|b| b.try_into().ok()).map(PublicKey))
    }

    pub fn successions(&self) -> AuthResult<Vec<Succession>> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT link FROM successions ORDER BY seq")?;
        let rows = st.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut v = Vec::new();
        for r in rows {
            if let Some(s) = Succession::from_bytes(&r?) {
                v.push(s);
            }
        }
        Ok(v)
    }

    /// Records the host's public key. `link` is the signed succession from the previous key
    /// (`None` for first creation or when trust is deliberately broken); `drop_chain` forgets
    /// earlier links so clients pinned to old keys must re-enroll.
    pub fn set_host_public(
        &self,
        key: &PublicKey,
        link: Option<Succession>,
        drop_chain: bool,
    ) -> AuthResult<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR REPLACE INTO meta(k, v) VALUES('host_pub', ?)",
            [&key.0[..]],
        )?;
        if drop_chain {
            tx.execute("DELETE FROM successions", [])?;
        }
        if let Some(l) = link {
            tx.execute("INSERT INTO successions(link) VALUES(?)", [&l.to_bytes()[..]])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Small free-form facts (for example the address `serve` last listened on).
    pub fn set_meta(&self, key: &str, value: &str) -> AuthResult<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT OR REPLACE INTO meta(k, v) VALUES(?, ?)",
            params![format!("x:{key}"), value.as_bytes()],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> AuthResult<Option<String>> {
        let db = self.db.lock().unwrap();
        let v: Option<Vec<u8>> = db
            .query_row("SELECT v FROM meta WHERE k = ?", [format!("x:{key}")], |r| r.get(0))
            .optional()?;
        Ok(v.and_then(|b| String::from_utf8(b).ok()))
    }

    // ---- invitations -----------------------------------------------------------------------

    /// Creates an invitation and returns its id and the one-time secret (not recoverable
    /// afterwards).
    pub fn create_invitation(
        &self,
        role: Role,
        ttl_secs: u64,
        max_uses: u64,
        label: &str,
        now: u64,
    ) -> AuthResult<([u8; 16], EnrollmentSecret)> {
        if ttl_secs == 0 || max_uses == 0 {
            return Err(AuthError::Invalid(
                "an invitation needs a positive lifetime and at least one use".into(),
            ));
        }
        let mut id = [0u8; 16];
        random_bytes(&mut id);
        let secret = EnrollmentSecret::generate();
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO invitations(id, secret_hash, role, created, expires, max_uses, label)
             VALUES(?,?,?,?,?,?,?)",
            params![
                &id[..],
                &secret.hash()[..],
                role as u8,
                now as i64,
                now.saturating_add(ttl_secs) as i64,
                max_uses as i64,
                label
            ],
        )?;
        Ok((id, secret))
    }

    fn inv_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<InvitationRecord> {
        let id: Vec<u8> = r.get(0)?;
        Ok(InvitationRecord {
            id: id.try_into().unwrap_or([0; 16]),
            role: Role::from_u8(r.get::<_, i64>(1)? as u8).unwrap_or(Role::Read),
            created: r.get::<_, i64>(2)? as u64,
            expires: r.get::<_, i64>(3)? as u64,
            max_uses: r.get::<_, i64>(4)? as u64,
            uses: r.get::<_, i64>(5)? as u64,
            revoked_at: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
            label: r.get(7)?,
        })
    }

    const INV_COLS: &'static str = "id, role, created, expires, max_uses, uses, revoked_at, label";

    pub fn list_invitations(&self) -> AuthResult<Vec<InvitationRecord>> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare(&format!(
            "SELECT {} FROM invitations ORDER BY created, rowid",
            Self::INV_COLS
        ))?;
        let rows = st.query_map([], Self::inv_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Revokes by full id or unique hex prefix (>= 6 digits). Used invitations can be revoked
    /// too (no effect on devices already enrolled; use `revoke_device` for those).
    pub fn revoke_invitation(&self, selector: &str, now: u64) -> AuthResult<InvitationRecord> {
        let all = self.list_invitations()?;
        let sel = selector.trim().to_ascii_lowercase();
        if sel.len() < 6 {
            return Err(AuthError::Invalid(
                "give at least the first 6 characters of the invitation id".into(),
            ));
        }
        let hits: Vec<_> = all.iter().filter(|i| i.id_hex().starts_with(&sel)).collect();
        let rec = match hits.as_slice() {
            [] => return Err(AuthError::NotFound("no such invitation".into())),
            [one] => (*one).clone(),
            _ => return Err(AuthError::Ambiguous("that prefix matches several invitations".into())),
        };
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE invitations SET revoked_at = COALESCE(revoked_at, ?) WHERE id = ?",
            params![now as i64, &rec.id[..]],
        )?;
        drop(db);
        Ok(InvitationRecord {
            revoked_at: rec.revoked_at.or(Some(now)),
            ..rec
        })
    }

    /// Redeems an invitation for `key`. Atomic: with any number of concurrent attempts a
    /// single-use invitation enrolls exactly one device. Unknown id and wrong secret are
    /// indistinguishable (`InvitationInvalid`); the specific failure (expired, used, revoked)
    /// is only reported to a caller that proved knowledge of the secret.
    pub fn redeem(
        &self,
        id: &[u8; 16],
        secret: &[u8; 32],
        key: &PublicKey,
        name: &str,
        now: u64,
    ) -> Result<DeviceRecord, WireError> {
        let invalid = || wire(ErrorCode::InvitationInvalid, "this invitation is not valid");
        let mut db = self.db.lock().unwrap();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_wire)?;
        let inv: Option<InvitationRow> = tx
            .query_row(
                "SELECT secret_hash, role, expires, max_uses, uses, revoked_at
                 FROM invitations WHERE id = ?",
                [&id[..]],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(sql_wire)?;
        let Some((stored, role, expires, max_uses, uses, revoked_at)) = inv else {
            return Err(invalid());
        };
        let stored: [u8; 32] = stored.try_into().map_err(|_| invalid())?;
        let presented = EnrollmentSecret::from_bytes(*secret).hash();
        // blake3::Hash equality is constant-time
        if blake3::Hash::from(stored) != blake3::Hash::from(presented) {
            return Err(invalid());
        }
        let role = Role::from_u8(role as u8).ok_or_else(invalid)?;
        let fp = key.fingerprint();
        let existing = device_row(&tx, &fp).map_err(sql_wire)?;
        if let Some(d) = &existing {
            if d.revoked_at.is_some() {
                return Err(wire(
                    ErrorCode::DeviceRevoked,
                    "this device's access was revoked; it cannot enroll again with this key",
                ));
            }
            if d.invitation == Some(*id) {
                // the same device retrying after a lost reply: already enrolled
                return Ok(d.clone());
            }
        }
        if revoked_at.is_some() {
            return Err(wire(ErrorCode::InvitationRevoked, "this invitation was revoked"));
        }
        if now as i64 >= expires {
            return Err(wire(ErrorCode::InvitationExpired, "this invitation has expired"));
        }
        let changed = tx
            .execute(
                "UPDATE invitations SET uses = uses + 1 WHERE id = ? AND uses < ?",
                params![&id[..], max_uses],
            )
            .map_err(sql_wire)?;
        if changed != 1 || uses >= max_uses {
            return Err(wire(
                ErrorCode::InvitationUsed,
                "this invitation was already used",
            ));
        }
        let name = clean_name(name);
        let rec = match existing {
            Some(mut d) => {
                d.role = d.role.max(role);
                d.invitation = Some(*id);
                tx.execute(
                    "UPDATE devices SET role = ?, invitation = ?, last_seen = ? WHERE fp = ?",
                    params![d.role as u8, &id[..], now as i64, d.fingerprint.to_hex()],
                )
                .map_err(sql_wire)?;
                d
            }
            None => {
                tx.execute(
                    "INSERT INTO devices(fp, key, name, role, added, last_seen, invitation)
                     VALUES(?,?,?,?,?,?,?)",
                    params![fp.to_hex(), &key.0[..], name, role as u8, now as i64, now as i64, &id[..]],
                )
                .map_err(sql_wire)?;
                DeviceRecord {
                    key: *key,
                    fingerprint: fp,
                    name,
                    role,
                    added: now,
                    last_seen: Some(now),
                    revoked_at: None,
                    invitation: Some(*id),
                }
            }
        };
        tx.commit().map_err(sql_wire)?;
        Ok(rec)
    }

    // ---- devices ---------------------------------------------------------------------------

    pub fn device(&self, fp: &Fingerprint) -> AuthResult<Option<DeviceRecord>> {
        let db = self.db.lock().unwrap();
        Ok(device_row(&db, fp)?)
    }

    /// The per-request check: the device must be enrolled, not revoked, and its role must
    /// allow `needed`. Reads the database every time, so revocation takes effect on the very
    /// next request of an open connection.
    pub fn check(&self, fp: &Fingerprint, needed: Role) -> Result<DeviceRecord, WireError> {
        let d = self
            .device(fp)
            .map_err(|e| WireError::new(ErrorCode::Internal, format!("authorization store: {e}")))?
            .ok_or_else(|| {
                wire(
                    ErrorCode::UnknownDevice,
                    "this device is not enrolled on this host; ask for an invitation",
                )
            })?;
        if d.revoked_at.is_some() {
            return Err(wire(ErrorCode::DeviceRevoked, "this device's access was revoked"));
        }
        if !d.role.allows(needed) {
            return Err(wire(
                ErrorCode::Forbidden,
                &format!("role '{}' does not allow this; '{}' is required", d.role, needed),
            ));
        }
        Ok(d)
    }

    pub fn touch(&self, fp: &Fingerprint, now: u64) {
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE devices SET last_seen = ? WHERE fp = ?",
            params![now as i64, fp.to_hex()],
        );
    }

    pub fn list_devices(&self) -> AuthResult<Vec<DeviceRecord>> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare(
            "SELECT fp, key, name, role, added, last_seen, revoked_at, invitation
             FROM devices ORDER BY added, rowid",
        )?;
        let rows = st.query_map([], device_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Finds a device by fingerprint (>= 6 hex digits of a unique prefix) or exact name.
    pub fn find_device(&self, selector: &str) -> AuthResult<DeviceRecord> {
        let all = self.list_devices()?;
        let sel: String = selector
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !matches!(c, '-' | ':' | ' '))
            .collect();
        let by_fp: Vec<_> = if sel.len() >= 6 && sel.chars().all(|c| c.is_ascii_hexdigit()) {
            all.iter().filter(|d| d.fingerprint.to_hex().starts_with(&sel)).collect()
        } else {
            vec![]
        };
        let hits = if by_fp.is_empty() {
            all.iter().filter(|d| d.name == selector.trim()).collect()
        } else {
            by_fp
        };
        match hits.as_slice() {
            [] => Err(AuthError::NotFound(
                "no such device (give 6+ digits of its fingerprint, or its exact name)".into(),
            )),
            [one] => Ok((*one).clone()),
            _ => Err(AuthError::Ambiguous(
                "that matches several devices; use more fingerprint digits".into(),
            )),
        }
    }

    /// Revokes future access. Data the device already fetched stays with it.
    pub fn revoke_device(&self, selector: &str, now: u64) -> AuthResult<DeviceRecord> {
        let d = self.find_device(selector)?;
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE devices SET revoked_at = COALESCE(revoked_at, ?) WHERE fp = ?",
            params![now as i64, d.fingerprint.to_hex()],
        )?;
        Ok(DeviceRecord {
            revoked_at: d.revoked_at.or(Some(now)),
            ..d
        })
    }

    pub fn set_role(&self, selector: &str, role: Role) -> AuthResult<DeviceRecord> {
        let d = self.find_device(selector)?;
        if d.revoked_at.is_some() {
            return Err(AuthError::Invalid("that device is revoked".into()));
        }
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE devices SET role = ? WHERE fp = ?",
            params![role as u8, d.fingerprint.to_hex()],
        )?;
        Ok(DeviceRecord { role, ..d })
    }

    /// Revokes every invitation that can still be used (after a suspected host key
    /// compromise, for example).
    pub fn revoke_all_invitations(&self, now: u64) -> AuthResult<usize> {
        let db = self.db.lock().unwrap();
        Ok(db.execute(
            "UPDATE invitations SET revoked_at = ? WHERE revoked_at IS NULL",
            [now as i64],
        )?)
    }
}

fn sql_wire(e: rusqlite::Error) -> WireError {
    WireError::new(ErrorCode::Internal, format!("authorization store: {e}"))
}

fn device_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRecord> {
    let key: Vec<u8> = r.get(1)?;
    let key = PublicKey(key.try_into().unwrap_or([0; 32]));
    let inv: Option<Vec<u8>> = r.get(7)?;
    Ok(DeviceRecord {
        fingerprint: key.fingerprint(),
        key,
        name: r.get(2)?,
        role: Role::from_u8(r.get::<_, i64>(3)? as u8).unwrap_or(Role::Read),
        added: r.get::<_, i64>(4)? as u64,
        last_seen: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        revoked_at: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
        invitation: inv.and_then(|b| b.try_into().ok()),
    })
}

fn device_row(db: &Connection, fp: &Fingerprint) -> rusqlite::Result<Option<DeviceRecord>> {
    db.query_row(
        "SELECT fp, key, name, role, added, last_seen, revoked_at, invitation
         FROM devices WHERE fp = ?",
        [fp.to_hex()],
        device_from_row,
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgit_protocol::DeviceKey;
    use std::sync::Arc;

    fn store() -> (tempfile::TempDir, Arc<AuthStore>) {
        let d = tempfile::tempdir().unwrap();
        let s = Arc::new(AuthStore::open(d.path()).unwrap());
        (d, s)
    }

    #[test]
    fn single_use_invitation_enrolls_exactly_one_device_under_a_race() {
        let (d, s) = store();
        let (id, secret) = s.create_invitation(Role::Write, 3600, 1, "t", 1000).unwrap();
        let keys: Vec<_> = (0..24).map(|_| DeviceKey::generate()).collect();
        // separate connections to the same file, like separate processes would have
        let handles: Vec<_> = keys
            .iter()
            .map(|k| {
                let root = d.path().to_path_buf();
                let pubkey = k.public();
                let sec = *secret.expose();
                std::thread::spawn(move || {
                    let st = AuthStore::open(&root).unwrap();
                    st.redeem(&id, &sec, &pubkey, "racer", 1001)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let ok = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok, 1, "exactly one redemption may win: {results:?}");
        for r in results.iter().filter_map(|r| r.as_ref().err()) {
            assert_eq!(r.code, ErrorCode::InvitationUsed);
        }
        assert_eq!(s.list_devices().unwrap().len(), 1);
    }

    #[test]
    fn failures_are_distinct_and_do_not_leak_state_without_the_secret() {
        let (_d, s) = store();
        let k = DeviceKey::generate().public();
        let (id, secret) = s.create_invitation(Role::Read, 100, 1, "", 1000).unwrap();
        let wrong = [7u8; 32];
        // unknown id and wrong secret look the same
        let unknown = s.redeem(&[9; 16], secret.expose(), &k, "n", 1001).unwrap_err();
        let badsecret = s.redeem(&id, &wrong, &k, "n", 1001).unwrap_err();
        assert_eq!(unknown.code, ErrorCode::InvitationInvalid);
        assert_eq!(badsecret.code, ErrorCode::InvitationInvalid);
        assert_eq!(unknown.message, badsecret.message);
        // expired (only visible with the right secret)
        let e = s.redeem(&id, secret.expose(), &k, "n", 1100).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvitationExpired);
        // wrong secret on an expired invitation still says "invalid"
        let e = s.redeem(&id, &wrong, &k, "n", 1100).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvitationInvalid);
        // success, then replay by another device
        s.redeem(&id, secret.expose(), &k, "n", 1050).unwrap();
        let other = DeviceKey::generate().public();
        let e = s.redeem(&id, secret.expose(), &other, "n", 1051).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvitationUsed);
        // the same device retrying is idempotent
        assert!(s.redeem(&id, secret.expose(), &k, "n", 1052).is_ok());
        // revoked invitation
        let (id2, sec2) = s.create_invitation(Role::Read, 100, 1, "", 1000).unwrap();
        s.revoke_invitation(&rustgit_protocol::identity::hex_of(&id2), 1001).unwrap();
        let e = s.redeem(&id2, sec2.expose(), &other, "n", 1002).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvitationRevoked);
    }

    #[test]
    fn revoked_devices_fail_checks_and_stay_revoked() {
        let (_d, s) = store();
        let k = DeviceKey::generate().public();
        let (id, sec) = s.create_invitation(Role::Write, 100, 5, "", 1000).unwrap();
        s.redeem(&id, sec.expose(), &k, "laptop", 1001).unwrap();
        assert!(s.check(&k.fingerprint(), Role::Write).is_ok());
        assert_eq!(s.check(&k.fingerprint(), Role::Admin).unwrap_err().code, ErrorCode::Forbidden);
        s.revoke_device("laptop", 1002).unwrap();
        assert_eq!(s.check(&k.fingerprint(), Role::Read).unwrap_err().code, ErrorCode::DeviceRevoked);
        // a fresh invitation does not resurrect a revoked key
        let e = s.redeem(&id, sec.expose(), &k, "laptop", 1003).unwrap_err();
        assert_eq!(e.code, ErrorCode::DeviceRevoked);
        assert_eq!(
            s.check(&DeviceKey::generate().fingerprint(), Role::Read).unwrap_err().code,
            ErrorCode::UnknownDevice
        );
    }

    #[test]
    fn only_secret_hashes_are_stored() {
        let (d, s) = store();
        let (_id, sec) = s.create_invitation(Role::Read, 100, 1, "", 1000).unwrap();
        drop(s);
        let raw = std::fs::read(d.path().join(".rgit/host/authz.sqlite")).unwrap();
        let needle = sec.expose();
        assert!(!raw.windows(32).any(|w| w == needle), "the enrollment secret was stored in clear");
    }
}
