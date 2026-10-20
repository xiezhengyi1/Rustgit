//! Device identity, invitations, enrollment and device management:
//! `rgit device`, `invite`, `join`, `devices`, `host`. Output follows docs/gui-interface.md
//! (JSON first; human text is a rendering of the same facts). Secrets are printed only where
//! they are the point (`invite create` prints the link); they never appear in logs, lists or
//! error messages.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use clap::{Args, Subcommand};
use rustgit_core::{Error, Repo};
use rustgit_host::authz::unix_now;
use rustgit_host::{
    ensure_host_identity, rotate_host_identity, AuthStore, DeviceRecord, Host, HostKeyError,
    InvitationRecord, KeyError, KeyStorage,
};
use rustgit_protocol::{
    clone_into, CancelToken, Client, ClientConfig, DeviceKey, ErrorCode, Fingerprint, Invitation,
    Role,
};
use serde_json::{json, Value};

use crate::{say, Cli, Cmd};

// ---- key storage selection ---------------------------------------------------------------------

struct KeyOpts {
    insecure_file: bool,
    dir: Option<PathBuf>,
}

static KEY_OPTS: OnceLock<KeyOpts> = OnceLock::new();
static WARNED: OnceLock<()> = OnceLock::new();

pub fn init_key_opts(insecure_file: bool, dir: Option<PathBuf>) {
    let _ = KEY_OPTS.set(KeyOpts { insecure_file, dir });
}

/// The platform key store, or - only when explicitly requested - the 0600 file store.
pub fn key_storage() -> KeyStorage {
    let o = KEY_OPTS.get();
    if o.is_some_and(|o| o.insecure_file) {
        let dir = o
            .and_then(|o| o.dir.clone())
            .unwrap_or_else(rustgit_host::keystore::default_key_dir);
        WARNED.get_or_init(|| {
            eprintln!(
                "warning: --insecure-file-keys: private keys are kept in plain files under {} \
                 (permissions 0600, not protected by the operating system)",
                dir.display()
            );
        });
        KeyStorage::File(dir)
    } else {
        KeyStorage::Platform
    }
}

fn storage_name(s: &KeyStorage) -> &'static str {
    s.name()
}

// ---- error codes ---------------------------------------------------------------------------------

/// Errors that carry a stable machine code are encoded as `[code] message` inside
/// `Error::Other`; `out::error_json` turns the prefix into the `code` field.
pub fn coded(code: &str, msg: impl std::fmt::Display) -> Error {
    Error::Other(format!("[{code}] {msg}"))
}

const CODES: &[&str] = &[
    "host_key_mismatch",
    "secure_channel_failed",
    "security_required",
    "not_enrolled",
    "device_revoked",
    "permission_denied",
    "invite_invalid",
    "invite_expired",
    "invite_used",
    "invite_revoked",
    "invite_malformed",
    "key_storage_unavailable",
    "no_such_device",
    "no_such_invitation",
    "ambiguous",
    "not_hosted",
    "host_serving",
    "host_key_unavailable",
];

/// Splits a `[code] message` error; `None` for ordinary messages.
pub fn split_code(m: &str) -> Option<(&'static str, &str)> {
    let rest = m.strip_prefix('[')?;
    let (code, msg) = rest.split_once("] ")?;
    CODES.iter().find(|c| **c == code).map(|c| (*c, msg))
}

fn key_err(e: KeyError) -> Error {
    match e {
        KeyError::Unavailable(_) => coded("key_storage_unavailable", e),
        e => Error::Other(e.to_string()),
    }
}

pub fn host_key_error(e: HostKeyError) -> Error {
    match e {
        HostKeyError::Key(k) => key_err(k),
        HostKeyError::Inconsistent(m) => coded("host_key_unavailable", m),
        HostKeyError::Store(s) => Error::Other(s.to_string()),
    }
}

fn store_err(e: rustgit_host::authz::AuthError) -> Error {
    use rustgit_host::authz::AuthError::*;
    match e {
        NotFound(m) => coded("no_such_device", m),
        Ambiguous(m) => coded("ambiguous", m),
        Invalid(m) => Error::Invalid(m),
        e => Error::Other(e.to_string()),
    }
}

