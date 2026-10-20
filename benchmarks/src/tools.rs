//! Child-process runner with exact CPU/memory accounting, directory sizing, and the
//! Git / rgit adapters used by the benchmark.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

pub type R<T> = Result<T, String>;

static DELTA_DEPTH: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

pub fn set_delta_depth(d: u32) {
    let _ = DELTA_DEPTH.set(d);
}

pub fn delta_depth() -> Option<u32> {
    DELTA_DEPTH.get().copied()
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Usage {
    pub wall_s: f64,
    pub cpu_s: f64,
    pub max_rss_bytes: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.wall_s += o.wall_s;
        self.cpu_s += o.cpu_s;
        self.max_rss_bytes = self.max_rss_bytes.max(o.max_rss_bytes);
    }
}

/// Runs a command to completion, reaping it with wait4 to obtain its resource usage.
pub fn run(cmd: &mut Command) -> R<Usage> {
    let errfile = std::env::temp_dir().join(format!("bench-stderr-{}", std::process::id()));
    let err = File::create(&errfile).map_err(|e| e.to_string())?;
    cmd.stdout(Stdio::null()).stderr(Stdio::from(err));
    let start = Instant::now();
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn {:?}: {e}", cmd.get_program()))?;
    let pid = child.id() as libc::pid_t;
    let mut status: libc::c_int = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { libc::wait4(pid, &mut status, 0, &mut ru) };
        if r == pid {
            break;
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("wait4: {e}"));
        }
    }
    let wall = start.elapsed().as_secs_f64();
    drop(child);
    if !(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0) {
        let msg = fs::read_to_string(&errfile).unwrap_or_default();
        return Err(format!(
            "{:?} {:?} failed (status {status}): {}",
            cmd.get_program(),
            cmd.get_args().collect::<Vec<_>>(),
            msg.trim()
        ));
    }
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    let rss = if cfg!(target_os = "macos") {
        ru.ru_maxrss as u64
    } else {
        ru.ru_maxrss as u64 * 1024
    };
    Ok(Usage {
        wall_s: wall,
        cpu_s: tv(ru.ru_utime) + tv(ru.ru_stime),
        max_rss_bytes: rss,
    })
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Size {
    pub apparent: u64,
    pub allocated: u64,
}

pub fn dir_size(p: &Path) -> Size {
    fn walk(p: &Path, s: &mut Size) {
        let Ok(rd) = fs::read_dir(p) else { return };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                walk(&e.path(), s);
            } else {
                s.apparent += m.len();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    s.allocated += m.blocks() * 512;
                }
            }
        }
    }
    let mut s = Size::default();
    walk(p, &mut s);
    s
}

/// Sizes of each top-level entry (file or directory) of a repository directory.
pub fn breakdown(p: &Path) -> BTreeMap<String, Size> {
    let mut out = BTreeMap::new();
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(m) = e.metadata() else { continue };
            let sz = if m.is_dir() {
                dir_size(&e.path())
            } else {
                let mut s = Size {
                    apparent: m.len(),
                    allocated: 0,
                };
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    s.allocated = m.blocks() * 512;
                }
                s
            };
            out.insert(name, sz);
        }
    }
    out
}

/// Samples the size of `dir` in the background to find the peak during maintenance.
pub struct PeakSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    h: Option<std::thread::JoinHandle<()>>,
}

impl PeakSampler {
    pub fn start(dir: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let (s2, p2) = (stop.clone(), peak.clone());
        let h = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                let sz = dir_size(&dir).allocated;
                p2.fetch_max(sz, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        PeakSampler {
            stop,
            peak,
            h: Some(h),
        }
    }
    pub fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.h.take() {
            let _ = h.join();
        }
        self.peak.load(Ordering::Relaxed)
    }
}

pub fn copy_dir(src: &Path, dst: &Path) -> R<()> {
    let st = Command::new("cp")
        .arg("-R")
        .arg(src)
        .arg(dst)
        .status()
        .map_err(|e| e.to_string())?;
    if st.success() {
        Ok(())
    } else {
        Err("cp failed".into())
    }
}

