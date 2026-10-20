//! Journaled staging area for incoming objects.
//!
//! Layout: `<root>/<session-hex>/SESSION` (who/what, informational) and
//! `<root>/<session-hex>/obj/<id-hex>` holding `type byte | logical bytes`.
//!
//! The journal is the filesystem's atomic rename: an object is written to `tmp/<id>.part`,
//! fsynced, renamed into `obj/`, and the `obj/` directory is fsynced before the batch is
//! acknowledged. A file in `obj/` therefore always holds a complete, hash-verified object
//! that the host promised to remember; a `.part` file is an interrupted write and is
//! deleted. Nothing in staging is visible to readers of the repository: only `publish`
//! moves objects into the pack store.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rustgit_protocol::SessionId;
use rustgit_store::{ObjectId, ObjectType};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone)]
pub struct Staging {
    root: PathBuf,
}

fn sync_dir(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(p)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        Ok(())
    }
}

impl Staging {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Staging { root })
    }

    fn dir(&self, s: &SessionId) -> PathBuf {
        self.root.join(hex(s))
    }

    fn obj_dir(&self, s: &SessionId) -> PathBuf {
        self.dir(s).join("obj")
    }

    /// Creates the session directory if needed, removes interrupted writes, and returns the
    /// number and total size of objects already staged.
    pub fn open_session(&self, s: &SessionId, info: &str) -> io::Result<(u64, u64)> {
        let d = self.dir(s);
        let created = !d.exists();
        fs::create_dir_all(d.join("obj"))?;
        fs::create_dir_all(d.join("tmp"))?;
        if created {
            let tmp = d.join("tmp").join("SESSION.part");
            let mut f = File::create(&tmp)?;
            f.write_all(info.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, d.join("SESSION"))?;
            sync_dir(&d)?;
            sync_dir(&self.root)?;
        }
        for e in fs::read_dir(d.join("tmp"))? {
            let _ = fs::remove_file(e?.path());
        }
        self.stat(s)
    }

    pub fn stat(&self, s: &SessionId) -> io::Result<(u64, u64)> {
        let (mut n, mut b) = (0u64, 0u64);
        let d = self.obj_dir(s);
        if !d.exists() {
            return Ok((0, 0));
        }
        for e in fs::read_dir(d)? {
            let e = e?;
            n += 1;
            b += e.metadata()?.len().saturating_sub(1);
        }
        Ok((n, b))
    }

    pub fn has(&self, s: &SessionId, id: &ObjectId) -> bool {
        self.obj_dir(s).join(id.to_hex()).is_file()
    }

    /// Stages one object. Returns false if it was already staged. Not durable until
    /// `commit_batch` is called.
    pub fn put(&self, s: &SessionId, ty: ObjectType, id: &ObjectId, data: &[u8]) -> io::Result<bool> {
        let name = id.to_hex();
        let fin = self.obj_dir(s).join(&name);
        if fin.is_file() {
            return Ok(false);
        }
        let tmp = self.dir(s).join("tmp").join(format!("{name}.part"));
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(&[ty as u8])?;
        f.write_all(data)?;
        // `sync_all` is F_FULLFSYNC on macOS and FlushFileBuffers on Windows.
        f.sync_all()?;
        fs::rename(&tmp, &fin)?;
        Ok(true)
    }

    /// Makes the renames of the batch durable.
    pub fn commit_batch(&self, s: &SessionId) -> io::Result<()> {
        sync_dir(&self.obj_dir(s))
    }

    /// Type of a staged object without reading its payload.
    pub fn staged_type(&self, s: &SessionId, id: &ObjectId) -> io::Result<Option<ObjectType>> {
        let p = self.obj_dir(s).join(id.to_hex());
        let mut f = match File::open(p) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut b = [0u8; 1];
        f.read_exact(&mut b)?;
        ObjectType::from_u8(b[0])
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    pub fn read(&self, s: &SessionId, id: &ObjectId) -> io::Result<Option<(ObjectType, Vec<u8>)>> {
        let p = self.obj_dir(s).join(id.to_hex());
        let mut v = match fs::read(p) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if v.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "empty staged file"));
        }
        let ty = ObjectType::from_u8(v[0])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        v.remove(0);
        Ok(Some((ty, v)))
    }

    pub fn remove_object(&self, s: &SessionId, id: &ObjectId) {
        let _ = fs::remove_file(self.obj_dir(s).join(id.to_hex()));
    }

    /// Deletes a session; returns the bytes it held.
    pub fn discard(&self, s: &SessionId) -> u64 {
        let freed = self.stat(s).map(|(_, b)| b).unwrap_or(0);
        let _ = fs::remove_dir_all(self.dir(s));
        freed
    }

    /// Total bytes staged by every session (a full directory scan; hosts keep a running
    /// counter and use this only at start-up).
    pub fn scan_total_bytes(&self) -> u64 {
        let mut t = 0;
        if let Ok(rd) = fs::read_dir(&self.root) {
            for e in rd.flatten() {
                if let Ok(od) = fs::read_dir(e.path().join("obj")) {
                    for o in od.flatten() {
                        t += o.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                }
            }
        }
        t
    }

    /// Deletes sessions untouched for longer than `ttl` and sessions in `keep` are spared.
    pub fn sweep(&self, ttl: Duration, in_use: &dyn Fn(&str) -> bool) -> usize {
        let mut removed = 0;
        let Ok(rd) = fs::read_dir(&self.root) else { return 0 };
        let now = SystemTime::now();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if in_use(&name) {
                continue;
            }
            let newest = newest_mtime(&e.path());
            if let Some(t) = newest {
                if now.duration_since(t).unwrap_or_default() > ttl {
                    let _ = fs::remove_dir_all(e.path());
                    removed += 1;
                }
            }
        }
        removed
    }
}

fn newest_mtime(p: &Path) -> Option<SystemTime> {
    let mut best = fs::metadata(p).ok()?.modified().ok()?;
    for sub in ["obj", "tmp"] {
        if let Ok(rd) = fs::read_dir(p.join(sub)) {
            if let Ok(m) = fs::metadata(p.join(sub)).and_then(|m| m.modified()) {
                best = best.max(m);
            }
            for e in rd.flatten() {
                if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                    best = best.max(t);
                }
            }
        }
    }
    Some(best)
}