/// Turns a protocol failure into a coded CLI error (used by `sync.rs::proto` as well).
pub fn tag_proto(e: &rustgit_protocol::Error) -> Option<Error> {
    use rustgit_protocol::Error as P;
    let code = match e {
        P::Remote(w) => match w.code {
            ErrorCode::UnknownDevice => "not_enrolled",
            ErrorCode::DeviceRevoked => "device_revoked",
            ErrorCode::Forbidden | ErrorCode::Unauthorized => "permission_denied",
            ErrorCode::InvitationInvalid => "invite_invalid",
            ErrorCode::InvitationExpired => "invite_expired",
            ErrorCode::InvitationUsed => "invite_used",
            ErrorCode::InvitationRevoked => "invite_revoked",
            ErrorCode::SecurityRequired => "security_required",
            ErrorCode::HandshakeFailed => "secure_channel_failed",
            _ => return None,
        },
        P::HostKeyMismatch { .. } => "host_key_mismatch",
        P::Handshake(_) => "secure_channel_failed",
        _ => return None,
    };
    let msg = match e {
        P::Remote(w) => w.message.clone(),
        P::HostKeyMismatch { pinned, presented } => format!(
            "the host at this address is not the one this remote trusts (expected identity {}, \
             got {}). Someone may be intercepting the connection; do not re-pin unless the host's \
             owner confirms the new fingerprint",
            pinned.short(),
            presented.short()
        ),
        other => other.to_string(),
    };
    Some(coded(code, msg))
}

// ---- commands ----------------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum DeviceCmd {
    /// Show this device's identity (creates it on first use)
    Id,
    /// Delete this device's private key. It can no longer authenticate to any host; hosts that
    /// enrolled it keep a record they should revoke
    Reset {
        /// Required: this cannot be undone
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum InviteCmd {
    /// Create an invitation link (run in the hosted project)
    Create {
        /// What the joining device may do: read, write or admin
        #[arg(long, default_value = "read")]
        role: String,
        /// Lifetime: 30m, 24h, 7d, or plain seconds
        #[arg(long, default_value = "24h")]
        expires: String,
        /// Allow a single redemption (default). `--single-use=false` allows several (see --max-uses)
        #[arg(long, num_args = 0..=1, default_missing_value = "true", default_value = "true")]
        single_use: bool,
        /// With `--single-use=false`: how many devices may redeem it
        #[arg(long, default_value_t = 10)]
        max_uses: u64,
        /// Where the joining device should try to connect (repeatable), e.g. tcp://192.168.1.5:7878.
        /// Default: the address `rgit serve` last listened on
        #[arg(long = "hint")]
        hints: Vec<String>,
        /// A note shown in `invite list`
        #[arg(long, default_value = "")]
        label: String,
    },
    /// List invitations (never shows secrets)
    List,
    /// Make an invitation unusable
    Revoke {
        /// The invitation id (or its first 6+ characters) from `invite list`
        id: String,
    },
}

#[derive(Args)]
pub struct JoinArgs {
    /// The invitation link (projectapp://join/...); `-` reads it from stdin, which keeps the
    /// secret out of the process list
    pub invitation: String,
    /// Where to clone (default: project-<id> in the current directory)
    pub dest: Option<PathBuf>,
    /// Instead of cloning, add the host as a remote of the project in --dir
    #[arg(long)]
    pub add_remote: bool,
    /// Remote name
    #[arg(long, default_value = "origin")]
    pub remote: String,
}

#[derive(Subcommand)]
pub enum DevicesCmd {
    /// List devices enrolled on this hosted project, with when they were last seen
    List,
    /// Revoke a device: it is refused from its next request on, and open connections end
    Revoke {
        /// First 6+ digits of the fingerprint, or the exact device name
        device: String,
    },
    /// Change a device's role
    Role { device: String, role: String },
}

#[derive(Subcommand)]
pub enum HostCmd {
    /// Show the host identity fingerprint
    Id,
    /// Replace the host key. Stop `rgit serve` first
    Rotate {
        /// The old key may be known to an attacker (or is lost): no succession link is made,
        /// unused invitations are revoked and every client must be given a new invitation
        #[arg(long)]
        compromised: bool,
    },
}

pub fn run(cli: Cli) -> Result<u8, Error> {
    match cli.cmd {
        Cmd::Device { action } => device(action),
        Cmd::Join(args) => join(&cli.dir, args),
        Cmd::Invite { action } => invite(&cli.dir, action),
        Cmd::Devices { action } => devices(&cli.dir, action),
        Cmd::Host { action } => host(&cli.dir, action),
        _ => unreachable!(),
    }
}

fn device_name() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into());
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into());
    format!("{user}@{host}")
}

