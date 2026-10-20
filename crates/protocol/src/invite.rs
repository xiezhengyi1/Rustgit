//! Invitations: `projectapp://join/<base64url>`.
//!
//! An invitation carries everything a new device needs to find, authenticate and enroll with
//! a host: the project id, the host's identity fingerprint (the pin), connection hints, an
//! expiry, the role it grants and a high-entropy enrollment secret. Addresses are only hints;
//! the pinned fingerprint is what decides who the host is.
//!
//! Payload (canonical, same primitives as the rest of the protocol):
//!
//! ```text
//! u8 version (1) | string project | 32 bytes host fingerprint | varint n, n x string hint |
//! varint expires (unix seconds) | u8 role | 16 bytes invitation id | 32 bytes secret
//! ```

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rustgit_store::codec::{Decoder, Encoder};

use crate::identity::{EnrollmentSecret, Fingerprint, Role};

pub const URI_PREFIX: &str = "projectapp://join/";
pub const INVITATION_VERSION: u8 = 1;
const MAX_URI: usize = 4096;
const MAX_HINTS: usize = 8;

#[derive(Clone, Debug)]
pub struct Invitation {
    pub project: String,
    pub host: Fingerprint,
    pub hints: Vec<String>,
    /// Unix seconds.
    pub expires: u64,
    pub role: Role,
    pub id: [u8; 16],
    pub secret: EnrollmentSecret,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InviteParseError {
    #[error("not an invitation link (expected projectapp://join/...)")]
    NotAnInvitation,
    #[error("the invitation is malformed: {0}")]
    Malformed(&'static str),
    #[error("unsupported invitation version {0}; update rgit")]
    UnsupportedVersion(u8),
}

impl Invitation {
    pub fn to_uri(&self) -> String {
        let mut e = Encoder::new();
        e.u8(INVITATION_VERSION)
            .string(&self.project)
            .bytes(&self.host.0)
            .varint(self.hints.len() as u64);
        for h in &self.hints {
            e.string(h);
        }
        e.varint(self.expires)
            .u8(self.role as u8)
            .bytes(&self.id)
            .bytes(self.secret.expose());
        format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(e.finish()))
    }

    /// Parses a pasted link. Whitespace (line wraps from copy/paste) is ignored. Errors never
    /// echo any part of the input.
    pub fn parse(input: &str) -> Result<Invitation, InviteParseError> {
        let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
        let b64 = compact
            .strip_prefix(URI_PREFIX)
            .ok_or(InviteParseError::NotAnInvitation)?;
        if compact.len() > MAX_URI {
            return Err(InviteParseError::Malformed("too long"));
        }
        let raw = URL_SAFE_NO_PAD
            .decode(b64.trim_end_matches('/'))
            .map_err(|_| InviteParseError::Malformed("not valid base64url"))?;
        let mut d = Decoder::new(&raw);
        let bad = |_| InviteParseError::Malformed("truncated or non-canonical");
        let version = d.u8().map_err(bad)?;
        if version != INVITATION_VERSION {
            return Err(InviteParseError::UnsupportedVersion(version));
        }
        let project = d.string(64).map_err(bad)?.to_string();
        let host: [u8; 32] = d
            .bytes(32)
            .map_err(bad)?
            .try_into()
            .map_err(|_| InviteParseError::Malformed("bad host fingerprint"))?;
        let n = d.varint().map_err(bad)?;
        if n as usize > MAX_HINTS {
            return Err(InviteParseError::Malformed("too many connection hints"));
        }
        let mut hints = Vec::new();
        for _ in 0..n {
            let h = d.string(255).map_err(bad)?;
            if h.is_empty() || h.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(InviteParseError::Malformed("bad connection hint"));
            }
            hints.push(h.to_string());
        }
        let expires = d.varint().map_err(bad)?;
        let role = Role::from_u8(d.u8().map_err(bad)?)
            .ok_or(InviteParseError::Malformed("unknown role"))?;
        let id: [u8; 16] = d
            .bytes(16)
            .map_err(bad)?
            .try_into()
            .map_err(|_| InviteParseError::Malformed("bad invitation id"))?;
        let secret: [u8; 32] = d
            .bytes(32)
            .map_err(bad)?
            .try_into()
            .map_err(|_| InviteParseError::Malformed("bad enrollment secret"))?;
        d.finish().map_err(bad)?;
        if project.is_empty() {
            return Err(InviteParseError::Malformed("empty project id"));
        }
        Ok(Invitation {
            project,
            host: Fingerprint(host),
            hints,
            expires,
            role,
            id,
            secret: EnrollmentSecret::from_bytes(secret),
        })
    }

    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Invitation {
        Invitation {
            project: "0b0b0b0b-0000-4000-8000-000000000001".into(),
            host: Fingerprint([9; 32]),
            hints: vec!["tcp://192.168.1.5:7878".into(), "tcp://host.local:7878".into()],
            expires: 1_900_000_000,
            role: Role::Write,
            id: [3; 16],
            secret: EnrollmentSecret::from_bytes([5; 32]),
        }
    }

    #[test]
    fn roundtrip_and_paste_tolerance() {
        let i = sample();
        let uri = i.to_uri();
        assert!(uri.starts_with("projectapp://join/"));
        let p = Invitation::parse(&uri).unwrap();
        assert_eq!(p.project, i.project);
        assert_eq!(p.host, i.host);
        assert_eq!(p.hints, i.hints);
        assert_eq!((p.expires, p.role, p.id), (i.expires, i.role, i.id));
        assert_eq!(p.secret.expose(), i.secret.expose());
        // line-wrapped paste with surrounding whitespace
        let wrapped = format!("  {}\n{}\n", &uri[..40], &uri[40..]);
        assert!(Invitation::parse(&wrapped).is_ok());
    }

    #[test]
    fn malformed_inputs_fail_without_echoing_the_input() {
        let uri = sample().to_uri();
        for bad in [
            "".to_string(),
            "https://example.com/join/abc".into(),
            "projectapp://join/".into(),
            "projectapp://join/@@@@".into(),
            uri[..uri.len() - 10].to_string(),
            format!("{uri}AAAA"),
        ] {
            let e = Invitation::parse(&bad).unwrap_err();
            assert!(!e.to_string().contains(&uri[URI_PREFIX.len()..URI_PREFIX.len() + 12]));
        }
        // unknown version
        let mut e = Encoder::new();
        e.u8(9);
        let v9 = format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(e.finish()));
        assert_eq!(Invitation::parse(&v9).unwrap_err(), InviteParseError::UnsupportedVersion(9));
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let s = format!("{:?}", sample());
        assert!(s.contains("redacted"));
        assert!(!s.contains("05, 05"));
    }
}
