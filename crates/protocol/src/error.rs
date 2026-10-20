//! Structured errors: what travels on the wire (`WireError`) and what a local caller gets
//! back (`Error`).

use rustgit_store::codec::{Decoder, Encoder};
use rustgit_store::ObjectId;

/// Stable numeric error codes. New codes may be added; unknown codes decode to `Internal`
/// with the original number kept in the message, so old clients still show something useful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    Internal = 1,
    BadRequest = 2,
    VersionMismatch = 3,
    Unauthorized = 4,
    NotFound = 5,
    ProjectMismatch = 6,
    /// The ref is no longer at the head/generation the client expected.
    StaleRef = 10,
    /// Expected head matched but the new head does not descend from it.
    NonFastForward = 11,
    ForcePushDisabled = 12,
    DeleteDisabled = 13,
    InvalidRefName = 14,
    /// Objects the new head depends on are neither staged nor stored. Detail lists them.
    MissingObjects = 20,
    CorruptObject = 21,
    ObjectTooLarge = 22,
    /// An object does not decode, or violates the format's structural rules.
    InvalidObject = 23,
    QuotaExceeded = 30,
    OutOfSpace = 31,
    /// Too many connections / the session is in use by another connection. Retry later.
    Busy = 32,
    Timeout = 33,
    Cancelled = 34,
    UnknownSession = 35,
    ShuttingDown = 36,
    FrameTooLarge = 37,
    /// A delta-encoded object names a base the receiver does not have. Resend whole.
    UnknownBase = 38,
    /// The connection is authenticated but the device is not enrolled and presented no valid
    /// invitation.
    UnknownDevice = 40,
    /// The device's authorization was revoked. Permanent for that key.
    DeviceRevoked = 41,
    /// The device is enrolled but its role does not allow the operation.
    Forbidden = 42,
    /// Unknown invitation or wrong secret (deliberately indistinguishable).
    InvitationInvalid = 43,
    InvitationExpired = 44,
    /// All uses of the invitation are spent (a replay).
    InvitationUsed = 45,
    InvitationRevoked = 46,
    /// The host only accepts the encrypted, authenticated channel.
    SecurityRequired = 47,
    /// The host presented an identity other than the pinned one (detected by the client).
    HostKeyMismatch = 48,
    /// The secure channel could not be established.
    HandshakeFailed = 49,
}

impl ErrorCode {
    pub fn from_u16(v: u16) -> Option<Self> {
        use ErrorCode::*;
        Some(match v {
            1 => Internal,
            2 => BadRequest,
            3 => VersionMismatch,
            4 => Unauthorized,
            5 => NotFound,
            6 => ProjectMismatch,
            10 => StaleRef,
            11 => NonFastForward,
            12 => ForcePushDisabled,
            13 => DeleteDisabled,
            14 => InvalidRefName,
            20 => MissingObjects,
            21 => CorruptObject,
            22 => ObjectTooLarge,
            23 => InvalidObject,
            30 => QuotaExceeded,
            31 => OutOfSpace,
            32 => Busy,
            33 => Timeout,
            34 => Cancelled,
            35 => UnknownSession,
            36 => ShuttingDown,
            37 => FrameTooLarge,
            38 => UnknownBase,
            40 => UnknownDevice,
            41 => DeviceRevoked,
            42 => Forbidden,
            43 => InvitationInvalid,
            44 => InvitationExpired,
            45 => InvitationUsed,
            46 => InvitationRevoked,
            47 => SecurityRequired,
            48 => HostKeyMismatch,
            49 => HandshakeFailed,
            _ => return None,
        })
    }
}

/// Machine-readable extras that let a client recover without parsing the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detail {
    None,
    /// `MissingObjects`, `CorruptObject`: the offending ids.
    Ids(Vec<ObjectId>),
    /// `StaleRef`, `NonFastForward`: what the server has now.
    Ref {
        name: String,
        current: Option<ObjectId>,
        generation: u64,
    },
    /// `QuotaExceeded`: which limit and its value.
    Limit { what: String, limit: u64 },
    /// `VersionMismatch`: versions the server speaks.
    Versions { min: u16, max: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
    /// True when repeating the same request later can succeed without any change by the
    /// caller (server busy, timeout, shutting down).
    pub retryable: bool,
    pub detail: Detail,
}