pub const DEVICE_ACCOUNT: &str = "device";

/// This machine's device key; created on first use.
pub fn device_key() -> Result<(DeviceKey, bool), Error> {
    key_storage().load_or_create(DEVICE_ACCOUNT).map_err(key_err)
}

fn device(a: DeviceCmd) -> Result<u8, Error> {
    let ks = key_storage();
    match a {
        DeviceCmd::Id => {
            let (k, created) = ks.load_or_create(DEVICE_ACCOUNT).map_err(key_err)?;
            let fp = k.fingerprint();
            crate::result!(json!({
                "fingerprint": fp.to_hex(),
                "short": fp.short(),
                "public_key": k.public().to_hex(),
                "created": created,
                "key_storage": storage_name(&ks),
                "key_storage_protected": ks.is_protected(),
            }));
            say!("{}", fp.to_hex());
            if created {
                eprintln!("(new device identity created in {})", storage_name(&ks));
            }
        }
        DeviceCmd::Reset { yes } => {
            if !yes {
                return Err(Error::Invalid(
                    "this deletes the device's private key for good; pass --yes".into(),
                ));
            }
            ks.delete(DEVICE_ACCOUNT).map_err(key_err)?;
            crate::result!(json!({"deleted": true}));
            say!("Device identity deleted.");
        }
    }
    Ok(0)
}

// ---- hosted-project helpers ------------------------------------------------------------------------

struct Hosted {
    root: PathBuf,
    uuid: String,
    store: Arc<AuthStore>,
}

fn open_hosted(dir: &std::path::Path) -> Result<Hosted, Error> {
    let repo = Repo::discover(dir)?;
    if !Host::is_hosted(&repo.root) {
        return Err(coded(
            "not_hosted",
            "this command runs in a hosted project (create one with `rgit serve --init DIR`)",
        ));
    }
    let uuid = repo.uuid()?;
    let store = Arc::new(AuthStore::open(&repo.root).map_err(store_err)?);
    Ok(Hosted {
        root: repo.root.clone(),
        uuid,
        store,
    })
}

pub fn parse_duration(s: &str) -> Result<u64, Error> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86_400),
        Some(c) if c.is_ascii_digit() => (s, 1),
        _ => return Err(Error::Invalid(format!("bad lifetime '{s}' (use 30m, 24h, 7d)"))),
    };
    num.parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .and_then(|n| n.checked_mul(mult))
        .ok_or_else(|| Error::Invalid(format!("bad lifetime '{s}' (use 30m, 24h, 7d)")))
}

fn ago(now: u64, t: Option<u64>) -> String {
    let Some(t) = t else { return "never".into() };
    let d = now.saturating_sub(t);
    match d {
        0..=59 => format!("{d}s ago"),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86_399 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86_400),
    }
}

fn device_json(d: &DeviceRecord, now: u64) -> Value {
    json!({
        "fingerprint": d.fingerprint.to_hex(),
        "short": d.fingerprint.short(),
        "name": d.name,
        "role": d.role.as_str(),
        "status": if d.revoked_at.is_some() { "revoked" } else { "active" },
        "added": d.added,
        "last_seen": d.last_seen,
        "revoked_at": d.revoked_at,
        "seconds_since_seen": d.last_seen.map(|t| now.saturating_sub(t)),
    })
}

fn invitation_json(i: &InvitationRecord, now: u64) -> Value {
    json!({
        "id": i.id_hex(),
        "role": i.role.as_str(),
        "status": i.status(now).as_str(),
        "created": i.created,
        "expires": i.expires,
        "uses": i.uses,
        "max_uses": i.max_uses,
        "label": i.label,
    })
}