/// Bytes that cross the network (or would, for Git's file-based equivalents) when one copy
/// of the history moves to another: a full copy and an increment of the last commits.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Transfer {
    /// Git: size of `git bundle create <f> main`. rgit: wire bytes sent by `rgit push` to an
    /// empty host (frames, compression and acknowledgements included).
    pub initial_upload_bytes: u64,
    /// Git: bytes `upload-pack` wrote to the client during `git clone --no-local` of `main`
    /// (ref advertisement, side-band framing and the pack). rgit: wire bytes received by
    /// `rgit fetch` into an empty repository.
    pub initial_download_bytes: u64,
    /// First-parent distance between the old and new tip, and commits in the increment.
    pub incremental_back: u64,
    pub incremental_commits: u64,
    /// Git: incremental bundle `main ^old`. rgit: wire bytes sent by the push that follows an
    /// earlier push of the old tip.
    pub incremental_upload_bytes: u64,
    /// Git: bytes `upload-pack` wrote during a fetch into a repository that has the old tip.
    /// rgit: wire bytes received by the fetch.
    pub incremental_download_bytes: u64,
    /// Client-to-host bytes of the download (wants/haves, requests). Git: bytes written to
    /// `upload-pack`'s stdin; rgit: bytes sent by the fetching client.
    pub initial_download_request_bytes: u64,
    pub incremental_download_request_bytes: u64,
    pub initial_upload_s: f64,
    pub initial_download_s: f64,
    pub incremental_upload_s: f64,
    pub incremental_download_s: f64,
    /// rgit only: logical (uncompressed) bytes of the objects sent in the initial upload.
    pub initial_upload_logical_bytes: u64,
    pub notes: Vec<String>,
}

pub struct Server {
    child: std::process::Child,
    pub addr: String,
}