impl WireError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let retryable = matches!(
            code,
            ErrorCode::Busy | ErrorCode::Timeout | ErrorCode::ShuttingDown
        );
        WireError {
            code,
            message: message.into(),
            retryable,
            detail: Detail::None,
        }
    }

    pub fn with_detail(mut self, d: Detail) -> Self {
        self.detail = d;
        self
    }

    pub fn encode(&self, e: &mut Encoder) {
        e.varint(self.code as u16 as u64)
            .string(&self.message)
            .u8(self.retryable as u8);
        match &self.detail {
            Detail::None => {
                e.u8(0);
            }
            Detail::Ids(ids) => {
                e.u8(1).varint(ids.len() as u64);
                for i in ids {
                    e.id(i);
                }
            }
            Detail::Ref {
                name,
                current,
                generation,
            } => {
                e.u8(2).string(name);
                match current {
                    Some(c) => {
                        e.u8(1).id(c);
                    }
                    None => {
                        e.u8(0);
                    }
                }
                e.varint(*generation);
            }
            Detail::Limit { what, limit } => {
                e.u8(3).string(what).varint(*limit);
            }
            Detail::Versions { min, max } => {
                e.u8(4).varint(*min as u64).varint(*max as u64);
            }
        }
    }

    pub fn decode(d: &mut Decoder) -> rustgit_store::Result<Self> {
        let raw = d.varint()?;
        let message = d.string(64 * 1024)?.to_string();
        let retryable = d.u8()? != 0;
        let (code, message) = match u16::try_from(raw).ok().and_then(ErrorCode::from_u16) {
            Some(c) => (c, message),
            None => (ErrorCode::Internal, format!("[code {raw}] {message}")),
        };
        let detail = match d.u8()? {
            0 => Detail::None,
            1 => {
                let n = d.varint()?;
                if n > 65_536 || (n as usize) * 32 > d.remaining() {
                    return Err(rustgit_store::Error::malformed("error id list too long"));
                }
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    v.push(d.id()?);
                }
                Detail::Ids(v)
            }
            2 => {
                let name = d.string(1024)?.to_string();
                let current = match d.u8()? {
                    0 => None,
                    1 => Some(d.id()?),
                    _ => return Err(rustgit_store::Error::malformed("bad option tag")),
                };
                Detail::Ref {
                    name,
                    current,
                    generation: d.varint()?,
                }
            }
            3 => Detail::Limit {
                what: d.string(256)?.to_string(),
                limit: d.varint()?,
            },
            4 => Detail::Versions {
                min: d.varint()? as u16,
                max: d.varint()? as u16,
            },
            _ => return Err(rustgit_store::Error::malformed("unknown error detail")),
        };
        Ok(WireError {
            code,
            message,
            retryable,
            detail,
        })
    }
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

/// Everything a protocol operation can fail with on the calling side.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The peer answered with a structured error.
    #[error("remote error: {0}")]
    Remote(WireError),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The peer sent something that violates the protocol (bad frame, unexpected message).
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// The host proved an identity that is not the one pinned for it.
    #[error("host identity mismatch: pinned {pinned}, host presented {presented}")]
    HostKeyMismatch {
        pinned: crate::identity::Fingerprint,
        presented: crate::identity::Fingerprint,
    },
    /// The encrypted channel could not be established or failed authentication.
    #[error("secure channel: {0}")]
    Handshake(String),
    #[error("connection timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Store(#[from] rustgit_store::Error),
    #[error("{0}")]
    Core(#[from] rustgit_core::Error),
    /// A received object failed verification (hash, structure or dependencies).
    #[error("received data failed verification: {0}")]
    Verification(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn protocol(m: impl Into<String>) -> Self {
        Error::Protocol(m.into())
    }

    /// The remote error code, if this is a structured remote failure.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Error::Remote(w) => Some(w.code),
            Error::HostKeyMismatch { .. } => Some(ErrorCode::HostKeyMismatch),
            Error::Handshake(_) => Some(ErrorCode::HandshakeFailed),
            _ => None,
        }
    }

    /// Whether the same operation may succeed if repeated later without changes.
    pub fn retryable(&self) -> bool {
        match self {
            Error::Remote(w) => w.retryable,
            Error::Timeout => true,
            Error::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
            ),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_error_roundtrip() {
        let id = ObjectId([7; 32]);
        for d in [
            Detail::None,
            Detail::Ids(vec![id, ObjectId([1; 32])]),
            Detail::Ref {
                name: "heads/main".into(),
                current: Some(id),
                generation: 9,
            },
            Detail::Ref {
                name: "heads/x".into(),
                current: None,
                generation: 0,
            },
            Detail::Limit {
                what: "session bytes".into(),
                limit: 5,
            },
            Detail::Versions { min: 1, max: 3 },
        ] {
            let w = WireError::new(ErrorCode::StaleRef, "m").with_detail(d);
            let mut e = Encoder::new();
            w.encode(&mut e);
            let b = e.finish();
            let mut dec = Decoder::new(&b);
            assert_eq!(WireError::decode(&mut dec).unwrap(), w);
            dec.finish().unwrap();
        }
    }

    #[test]
    fn unknown_code_degrades_to_internal() {
        let mut e = Encoder::new();
        e.varint(999).string("future").u8(0).u8(0);
        let b = e.finish();
        let w = WireError::decode(&mut Decoder::new(&b)).unwrap();
        assert_eq!(w.code, ErrorCode::Internal);
        assert!(w.message.contains("999"));
    }
}