// ---- invite ---------------------------------------------------------------------------------------------

fn invite(dir: &std::path::Path, a: InviteCmd) -> Result<u8, Error> {
    let h = open_hosted(dir)?;
    let now = unix_now();
    match a {
        InviteCmd::Create {
            role,
            expires,
            single_use,
            max_uses,
            hints,
            label,
        } => {
            let role: Role = role.parse().map_err(Error::Invalid)?;
            let ttl = parse_duration(&expires)?;
            let uses = if single_use { 1 } else { max_uses.max(1) };
            let mut hints = hints;
            if hints.is_empty() {
                if let Some(addr) = h.store.get_meta("listen").map_err(store_err)? {
                    hints.push(format!("tcp://{addr}"));
                }
            }
            if hints.is_empty() {
                return Err(Error::Invalid(
                    "no connection hint: pass --hint tcp://HOST:PORT (the address other devices \
                     can reach), or start `rgit serve` once so its address is recorded"
                        .into(),
                ));
            }
            for hint in &hints {
                validate_hint(hint)?;
            }
            let ks = key_storage();
            let (hostkey, _) = ensure_host_identity(&h.uuid, &ks, &h.store).map_err(host_key_error)?;
            let (id, secret) = h
                .store
                .create_invitation(role, ttl, uses, &label, now)
                .map_err(store_err)?;
            let inv = Invitation {
                project: h.uuid.clone(),
                host: hostkey.fingerprint(),
                hints: hints.clone(),
                expires: now + ttl,
                role,
                id,
                secret,
            };
            let uri = inv.to_uri();
            crate::result!(json!({
                "uri": uri,
                "id": rustgit_protocol::identity::hex_of(&id),
                "role": role.as_str(),
                "expires": now + ttl,
                "max_uses": uses,
                "host_fingerprint": hostkey.fingerprint().to_hex(),
                "hints": hints,
                "project": h.uuid,
            }));
            say!("{uri}");
            eprintln!(
                "Invitation for role '{}', {} use(s), expires in {}. Host identity {}. \
                 Anyone with this link can join until then: share it privately.",
                role,
                uses,
                expires,
                hostkey.fingerprint().short()
            );
        }
        InviteCmd::List => {
            let list = h.store.list_invitations().map_err(store_err)?;
            for i in &list {
                say!(
                    "{}  {:<7}  {:<5}  {}/{} used  expires {}  {}",
                    i.id_hex(),
                    i.status(now).as_str(),
                    i.role,
                    i.uses,
                    i.max_uses,
                    if i.expires > now {
                        format!("in {}s", i.expires - now)
                    } else {
                        format!("{} ago", ago(now, Some(i.expires)))
                    },
                    i.label
                );
            }
            crate::result!(json!({"invitations": list.iter().map(|i| invitation_json(i, now)).collect::<Vec<_>>()}));
        }
        InviteCmd::Revoke { id } => {
            let rec = h.store.revoke_invitation(&id, now).map_err(|e| match e {
                rustgit_host::authz::AuthError::NotFound(m) => coded("no_such_invitation", m),
                other => store_err(other),
            })?;
            say!("Revoked invitation {}", rec.id_hex());
            crate::result!(json!({"invitation": invitation_json(&rec, now)}));
        }
    }
    Ok(0)
}

fn validate_hint(h: &str) -> Result<(), Error> {
    let ok = h.contains("://")
        && !h.chars().any(|c| c.is_whitespace() || c.is_control())
        && h.len() <= 255
        && ["tcp://", "ssh://", "iroh://"].iter().any(|p| h.starts_with(p));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "'{h}' is not a connection hint (expected tcp://host:port, ssh://..., iroh://...)"
        )))
    }
}

// ---- devices ----------------------------------------------------------------------------------------------

