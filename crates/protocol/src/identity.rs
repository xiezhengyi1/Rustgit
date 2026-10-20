//! Device identities: Ed25519 key pairs, fingerprints, roles and secrets.
//!
//! A *device key* is an Ed25519 key pair. Its public half identifies the device (and, on the
//! host side, the host); the fingerprint is a 256-bit hash of it. The Noise (X25519) static key
//! used by the secure channel is derived from the same 32-byte seed, so there is exactly one
//! secret to protect per identity. See docs/protocol.md section 12.

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

const FP_DOMAIN: &str = "rustgit device fingerprint v1";
const NOISE_KEY_DOMAIN: &str = "rustgit noise static key v1";
const SECRET_HASH_DOMAIN: &str = "rustgit enrollment secret hash v1";

/// Fills `buf` from the operating system's CSPRNG.
pub fn random_bytes(buf: &mut [u8]) {
    getrandom::fill(buf).expect("the operating system's random number generator failed");
}

/// An Ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint(blake3::derive_key(FP_DOMAIN, &self.0))
    }

    /// Strict verification (rejects small-order keys and non-canonical signatures).
    pub fn verify(&self, msg: &[u8], sig: &[u8; 64]) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&self.0) else {
            return false;
        };
        let sig = Signature::from_bytes(sig);
        vk.verify_strict(msg, &sig).is_ok()
    }

    pub fn is_valid(&self) -> bool {
        VerifyingKey::from_bytes(&self.0).is_ok()
    }

    pub fn to_hex(&self) -> String {
        hex_of(&self.0)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.fingerprint().short())
    }
}

/// SHA-256-sized identity fingerprint (BLAKE3, domain separated). Canonical text form is 64
/// lowercase hex digits; [`FromStr`] also accepts `:`/`-`/space separators and upper case.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn to_hex(&self) -> String {
        hex_of(&self.0)
    }

    /// First 16 hex digits in groups of four, for people to compare.
    pub fn short(&self) -> String {
        let h = self.to_hex();
        h.as_bytes()[..16]
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

impl FromStr for Fingerprint {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let s = s.strip_prefix("rgfp:").unwrap_or(s);
        let digits: String = s
            .chars()
            .filter(|c| !matches!(c, ':' | '-' | ' '))
            .collect();
        let bytes = unhex(&digits).ok_or("a fingerprint is 64 hexadecimal digits")?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "a fingerprint is 64 hexadecimal digits".to_string())?;
        Ok(Fingerprint(arr))
    }
}

pub fn hex_of(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let v = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    s.as_bytes()
        .chunks(2)
        .map(|p| Some(v(p[0])? << 4 | v(p[1])?))
        .collect()
}

/// A device (or host) signing identity. The seed never leaves this type except through
/// [`DeviceKey::seed`], which key storage uses; `Debug` prints only the fingerprint.
pub struct DeviceKey {
    sk: SigningKey,
}

impl DeviceKey {
    pub fn generate() -> Self {
        let mut seed = Zeroizing::new([0u8; 32]);
        random_bytes(&mut seed[..]);
        Self::from_seed(&seed)
    }

    pub fn from_seed(seed: &[u8; 32]) -> Self {
        DeviceKey {
            sk: SigningKey::from_bytes(seed),
        }
    }

    pub fn seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.sk.to_bytes())
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.sk.verifying_key().to_bytes())
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.public().fingerprint()
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.sk.sign(msg).to_bytes()
    }

    /// X25519 private key for the Noise handshake, derived from the seed (domain separated
    /// from the signing key, so the two never meet in one algorithm).
    pub fn noise_static_secret(&self) -> Zeroizing<[u8; 32]> {
        let seed = self.seed();
        Zeroizing::new(blake3::derive_key(NOISE_KEY_DOMAIN, &seed[..]))
    }
}

impl fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceKey({})", self.fingerprint().short())
    }
}

/// What a device may do on a host. Ordered: `Read < Write < Admin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Role {
    /// Fetch, clone, watch.
    Read = 1,
    /// Read, plus push (fast-forward updates of branches).
    Write = 2,
    /// Write, plus force-push and branch deletion (where the host policy enables them).
    Admin = 3,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Read => "read",
            Role::Write => "write",
            Role::Admin => "admin",
        }
    }

    pub fn from_u8(v: u8) -> Option<Role> {
        match v {
            1 => Some(Role::Read),
            2 => Some(Role::Write),
            3 => Some(Role::Admin),
            _ => None,
        }
    }

    pub fn allows(self, needed: Role) -> bool {
        self >= needed
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read" => Ok(Role::Read),
            "write" => Ok(Role::Write),
            "admin" => Ok(Role::Admin),
            _ => Err("role must be read, write or admin".into()),
        }
    }
}

/// A 256-bit random enrollment secret. Zeroed on drop, redacted in `Debug`.
pub struct EnrollmentSecret(Zeroizing<[u8; 32]>);

