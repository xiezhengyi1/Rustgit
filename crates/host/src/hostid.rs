//! The host's own identity key: creation, loading and rotation.
//!
//! The private key lives in [`KeyStorage`] under the account `host-<project uuid>`; the public
//! key (and any signed succession links from older keys) lives in the [`AuthStore`], so
//! creating invitations never needs the private key.
//!
//! Rotation replaces the key. By default the old key signs a *succession link* to the new one,
//! which hosts present in the handshake: clients pinned to the old fingerprint accept the new
//! key and re-pin. With `compromised` (or a lost key) no link is made, every unused invitation
//! is revoked, and clients must be given a new invitation, because a stolen old key could
//! otherwise vouch for an attacker. Enrolled devices stay enrolled either way (they are
//! identified by their own keys).

use rustgit_protocol::{DeviceKey, Fingerprint, Succession};

use crate::authz::{unix_now, AuthError, AuthStore};
use crate::keystore::{KeyError, KeyStorage};

#[derive(Debug, thiserror::Error)]
pub enum HostKeyError {
    #[error("{0}")]
    Key(#[from] KeyError),
    #[error("{0}")]
    Store(#[from] AuthError),
    #[error("{0}")]
    Inconsistent(String),
}

pub fn host_account(uuid: &str) -> String {
    format!("host-{uuid}")
}

fn next_account(uuid: &str) -> String {
    format!("host-{uuid}-next")
}

/// Loads the host key, creating it on first use. Returns `(key, created)`.
pub fn ensure_host_identity(
    uuid: &str,
    storage: &KeyStorage,
    store: &AuthStore,
) -> Result<(DeviceKey, bool), HostKeyError> {
    let account = host_account(uuid);
    let db_pub = store.host_public()?;
    // finish an interrupted rotation: the staged key is already recorded in the database
    if let (Some(p), Some(next)) = (db_pub, storage.load(&next_account(uuid))?) {
        if next.public() == p {
            storage.store(&account, &next)?;
            storage.delete(&next_account(uuid))?;
        }
    }
    let stored = storage.load(&account)?;
    match (db_pub, stored) {
        (Some(p), Some(k)) if k.public() == p => Ok((k, false)),
        (Some(_), Some(_)) => Err(HostKeyError::Inconsistent(
            "the host key in key storage does not match this repository's record (restored from a \
             backup? run `rgit host rotate --compromised` to start a new identity)"
                .into(),
        )),
        (Some(_), None) => Err(HostKeyError::Inconsistent(
            "this repository has a host identity but its private key is not in key storage here \
             (moved to another machine or user? restore the key, or run `rgit host rotate \
             --compromised` to start a new identity)"
                .into(),
        )),
        (None, Some(k)) => {
            store.set_host_public(&k.public(), None, false)?;
            Ok((k, false))
        }
        (None, None) => {
            let k = DeviceKey::generate();
            storage.store(&account, &k)?;
            store.set_host_public(&k.public(), None, false)?;
            Ok((k, true))
        }
    }
}

#[derive(Debug)]
pub struct Rotation {
    pub old: Option<Fingerprint>,
    pub new: Fingerprint,
    pub succession_recorded: bool,
    pub invitations_revoked: usize,
}

/// Replaces the host key. Call only while the host is not being served.
pub fn rotate_host_identity(
    uuid: &str,
    storage: &KeyStorage,
    store: &AuthStore,
    compromised: bool,
) -> Result<Rotation, HostKeyError> {
    let account = host_account(uuid);
    let old = match ensure_host_identity(uuid, storage, store) {
        Ok((k, _)) => Some(k),
        Err(HostKeyError::Inconsistent(_)) if compromised => None,
        Err(e) => return Err(e),
    };
    let new = DeviceKey::generate();
    let link: Option<Succession> = match (&old, compromised) {
        (Some(o), false) => Some(Succession::create(o, &new.public())),
        _ => None,
    };
    // 1. stage the new key, 2. record it (with the link), 3. promote it. A crash in between is
    // completed by the next `ensure_host_identity`.
    storage.store(&next_account(uuid), &new)?;
    store.set_host_public(&new.public(), link, compromised)?;
    let revoked = if compromised {
        store.revoke_all_invitations(unix_now())?
    } else {
        0
    };
    storage.store(&account, &new)?;
    storage.delete(&next_account(uuid))?;
    Ok(Rotation {
        old: old.map(|k| k.fingerprint()),
        new: new.fingerprint(),
        succession_recorded: link.is_some(),
        invitations_revoked: revoked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgit_protocol::identity::pin_accepts;

    fn setup() -> (tempfile::TempDir, KeyStorage, AuthStore) {
        let d = tempfile::tempdir().unwrap();
        let ks = KeyStorage::File(d.path().join("keys"));
        let st = AuthStore::open(d.path()).unwrap();
        (d, ks, st)
    }

    #[test]
    fn create_then_reload_then_rotate_with_succession() {
        let (_d, ks, st) = setup();
        let (k1, created) = ensure_host_identity("u1", &ks, &st).unwrap();
        assert!(created);
        let (k1b, created) = ensure_host_identity("u1", &ks, &st).unwrap();
        assert!(!created && k1b.fingerprint() == k1.fingerprint());
        let r = rotate_host_identity("u1", &ks, &st, false).unwrap();
        assert!(r.succession_recorded);
        let (k2, _) = ensure_host_identity("u1", &ks, &st).unwrap();
        assert_ne!(k2.fingerprint(), k1.fingerprint());
        assert_eq!(r.new, k2.fingerprint());
        // a client pinned to the first key accepts the second through the stored link
        assert!(pin_accepts(&k1.fingerprint(), &k2.public(), &st.successions().unwrap()));
    }

    #[test]
    fn compromised_rotation_breaks_the_chain_and_revokes_invitations() {
        let (_d, ks, st) = setup();
        let (k1, _) = ensure_host_identity("u2", &ks, &st).unwrap();
        st.create_invitation(rustgit_protocol::Role::Read, 100, 1, "", 1).unwrap();
        let r = rotate_host_identity("u2", &ks, &st, true).unwrap();
        assert!(!r.succession_recorded);
        assert_eq!(r.invitations_revoked, 1);
        let (k2, _) = ensure_host_identity("u2", &ks, &st).unwrap();
        assert!(!pin_accepts(&k1.fingerprint(), &k2.public(), &st.successions().unwrap()));
    }

    #[test]
    fn a_missing_private_key_is_an_error_not_a_silent_new_identity() {
        let (_d, ks, st) = setup();
        ensure_host_identity("u3", &ks, &st).unwrap();
        ks.delete(&host_account("u3")).unwrap();
        assert!(matches!(
            ensure_host_identity("u3", &ks, &st),
            Err(HostKeyError::Inconsistent(_))
        ));
        // the explicit escape hatch works
        assert!(rotate_host_identity("u3", &ks, &st, true).is_ok());
        assert!(ensure_host_identity("u3", &ks, &st).is_ok());
    }
}