fn devices(dir: &std::path::Path, a: DevicesCmd) -> Result<u8, Error> {
    let h = open_hosted(dir)?;
    let now = unix_now();
    match a {
        DevicesCmd::List => {
            let list = h.store.list_devices().map_err(store_err)?;
            for d in &list {
                say!(
                    "{}  {:<7}  {:<5}  {:<24}  last seen {}",
                    d.fingerprint.short(),
                    if d.revoked_at.is_some() { "revoked" } else { "active" },
                    d.role,
                    d.name,
                    ago(now, d.last_seen)
                );
            }
            crate::result!(json!({"devices": list.iter().map(|d| device_json(d, now)).collect::<Vec<_>>()}));
        }
        DevicesCmd::Revoke { device } => {
            let d = h.store.revoke_device(&device, now).map_err(store_err)?;
            say!(
                "Revoked {} ({}). It is refused from its next request on; data it already \
                 fetched stays with it.",
                d.fingerprint.short(),
                d.name
            );
            crate::result!(json!({"device": device_json(&d, now)}));
        }
        DevicesCmd::Role { device, role } => {
            let role: Role = role.parse().map_err(Error::Invalid)?;
            let d = h.store.set_role(&device, role).map_err(store_err)?;
            say!("{} ({}) is now {}", d.fingerprint.short(), d.name, role);
            crate::result!(json!({"device": device_json(&d, now)}));
        }
    }
    Ok(0)
}

// ---- host key ---------------------------------------------------------------------------------------------

fn host(dir: &std::path::Path, a: HostCmd) -> Result<u8, Error> {
    let h = open_hosted(dir)?;
    let ks = key_storage();
    match a {
        HostCmd::Id => {
            let (k, created) = ensure_host_identity(&h.uuid, &ks, &h.store).map_err(host_key_error)?;
            let fp = k.fingerprint();
            say!("{}", fp.to_hex());
            crate::result!(json!({
                "fingerprint": fp.to_hex(),
                "short": fp.short(),
                "created": created,
                "key_storage": storage_name(&ks),
                "project": h.uuid,
            }));
        }
        HostCmd::Rotate { compromised } => {
            let _lock = rustgit_host::maintenance_lock(&h.root).map_err(|e| match e {
                rustgit_host::Error::Locked(_) => {
                    coded("host_serving", "stop `rgit serve` before rotating the host key")
                }
                e => Error::Other(e.to_string()),
            })?;
            let r = rotate_host_identity(&h.uuid, &ks, &h.store, compromised).map_err(host_key_error)?;
            say!("New host identity {}", r.new);
            if r.succession_recorded {
                say!("Clients pinned to the old identity will follow the signed succession and re-pin automatically.");
            } else {
                say!(
                    "No succession was recorded: every client needs a new invitation ({} unused invitation(s) revoked). \
                     Enrolled devices stay enrolled.",
                    r.invitations_revoked
                );
            }
            crate::result!(json!({
                "old_fingerprint": r.old.map(|f| f.to_hex()),
                "fingerprint": r.new.to_hex(),
                "succession_recorded": r.succession_recorded,
                "invitations_revoked": r.invitations_revoked,
            }));
        }
    }
    Ok(0)
}

// ---- join -------------------------------------------------------------------------------------------------

fn read_invitation(arg: &str) -> Result<Invitation, Error> {
    let text = if arg == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::Read::take(std::io::stdin(), 8192), &mut s)?;
        s
    } else {
        arg.to_string()
    };
    Invitation::parse(&text).map_err(|e| coded("invite_malformed", e))
}