impl Server {
    pub fn start(rgit: &Path, dir: &Path) -> R<Server> {
        let pf = dir.with_extension("port");
        let _ = fs::remove_file(&pf);
        let child = Command::new(rgit)
            .arg("serve")
            .arg(dir)
            .args(["--init", "--insecure-open-access", "--listen", "127.0.0.1:0", "--port-file"])
            .arg(&pf)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn rgit serve: {e}"))?;
        let start = Instant::now();
        loop {
            if let Ok(a) = fs::read_to_string(&pf) {
                if !a.is_empty() {
                    return Ok(Server { child, addr: a });
                }
            }
            if start.elapsed() > Duration::from_secs(30) {
                return Err("rgit serve did not start".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A stand-in for `git-upload-pack` that copies both directions of the conversation to files,
/// so the bytes a fetch really exchanges can be counted.
fn counting_upload_pack(dir: &Path) -> R<PathBuf> {
    let script = dir.join("counting-upload-pack.sh");
    fs::write(
        &script,
        "#!/bin/sh\ntee \"$GIT_BENCH_IN\" | git upload-pack \"$@\" | tee \"$GIT_BENCH_OUT\"\n",
    )
    .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(script)
}

fn file_bytes(p: &Path) -> u64 {
    fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn stdout_of(cmd: &mut Command) -> R<String> {
    let o = cmd.output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&o.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

fn timed_stdout(cmd: &mut Command) -> R<(String, f64)> {
    let t = Instant::now();
    let s = stdout_of(cmd)?;
    Ok((s, t.elapsed().as_secs_f64()))
}

fn after(s: &str, key: &str) -> u64 {
    s.find(key)
        .map(|i| {
            s[i + key.len()..]
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

pub trait Tool {
    fn name(&self) -> &'static str;
    fn meta_dir(&self) -> &'static str;
    fn version(&self) -> String;
    fn init(&self, dir: &Path) -> R<()>;
    fn commit(&self, dir: &Path, msg: &str) -> R<Usage>;
    fn branch_create(&self, dir: &Path, name: &str) -> R<()>;
    fn switch(&self, dir: &Path, name: &str) -> R<Usage>;
    fn merge(&self, dir: &Path, name: &str) -> R<()>;
    /// Named maintenance variants applicable to this tool, in the order to report.
    fn maintenance_variants(&self) -> Vec<&'static str>;
    fn maintain(&self, dir: &Path, variant: &str) -> R<Usage>;
    fn object_count(&self, dir: &Path) -> R<Option<u64>>;
    /// Measures transfer bytes for the `main` branch of the repository whose project (or
    /// metadata-only copy) is `dir`, using `scratch` for temporary repositories.
    fn transfer(&self, dir: &Path, scratch: &Path) -> R<Transfer>;
}

pub struct Git;

impl Git {
    fn g(&self, dir: &Path) -> Command {
        let mut c = Command::new("git");
        c.current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
        c
    }
}

impl Tool for Git {
    fn name(&self) -> &'static str {
        "git"
    }
    fn meta_dir(&self) -> &'static str {
        ".git"
    }
    fn version(&self) -> String {
        let o = Command::new("git").arg("--version").output().ok();
        o.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
    fn init(&self, dir: &Path) -> R<()> {
        run(self.g(dir).args(["init", "-q", "-b", "main"]))?;
        for (k, v) in [
            ("user.name", "Bench"),
            ("user.email", "bench@example.com"),
            ("gc.auto", "0"),
            ("gc.autoDetach", "false"),
            ("maintenance.auto", "false"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("core.fsmonitor", "false"),
        ] {
            run(self.g(dir).args(["config", k, v]))?;
        }
        Ok(())
    }
    fn commit(&self, dir: &Path, msg: &str) -> R<Usage> {
        let mut u = run(self.g(dir).args(["add", "-A"]))?;
        u.add(&run(self.g(dir).args(["commit", "-q", "-m", msg]))?);
        Ok(u)
    }
    fn branch_create(&self, dir: &Path, name: &str) -> R<()> {
        run(self.g(dir).args(["branch", name])).map(|_| ())
    }
    fn switch(&self, dir: &Path, name: &str) -> R<Usage> {
        run(self.g(dir).args(["switch", "-q", name]))
    }
    fn merge(&self, dir: &Path, name: &str) -> R<()> {
        run(self.g(dir).args(["merge", "-q", "--no-edit", name])).map(|_| ())
    }
    fn maintenance_variants(&self) -> Vec<&'static str> {
        vec!["pre", "gc_default", "gc_aggressive"]
    }
    fn maintain(&self, dir: &Path, variant: &str) -> R<Usage> {
        match variant {
            "pre" => Ok(Usage::default()),
            "gc_default" => {
                let mut u = run(self
                    .g(dir)
                    .args(["reflog", "expire", "--expire=now", "--all"]))?;
                u.add(&run(self.g(dir).args(["gc", "-q", "--prune=now"]))?);
                Ok(u)
            }
            "gc_aggressive" => {
                let mut u = run(self
                    .g(dir)
                    .args(["reflog", "expire", "--expire=now", "--all"]))?;
                u.add(&run(self.g(dir).args([
                    "gc",
                    "-q",
                    "--aggressive",
                    "--prune=now",
                ]))?);
                Ok(u)
            }
            v => Err(format!("unknown variant {v}")),
        }
    }
    fn object_count(&self, dir: &Path) -> R<Option<u64>> {
        let o = self
            .g(dir)
            .args(["count-objects", "-v"])
            .output()
            .map_err(|e| e.to_string())?;
        let t = String::from_utf8_lossy(&o.stdout);
        let n = |k: &str| {
            t.lines()
                .find_map(|l| l.strip_prefix(k))
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(0)
        };
        Ok(Some(n("count: ") + n("in-pack: ")))
    }

    fn transfer(&self, dir: &Path, scratch: &Path) -> R<Transfer> {
        let mut t = Transfer::default();
        let tmp = scratch.join("transfer");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let src_url = format!("file://{}", dir.display());
        // Full copy as a bundle.
        let bundle = tmp.join("full.bundle");
        let (_, s) = timed_stdout(self.g(dir).args(["bundle", "create"]).arg(&bundle).arg("main"))?;
        t.initial_upload_s = s;
        t.initial_upload_bytes = file_bytes(&bundle);
        // Full copy as a fetch: count what upload-pack exchanges with the client.
        let wrapper = counting_upload_pack(&tmp)?;
        let (inb, outb) = (tmp.join("in.bytes"), tmp.join("out.bytes"));
        let clone = tmp.join("clone.git");
        let (_, s) = timed_stdout(
            self.g(&tmp)
                .env("GIT_BENCH_IN", &inb)
                .env("GIT_BENCH_OUT", &outb)
                .args(["clone", "-q", "--bare", "--no-local", "--single-branch", "-b", "main"])
                .arg("--upload-pack")
                .arg(&wrapper)
                .arg(&src_url)
                .arg(&clone),
        )?;
        t.initial_download_s = s;
        t.initial_download_bytes = file_bytes(&outb);
        t.initial_download_request_bytes = file_bytes(&inb);
        // Increment: the last `back` first-parent commits.
        let total: u64 = stdout_of(self.g(dir).args(["rev-list", "--first-parent", "--count", "main"]))?
            .trim()
            .parse()
            .unwrap_or(0);
        let back = (total / 4).clamp(1, 10);
        t.incremental_back = back;
        let base = stdout_of(self.g(dir).args(["rev-parse", &format!("main~{back}")]))?
            .trim()
            .to_string();
        t.incremental_commits = stdout_of(self.g(dir).args(["rev-list", "--count", "main", &format!("^{base}")]))?
            .trim()
            .parse()
            .unwrap_or(0);
        let inc = tmp.join("inc.bundle");
        let (_, s) = timed_stdout(
            self.g(dir)
                .args(["bundle", "create"])
                .arg(&inc)
                .arg("main")
                .arg(format!("^{base}")),
        )?;
        t.incremental_upload_s = s;
        t.incremental_upload_bytes = file_bytes(&inc);
        let old = tmp.join("old.git");
        stdout_of(self.g(&tmp).args(["init", "-q", "--bare"]).arg(&old))?;
        stdout_of(
            self.g(dir)
                .args(["-c", "receive.unpackLimit=1"])
                .arg("push")
                .arg("-q")
                .arg(&old)
                .arg(format!("{base}:refs/heads/main")),
        )?;
        let (_, s) = timed_stdout(
            self.g(&old)
                .env("GIT_BENCH_IN", &inb)
                .env("GIT_BENCH_OUT", &outb)
                .args(["fetch", "-q", "--no-tags", "--upload-pack"])
                .arg(&wrapper)
                .args([&src_url, "main:refs/heads/new"]),
        )?;
        t.incremental_download_s = s;
        t.incremental_download_bytes = file_bytes(&outb);
        t.incremental_download_request_bytes = file_bytes(&inb);
        t.notes.push("git download bytes = what upload-pack wrote to the client (counted through a tee wrapper); git upload = `git bundle create` size".into());
        let _ = fs::remove_dir_all(&tmp);
        Ok(t)
    }
}

pub struct Rgit {
    pub bin: PathBuf,
}

impl Rgit {
    fn r(&self, dir: &Path) -> Command {
        let mut c = Command::new(&self.bin);
        c.arg("-C").arg(dir);
        c
    }
}

impl Tool for Rgit {
    fn name(&self) -> &'static str {
        "rgit"
    }
    fn meta_dir(&self) -> &'static str {
        ".rgit"
    }
    fn version(&self) -> String {
        let o = Command::new(&self.bin).arg("--version").output().ok();
        o.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
    fn init(&self, dir: &Path) -> R<()> {
        run(self.r(dir).arg("init"))?;
        run(self.r(dir).args(["config", "user.name", "Bench"]))?;
        run(self
            .r(dir)
            .args(["config", "user.email", "bench@example.com"]))?;
        if let Some(d) = delta_depth() {
            run(self
                .r(dir)
                .args(["config", "delta.max_depth", &d.to_string()]))?;
        }
        Ok(())
    }
    fn commit(&self, dir: &Path, msg: &str) -> R<Usage> {
        run(self.r(dir).args(["commit", "-m", msg]))
    }
    fn branch_create(&self, dir: &Path, name: &str) -> R<()> {
        run(self.r(dir).args(["branch", name])).map(|_| ())
    }
    fn switch(&self, dir: &Path, name: &str) -> R<Usage> {
        run(self.r(dir).args(["switch", name]))
    }
    fn merge(&self, dir: &Path, name: &str) -> R<()> {
        run(self.r(dir).args(["merge", name])).map(|_| ())
    }
    fn maintenance_variants(&self) -> Vec<&'static str> {
        vec!["pre", "gc_aggressive"]
    }
    fn maintain(&self, dir: &Path, variant: &str) -> R<Usage> {
        match variant {
            "pre" => Ok(Usage::default()),
            "gc_aggressive" => run(self.r(dir).args(["gc", "--aggressive"])),
            v => Err(format!("unknown variant {v}")),
        }
    }
    fn object_count(&self, dir: &Path) -> R<Option<u64>> {
        let o = self
            .r(dir)
            .arg("stats")
            .output()
            .map_err(|e| e.to_string())?;
        let t = String::from_utf8_lossy(&o.stdout);
        Ok(t.lines()
            .find_map(|l| l.strip_prefix("objects:"))
            .and_then(|v| v.trim().parse().ok()))
    }
    fn transfer(&self, dir: &Path, scratch: &Path) -> R<Transfer> {
        let mut t = Transfer::default();
        let tmp = scratch.join("transfer");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let wire = |s: &str, key: &str| after(s, key);
        // --- full copy: push main to an empty host, fetch it into an empty repository ---
        let srv = Server::start(&self.bin, &tmp.join("host1"))?;
        let url = format!("tcp://{}", srv.addr);
        stdout_of(self.r(dir).args(["remote", "add", "origin", &url]))?;
        let (o, s) = timed_stdout(self.r(dir).args(["push", "origin", "main"]))?;
        t.initial_upload_s = s;
        t.initial_upload_bytes = wire(&o, "wire bytes sent:");
        t.initial_upload_logical_bytes = after(&o, "object(s) (");
        let c1 = tmp.join("c1");
        fs::create_dir_all(&c1).map_err(|e| e.to_string())?;
        stdout_of(self.r(&c1).arg("init"))?;
        stdout_of(self.r(&c1).args(["remote", "add", "origin", &url]))?;
        let (o, s) = timed_stdout(self.r(&c1).arg("fetch"))?;
        t.initial_download_s = s;
        t.initial_download_bytes = wire(&o, "received:");
        t.initial_download_request_bytes = wire(&o, "wire bytes sent:");
        drop(srv);
        // --- increment: host already has main~back; push main, then fetch it elsewhere ---
        // `rgit log` prints one unindented header line per commit.
        let count = |o: &str| o.lines().filter(|l| !l.is_empty() && !l.starts_with(' ')).count() as u64;
        let total = count(&stdout_of(self.r(dir).args(["log", "-n", "100000", "main"]))?);
        let back = (total / 4).clamp(1, 10);
        t.incremental_back = back;
        let srv2 = Server::start(&self.bin, &tmp.join("host2"))?;
        let url2 = format!("tcp://{}", srv2.addr);
        stdout_of(self.r(dir).args(["remote", "add", "origin", &url2]))?;
        stdout_of(self.r(dir).args(["branch", "bench-old", "--from", &format!("main~{back}")]))?;
        stdout_of(self.r(dir).args(["push", "origin", "bench-old", "--as", "main"]))?;
        let c2 = tmp.join("c2");
        fs::create_dir_all(&c2).map_err(|e| e.to_string())?;
        stdout_of(self.r(&c2).arg("init"))?;
        stdout_of(self.r(&c2).args(["remote", "add", "origin", &url2]))?;
        stdout_of(self.r(&c2).arg("fetch"))?;
        let (o, s) = timed_stdout(self.r(dir).args(["push", "origin", "main"]))?;
        t.incremental_upload_s = s;
        t.incremental_upload_bytes = wire(&o, "wire bytes sent:");
        let (o, s) = timed_stdout(self.r(&c2).arg("fetch"))?;
        t.incremental_download_s = s;
        t.incremental_download_bytes = wire(&o, "received:");
        t.incremental_download_request_bytes = wire(&o, "wire bytes sent:");
        // Commits in the increment: reachable from main but not from main~back.
        t.incremental_commits = {
            let old = count(&stdout_of(self.r(dir).args(["log", "-n", "100000", "bench-old"]))?);
            total.saturating_sub(old)
        };
        let _ = stdout_of(self.r(dir).args(["branch", "bench-old", "-d", "--force"]));
        t.notes.push("rgit bytes are measured on the connection (frames, per-batch zstd, acknowledgements); objects are sent whole (no cross-object deltas on the wire yet)".into());
        drop(srv2);
        let _ = fs::remove_dir_all(&tmp);
        Ok(t)
    }
}
