//! Where device and host private keys live.
//!
//! * Default: the platform's protected store (macOS Keychain, Windows Credential Manager).
//! * [`KeyStorage::File`]: a `0600` file. It is **never** chosen automatically: the user has to
//!   pass `--insecure-file-keys` (or set `RGIT_INSECURE_FILE_KEYS`) so that a missing keychain
//!   cannot silently downgrade protection. The file is not encrypted; anyone who can read it can
//!   impersonate the device.
//!
//! A key is 32 bytes (the Ed25519 seed; the Noise key is derived from it).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rustgit_protocol::DeviceKey;
use zeroize::Zeroizing;

pub const SERVICE: &str = "rustgit";

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// The platform store cannot be used. The message says how to opt into the file store.
    #[error("platform key storage is unavailable ({0}); to store the key in an unprotected 0600 file instead, pass --insecure-file-keys")]
    Unavailable(String),
    #[error("key storage error: {0}")]
    Platform(String),
    #[error("key file {path}: {why}")]
    File { path: PathBuf, why: String },
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub enum KeyStorage {
    /// macOS Keychain / Windows Credential Manager.
    Platform,
    /// Explicit opt-in: one `<account>.key` file per key in this directory.
    File(PathBuf),
}

/// Configuration directory for rgit (`RGIT_HOME`, else the platform's per-user location).
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("RGIT_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join("Library/Application Support/rgit");
        }
    }
    #[cfg(windows)]
    {
        if let Some(a) = std::env::var_os("APPDATA") {
            return PathBuf::from(a).join("rgit");
        }
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(x).join("rgit");
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config/rgit")
}

pub fn default_key_dir() -> PathBuf {
    config_dir().join("keys")
}

impl KeyStorage {
    pub fn name(&self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            KeyStorage::Platform => "macos-keychain",
            #[cfg(windows)]
            KeyStorage::Platform => "windows-credential-manager",
            #[cfg(not(any(target_os = "macos", windows)))]
            KeyStorage::Platform => "none",
            KeyStorage::File(_) => "file",
        }
    }

    pub fn is_protected(&self) -> bool {
        matches!(self, KeyStorage::Platform)
    }

    /// `Ok(None)` when no key is stored under `account`.
    pub fn load(&self, account: &str) -> Result<Option<DeviceKey>, KeyError> {
        match self {
            KeyStorage::Platform => platform::load(account),
            KeyStorage::File(dir) => file_load(dir, account),
        }
    }

    /// Stores (or replaces) the key.
    pub fn store(&self, account: &str, key: &DeviceKey) -> Result<(), KeyError> {
        match self {
            KeyStorage::Platform => platform::store(account, key),
            KeyStorage::File(dir) => file_store(dir, account, key),
        }
    }

    pub fn delete(&self, account: &str) -> Result<(), KeyError> {
        match self {
            KeyStorage::Platform => platform::delete(account),
            KeyStorage::File(dir) => match fs::remove_file(file_path(dir, account)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
        }
    }

    /// Loads the key, creating and storing a fresh one when absent. Returns `(key, created)`.
    pub fn load_or_create(&self, account: &str) -> Result<(DeviceKey, bool), KeyError> {
        if let Some(k) = self.load(account)? {
            return Ok((k, false));
        }
        let k = DeviceKey::generate();
        self.store(account, &k)?;
        Ok((k, true))
    }
}

fn seed_from(bytes: &[u8]) -> Option<DeviceKey> {
    let seed: Zeroizing<[u8; 32]> = Zeroizing::new(bytes.try_into().ok()?);
    Some(DeviceKey::from_seed(&seed))
}

// ---- file store ------------------------------------------------------------------------------

fn file_path(dir: &Path, account: &str) -> PathBuf {
    let safe: String = account
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    dir.join(format!("{safe}.key"))
}

fn file_err(path: &Path, why: impl Into<String>) -> KeyError {
    KeyError::File {
        path: path.to_path_buf(),
        why: why.into(),
    }
}

fn file_load(dir: &Path, account: &str) -> Result<Option<DeviceKey>, KeyError> {
    let path = file_path(dir, account);
    let meta = match fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(file_err(
                &path,
                "is readable by other users; run `chmod 600` on it (refusing to use it)",
            ));
        }
    }
    let _ = meta;
    let text = Zeroizing::new(fs::read_to_string(&path)?);
    let bytes = Zeroizing::new(
        unhex(text.trim()).ok_or_else(|| file_err(&path, "is not a key file"))?,
    );
    seed_from(&bytes).map(Some).ok_or_else(|| file_err(&path, "has the wrong length"))
}