fn join(dir: &std::path::Path, a: JoinArgs) -> Result<u8, Error> {
    let inv = read_invitation(&a.invitation)?;
    let now = unix_now();
    if inv.is_expired(now) {
        return Err(coded("invite_expired", "this invitation has expired; ask for a new one"));
    }
    // Refuse a destination that cannot work *before* redeeming: enrollment consumes the invitation.
    if a.add_remote {
        Repo::discover(dir)?;
    } else {
        let dest = a.dest.clone().unwrap_or_else(|| {
            PathBuf::from(format!("project-{}", &inv.project[..inv.project.len().min(8)]))
        });
        if dest.exists() && std::fs::read_dir(&dest)?.next().is_some() {
            return Err(Error::Invalid(format!(
                "destination {} exists and is not empty; nothing was redeemed",
                dest.display()
            )));
        }
    }
    let ks = key_storage();
    let (me, _) = ks.load_or_create(DEVICE_ACCOUNT).map_err(key_err)?;
    let me = Arc::new(me);

    let mut cfg = ClientConfig::new(device_name()).secure(me.clone(), inv.host);
    cfg.project = Some(inv.project.clone());
    let enroll_cfg = cfg.clone().enrolling(inv.id, *inv.secret.expose());
    let cancel = CancelToken::new();

    // Try each hint; stop at the first answer that is a verdict rather than an unreachable host.
    let mut last: Option<rustgit_protocol::Error> = None;
    let mut chosen: Option<(String, Role)> = None;
    for hint in &inv.hints {
        let t = match crate::net::transport_for(hint) {
            Ok(t) => t,
            Err(e) => {
                last = Some(rustgit_protocol::Error::Other(e));
                continue;
            }
        };
        match Client::connect(&*t, &enroll_cfg, cancel.clone()) {
            Ok(c) => {
                let role = c.hello.role.unwrap_or(inv.role);
                c.close();
                chosen = Some((hint.clone(), role));
                break;
            }
            Err(e) => {
                let verdict = matches!(
                    e,
                    rustgit_protocol::Error::HostKeyMismatch { .. } | rustgit_protocol::Error::Remote(_)
                );
                last = Some(e);
                if verdict {
                    break;
                }
            }
        }
    }
    let Some((url, role)) = chosen else {
        let e = last.unwrap_or_else(|| rustgit_protocol::Error::Other("the invitation has no connection hints".into()));
        return Err(tag_proto(&e).unwrap_or_else(|| Error::Other(format!("could not reach the host: {e}"))));
    };

    let t = crate::net::transport_for(&url).map_err(Error::Other)?;
    let (dest_str, branch, head, remote_name);
    if a.add_remote {
        let repo = Repo::discover(dir)?;
        repo.set_config(&format!("remote.{}.url", a.remote), &url)?;
        repo.set_config(&format!("remote.{}.project", a.remote), &inv.project)?;
        repo.set_config(&format!("remote.{}.host_key", a.remote), &inv.host.to_hex())?;
        dest_str = repo.root.to_string_lossy().to_string();
        branch = Value::Null;
        head = Value::Null;
        remote_name = a.remote.clone();
        say!("Joined; remote '{}' now points at {}", a.remote, url);
    } else {
        let dest = a
            .dest
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("project-{}", &inv.project[..inv.project.len().min(8)])));
        let repo = rustgit_protocol::with_retries(&cfg, &cancel, || {
            clone_into(&*t, &url, &cfg, &dest, &a.remote, cancel.clone())
        })
        .map_err(|e| tag_proto(&e).unwrap_or_else(|| Error::Other(e.to_string())))?;
        repo.set_config(&format!("remote.{}.host_key", a.remote), &inv.host.to_hex())?;
        let h = repo.head_commit()?;
        dest_str = dest.to_string_lossy().to_string();
        branch = json!(repo.current_branch()?);
        head = json!(h.map(|h| h.to_hex()));
        remote_name = a.remote.clone();
        say!("Joined and cloned into {} as '{}'", dest.display(), role);
    }
    crate::result!(json!({
        "project": inv.project,
        "role": role.as_str(),
        "host_fingerprint": inv.host.to_hex(),
        "url": url,
        "remote": remote_name,
        "dest": dest_str,
        "branch": branch,
        "head": head,
        "device_fingerprint": me.fingerprint().to_hex(),
    }));
    Ok(0)
}

/// Loads the secure client settings for a remote: when `remote.<name>.host_key` is set, the
/// connection must prove that identity and this device authenticates with its key.
pub fn secure_client(
    cfg: ClientConfig,
    host_key: Option<&str>,
) -> Result<ClientConfig, Error> {
    let Some(fp) = host_key.filter(|s| !s.is_empty()) else {
        return Ok(cfg);
    };
    let fp: Fingerprint = fp
        .parse()
        .map_err(|e: String| Error::Invalid(format!("remote host key: {e}")))?;
    let (me, _) = device_key()?;
    Ok(cfg.secure(Arc::new(me), fp))
}