impl EnrollmentSecret {
    pub fn generate() -> Self {
        let mut s = Zeroizing::new([0u8; 32]);
        random_bytes(&mut s[..]);
        EnrollmentSecret(s)
    }

    pub fn from_bytes(b: [u8; 32]) -> Self {
        EnrollmentSecret(Zeroizing::new(b))
    }

    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// What the host stores: a domain-separated hash. The secret has 256 bits of entropy, so
    /// a fast hash is sufficient; a leaked host database does not reveal usable secrets.
    pub fn hash(&self) -> [u8; 32] {
        blake3::derive_key(SECRET_HASH_DOMAIN, &self.0[..])
    }
}

impl fmt::Debug for EnrollmentSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnrollmentSecret(<redacted>)")
    }
}

impl Clone for EnrollmentSecret {
    fn clone(&self) -> Self {
        EnrollmentSecret(Zeroizing::new(*self.0))
    }
}

/// A host's signed statement that it moved to a new key ("key succession").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Succession {
    pub prev: PublicKey,
    pub next: PublicKey,
    pub sig: [u8; 64],
}

const SUCCESSION_DOMAIN: &[u8] = b"rustgit host key succession v1";

impl Succession {
    fn message(prev: &PublicKey, next: &PublicKey) -> Vec<u8> {
        let mut m = SUCCESSION_DOMAIN.to_vec();
        m.extend_from_slice(&prev.0);
        m.extend_from_slice(&next.0);
        m
    }

    /// Signed by the *old* key, so a client pinned to the old fingerprint can follow it.
    pub fn create(prev: &DeviceKey, next: &PublicKey) -> Succession {
        let prev_pub = prev.public();
        Succession {
            prev: prev_pub,
            next: *next,
            sig: prev.sign(&Self::message(&prev_pub, next)),
        }
    }

    pub fn verify(&self) -> bool {
        self.prev.verify(&Self::message(&self.prev, &self.next), &self.sig)
    }

    pub fn to_bytes(&self) -> [u8; 128] {
        let mut b = [0u8; 128];
        b[..32].copy_from_slice(&self.prev.0);
        b[32..64].copy_from_slice(&self.next.0);
        b[64..].copy_from_slice(&self.sig);
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<Succession> {
        if b.len() != 128 {
            return None;
        }
        Some(Succession {
            prev: PublicKey(b[..32].try_into().ok()?),
            next: PublicKey(b[32..64].try_into().ok()?),
            sig: b[64..].try_into().ok()?,
        })
    }
}

/// Whether `presented` is the pinned host key or reachable from it through verified
/// succession links (at most 16).
pub fn pin_accepts(pinned: &Fingerprint, presented: &PublicKey, chain: &[Succession]) -> bool {
    let mut current = *pinned;
    if presented.fingerprint() == current {
        return true;
    }
    for _ in 0..16 {
        let Some(link) = chain
            .iter()
            .find(|l| l.prev.fingerprint() == current && l.verify())
        else {
            return false;
        };
        if link.next == *presented {
            return true;
        }
        current = link.next.fingerprint();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_text_roundtrip_and_separators() {
        let k = DeviceKey::generate();
        let fp = k.fingerprint();
        assert_eq!(fp.to_hex().parse::<Fingerprint>().unwrap(), fp);
        let spaced: String = fp
            .to_hex()
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
            .to_uppercase();
        assert_eq!(spaced.parse::<Fingerprint>().unwrap(), fp);
        assert!("abcd".parse::<Fingerprint>().is_err());
        assert!(format!("{k:?}").len() < 40, "Debug must not print key material");
    }

    #[test]
    fn signatures_and_succession_chain() {
        let a = DeviceKey::generate();
        let b = DeviceKey::generate();
        let c = DeviceKey::generate();
        let ab = Succession::create(&a, &b.public());
        let bc = Succession::create(&b, &c.public());
        assert!(ab.verify() && bc.verify());
        assert!(pin_accepts(&a.fingerprint(), &c.public(), &[bc, ab]));
        assert!(!pin_accepts(&a.fingerprint(), &c.public(), &[bc]));
        // a link signed by someone else is not followed
        let forged = Succession {
            prev: a.public(),
            next: c.public(),
            sig: c.sign(b"whatever"),
        };
        assert!(!pin_accepts(&a.fingerprint(), &c.public(), &[forged]));
        let mut bytes = ab.to_bytes();
        assert_eq!(Succession::from_bytes(&bytes), Some(ab));
        bytes[70] ^= 1;
        assert!(!Succession::from_bytes(&bytes).unwrap().verify());
    }

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Admin.allows(Role::Write));
        assert!(Role::Write.allows(Role::Read));
        assert!(!Role::Read.allows(Role::Write));
        assert_eq!("WRITE".parse::<Role>().unwrap(), Role::Write);
        assert!("owner".parse::<Role>().is_err());
    }
}