fn file_store(dir: &Path, account: &str, key: &DeviceKey) -> Result<(), KeyError> {
    use std::io::Write;
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let path = file_path(dir, account);
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    let seed = key.seed();
    let line = Zeroizing::new(format!("{}\n", hex(&seed[..])));
    f.write_all(line.as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &path)?;
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

// ---- platform store --------------------------------------------------------------------------

#[cfg(any(target_os = "macos", windows))]
mod platform {
    use super::*;

    /// A locked keychain or an approval dialog nobody can see would block forever; give up
    /// with an actionable error instead.
    fn bounded<T: Send + 'static>(
        f: impl FnOnce() -> Result<T, KeyError> + Send + 'static,
    ) -> Result<T, KeyError> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap_or_else(|_| {
            Err(KeyError::Platform(
                "the platform key store did not answer within 30 seconds (is a Keychain / \
                 Credential Manager dialog waiting for approval, or the keychain locked?)"
                    .into(),
            ))
        })
    }

    fn entry(account: &str) -> Result<keyring::Entry, KeyError> {
        keyring::Entry::new(SERVICE, account).map_err(|e| KeyError::Unavailable(e.to_string()))
    }

    pub fn load(account: &str) -> Result<Option<DeviceKey>, KeyError> {
        let account = account.to_string();
        bounded(move || load_now(&account))
    }

    fn load_now(account: &str) -> Result<Option<DeviceKey>, KeyError> {
        match entry(account)?.get_secret() {
            Ok(b) => {
                let b = Zeroizing::new(b);
                seed_from(&b)
                    .map(Some)
                    .ok_or_else(|| KeyError::Platform("stored key has the wrong length".into()))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(KeyError::Platform(e.to_string())),
        }
    }

    pub fn store(account: &str, key: &DeviceKey) -> Result<(), KeyError> {
        let seed = key.seed();
        let account = account.to_string();
        bounded(move || {
            entry(&account)?
                .set_secret(&seed[..])
                .map_err(|e| KeyError::Platform(e.to_string()))
        })
    }

    pub fn delete(account: &str) -> Result<(), KeyError> {
        let account = account.to_string();
        bounded(move || delete_now(&account))
    }

    fn delete_now(account: &str) -> Result<(), KeyError> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeyError::Platform(e.to_string())),
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::*;

    fn none() -> KeyError {
        KeyError::Unavailable("this platform has no supported key store".into())
    }
    pub fn load(_: &str) -> Result<Option<DeviceKey>, KeyError> {
        Err(none())
    }
    pub fn store(_: &str, _: &DeviceKey) -> Result<(), KeyError> {
        Err(none())
    }
    pub fn delete(_: &str) -> Result<(), KeyError> {
        Err(none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_roundtrip_and_permissions() {
        let d = tempfile::tempdir().unwrap();
        let ks = KeyStorage::File(d.path().join("keys"));
        assert!(ks.load("device").unwrap().is_none());
        let (k, created) = ks.load_or_create("device").unwrap();
        assert!(created);
        let (k2, created2) = ks.load_or_create("device").unwrap();
        assert!(!created2);
        assert_eq!(k.fingerprint(), k2.fingerprint());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = d.path().join("keys/device.key");
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
            // a group-readable key file is refused
            fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(ks.load("device").is_err());
        }
        ks.delete("device").unwrap();
        assert!(ks.load("device").unwrap().is_none());
    }

    #[test]
    fn account_names_cannot_escape_the_directory() {
        let p = file_path(Path::new("/k"), "../../etc/passwd");
        assert_eq!(p.parent().unwrap(), Path::new("/k"));
    }

    /// Real platform store (macOS Keychain / Windows Credential Manager). Opt-in because it
    /// touches the user's keychain: `cargo test -p rustgit-host -- --ignored platform`.
    #[test]
    #[ignore]
    #[cfg(any(target_os = "macos", windows))]
    fn platform_store_roundtrip() {
        let ks = KeyStorage::Platform;
        let account = format!("rgit-test-{}", std::process::id());
        assert!(ks.load(&account).unwrap().is_none());
        let k = DeviceKey::generate();
        ks.store(&account, &k).unwrap();
        let back = ks.load(&account).unwrap().expect("stored key must load");
        assert_eq!(back.fingerprint(), k.fingerprint());
        ks.delete(&account).unwrap();
        assert!(ks.load(&account).unwrap().is_none());
    }
}
