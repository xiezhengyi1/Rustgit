//! Authorization. Two implementations of [`Authorizer`]:
//!
//! * [`DeviceAuthorizer`] (the default for `rgit serve`): the connection runs over the secure
//!   channel, so the device key is authenticated; the host looks that key up in its
//!   [`AuthStore`] on **every request** and enforces the role per operation.
//! * [`OpenAccess`]: everyone is an admin. Only for `--insecure-open-access` on loopback and
//!   for tests.

use std::sync::Arc;

use rustgit_protocol::{
    Credential, DeviceKey, ErrorCode, Fingerprint, PublicKey, Role, Succession, WireError,
};

use crate::authz::{unix_now, AuthStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op<'a> {
    /// ListRefs, Have, Plan, PlanPage, Get, Watch.
    Read,
    /// Put / Cancel: sending objects into a staging area.
    Upload,
    /// Move a branch forward.
    Push { ref_name: &'a str },
    /// Delete a branch.
    Delete { ref_name: &'a str },
    /// Replace history (force) on a branch.
    Force { ref_name: &'a str },
}

impl Op<'_> {
    pub fn required_role(&self) -> Role {
        match self {
            Op::Read => Role::Read,
            Op::Upload | Op::Push { .. } => Role::Write,
            Op::Delete { .. } | Op::Force { .. } => Role::Admin,
        }
    }
}

/// What the server knows about a connection when it asks for admission.
pub struct AdmitRequest<'a> {
    /// The peer's authenticated device key (`None` on a plaintext connection).
    pub peer_key: Option<&'a PublicKey>,
    /// Self-declared, display only.
    pub device_name: &'a str,
    /// Encoded `Credential` from `Hello`.
    pub credential: &'a [u8],
}

#[derive(Clone, Debug)]
pub struct Admission {
    /// Identifies the connection to the authorizer afterwards (`Authorizer::authorize`'s
    /// `device` argument) and keys per-device limits.
    pub principal: String,
    pub role: Role,
    /// For logs.
    pub display: String,
}

pub trait Authorizer: Send + Sync {
    /// Decides who a new connection is (and enrolls it when it presents a valid invitation).
    fn admit(&self, req: &AdmitRequest<'_>) -> Result<Admission, WireError>;

    /// Checked before every operation. `device` is the `principal` from [`Admission`];
    /// `credential` is unused by the built-in authorizers.
    fn authorize(&self, device: &str, credential: &[u8], op: Op<'_>) -> Result<(), WireError>;

    /// Called now and then on long connections so `last seen` stays current.
    fn touch(&self, _principal: &str) {}

    /// When `Some`, the server requires the secure channel and presents this identity (and the
    /// signed succession links from earlier keys). `None` serves plaintext.
    fn host_identity(&self) -> Option<(Arc<DeviceKey>, Vec<Succession>)> {
        None
    }
}

/// Everyone allowed, nothing encrypted.
pub struct OpenAccess;

impl Authorizer for OpenAccess {
    fn admit(&self, req: &AdmitRequest<'_>) -> Result<Admission, WireError> {
        Ok(Admission {
            principal: req.device_name.to_string(),
            role: Role::Admin,
            display: req.device_name.to_string(),
        })
    }
    fn authorize(&self, _device: &str, _credential: &[u8], _op: Op<'_>) -> Result<(), WireError> {
        Ok(())
    }
}

const PRINCIPAL_PREFIX: &str = "fp:";

fn principal_of(fp: &Fingerprint) -> String {
    format!("{PRINCIPAL_PREFIX}{}", fp.to_hex())
}

fn parse_principal(p: &str) -> Result<Fingerprint, WireError> {
    p.strip_prefix(PRINCIPAL_PREFIX)
        .and_then(|h| h.parse().ok())
        .ok_or_else(|| WireError::new(ErrorCode::Unauthorized, "connection is not authenticated"))
}

/// Device-key authorization backed by the host's [`AuthStore`].
pub struct DeviceAuthorizer {
    store: Arc<AuthStore>,
    identity: Arc<DeviceKey>,
    chain: Vec<Succession>,
}

impl DeviceAuthorizer {
    pub fn new(store: Arc<AuthStore>, identity: DeviceKey, chain: Vec<Succession>) -> Self {
        DeviceAuthorizer {
            store,
            identity: Arc::new(identity),
            chain,
        }
    }

    pub fn store(&self) -> &Arc<AuthStore> {
        &self.store
    }
}

impl Authorizer for DeviceAuthorizer {
    fn admit(&self, req: &AdmitRequest<'_>) -> Result<Admission, WireError> {
        let key = req.peer_key.ok_or_else(|| {
            WireError::new(ErrorCode::SecurityRequired, "device authentication is required")
        })?;
        let fp = key.fingerprint();
        let cred = Credential::decode(req.credential)
            .map_err(|e| WireError::new(ErrorCode::BadRequest, e.to_string()))?;
        let now = unix_now();
        let rec = match cred {
            Credential::Enroll { invitation, secret } => {
                self.store
                    .redeem(&invitation, &secret, key, req.device_name, now)?
            }
            Credential::None => {
                let d = self.store.check(&fp, Role::Read)?;
                self.store.touch(&fp, now);
                d
            }
        };
        Ok(Admission {
            principal: principal_of(&fp),
            role: rec.role,
            display: format!("{} [{}]", rec.name, fp.short()),
        })
    }

    fn authorize(&self, device: &str, _credential: &[u8], op: Op<'_>) -> Result<(), WireError> {
        let fp = parse_principal(device)?;
        self.store.check(&fp, op.required_role()).map(|_| ())
    }

    fn touch(&self, principal: &str) {
        if let Ok(fp) = parse_principal(principal) {
            self.store.touch(&fp, unix_now());
        }
    }

    fn host_identity(&self) -> Option<(Arc<DeviceKey>, Vec<Succession>)> {
        Some((self.identity.clone(), self.chain.clone()))
    }
}
