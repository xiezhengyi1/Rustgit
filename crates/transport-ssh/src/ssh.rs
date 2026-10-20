//! The client side: run the system `ssh` client with a structured argument list and speak the
//! protocol over its stdin/stdout.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustgit_protocol::{Duplex, Transport};

use crate::pipe::{PipeDuplex, STDIO_MARKER};
use crate::target::{shell_quote, validate_program, SshTarget, TargetError};

/// How host keys are treated. There is deliberately no variant that disables verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// `StrictHostKeyChecking=ask`: unknown keys prompt on the user's terminal; changed keys fail.
    Ask,
    /// `StrictHostKeyChecking=yes`: unknown or changed keys fail. Used when nothing can prompt.
    Strict,
    /// `StrictHostKeyChecking=accept-new`: trust a key the first time it is seen, fail if it
    /// later changes. Only ever selected by an explicit user opt-in, never by a URL.
    AcceptNew,
}

#[derive(Debug, Clone)]
pub struct SshOptions {
    /// The ssh executable (a program name or path, no arguments). Default `ssh`.
    pub program: OsString,
    /// Name or path of `rgit` on the remote machine. Default `rgit`.
    pub remote_program: String,
    /// `ssh -F`
    pub config_file: Option<PathBuf>,
    /// `ssh -i`
    pub identity_file: Option<PathBuf>,
    pub host_key_policy: HostKeyPolicy,
    /// `BatchMode=yes`: never ask for passwords or passphrases (for apps without a terminal).
    pub batch_mode: bool,
    /// Extra seconds allowed for connecting and logging in, beyond which the attempt fails.
    /// Interactive logins (passphrase prompts) may take a while.
    pub login_timeout: Duration,
    /// Echo the ssh client's stderr (warnings, host-key messages) to this process's stderr.
    pub forward_stderr: bool,
}

impl Default for SshOptions {
    fn default() -> Self {
        SshOptions {
            program: OsString::from("ssh"),
            remote_program: "rgit".into(),
            config_file: None,
            identity_file: None,
            host_key_policy: HostKeyPolicy::Ask,
            batch_mode: false,
            login_timeout: Duration::from_secs(120),
            forward_stderr: false,
        }
    }
}

fn env_flag(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
}

impl SshOptions {
    /// Options from the environment of the user running `rgit`. `interactive` says whether a
    /// person can answer prompts (the CLI passes "stdin is a terminal"); when not, ssh runs in
    /// `BatchMode` and unknown host keys are refused instead of prompting.
    ///
    /// * `RGIT_SSH` — ssh program (path only; never a shell command line)
    /// * `RGIT_SSH_CONFIG`, `RGIT_SSH_IDENTITY` — `-F` / `-i` files
    /// * `RGIT_SSH_ACCEPT_NEW_HOSTKEY=1` — trust unknown host keys on first use (still fails if a key changes)
    /// * `RGIT_SSH_BATCH=0|1` — override the interactive detection
    /// * `RGIT_REMOTE_RGIT` — the remote `rgit` command
    pub fn from_env(interactive: bool) -> Result<SshOptions, TargetError> {
        let mut o = SshOptions::default();
        if let Some(p) = std::env::var_os("RGIT_SSH").filter(|p| !p.is_empty()) {
            o.program = p;
        }
        if let Some(p) = std::env::var_os("RGIT_SSH_CONFIG").filter(|p| !p.is_empty()) {
            o.config_file = Some(p.into());
        }
        if let Some(p) = std::env::var_os("RGIT_SSH_IDENTITY").filter(|p| !p.is_empty()) {
            o.identity_file = Some(p.into());
        }
        if let Ok(p) = std::env::var("RGIT_REMOTE_RGIT") {
            if !p.is_empty() {
                validate_program(&p)?;
                o.remote_program = p;
            }
        }
        o.batch_mode = env_flag("RGIT_SSH_BATCH").unwrap_or(!interactive);
        o.host_key_policy = if env_flag("RGIT_SSH_ACCEPT_NEW_HOSTKEY") == Some(true) {
            HostKeyPolicy::AcceptNew
        } else if o.batch_mode {
            HostKeyPolicy::Strict
        } else {
            HostKeyPolicy::Ask
        };
        o.forward_stderr = true;
        Ok(o)
    }
}

/// The command string the remote shell receives. Both parts passed validation, so quoting is
/// only needed for spaces.
pub fn remote_command(target: &SshTarget, opts: &SshOptions) -> Result<String, TargetError> {
    validate_program(&opts.remote_program)?;
    crate::target::validate_path(&target.path)?;
    Ok(format!(
        "{} serve --stdio -- {}",
        shell_quote(&opts.remote_program),
        shell_quote(&target.path)
    ))
}

/// Complete `argv` (without the program) for the `ssh` process. Every value that originates
/// outside the program follows `--` or is the value of a dedicated option; none is parsed by a
/// local shell. Exposed for tests.
pub fn build_args(
    target: &SshTarget,
    opts: &SshOptions,
    connect_timeout: Duration,
) -> Result<Vec<OsString>, TargetError> {
    let mut a: Vec<OsString> = Vec::new();
    let mut o = |k: &str| {
        a.push("-o".into());
        a.push(k.into());
    };
    o(&format!("BatchMode={}", if opts.batch_mode { "yes" } else { "no" }));
    o(match opts.host_key_policy {
        HostKeyPolicy::Ask => "StrictHostKeyChecking=ask",
        HostKeyPolicy::Strict => "StrictHostKeyChecking=yes",
        HostKeyPolicy::AcceptNew => "StrictHostKeyChecking=accept-new",
    });
    // A transport for one protocol session: no forwarding, agent, X11 or local commands, even if
    // the user's ssh_config enables them for this host.
    o("ClearAllForwardings=yes");
    o("ForwardAgent=no");
    o("ForwardX11=no");
    o("PermitLocalCommand=no");
    o("ServerAliveInterval=15");
    o("ServerAliveCountMax=3");
    o(&format!("ConnectTimeout={}", connect_timeout.as_secs().clamp(1, 600)));
    a.push("-T".into());
    if let Some(f) = &opts.config_file {
        a.push("-F".into());
        a.push(f.clone().into());
    }
    if let Some(i) = &opts.identity_file {
        a.push("-i".into());
        a.push(i.clone().into());
    }
    if let Some(p) = target.hop.port {
        a.push("-p".into());
        a.push(p.to_string().into());
    }
    if let Some(u) = &target.hop.user {
        a.push("-l".into());
        a.push(u.clone().into());
    }
    if !target.jumps.is_empty() {
        a.push("-J".into());
        a.push(
            target
                .jumps
                .iter()
                .map(|h| h.jump_spec())
                .collect::<Vec<_>>()
                .join(",")
                .into(),
        );
    }
    a.push("--".into());
    a.push(target.hop.host.clone().into());
    a.push(remote_command(target, opts)?.into());
    Ok(a)
}

/// The last lines the ssh client wrote to stderr.
#[derive(Default)]
struct Tail {
    lines: VecDeque<String>,
    done: bool,
}

impl Tail {
    fn text(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

struct Session {
    child: Mutex<Child>,
    tail: Arc<Mutex<Tail>>,
    who: String,
    batch: bool,
    policy: HostKeyPolicy,
}

impl Session {
    fn wait_exit(&self, grace: Duration) -> Option<std::process::ExitStatus> {
        let end = Instant::now() + grace;
        loop {
            if let Ok(Some(s)) = self.child.lock().unwrap().try_wait() {
                return Some(s);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr_text(&self, grace: Duration) -> String {
        let end = Instant::now() + grace;
        while !self.tail.lock().unwrap().done && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.tail.lock().unwrap().text()
    }

    fn kill(&self) {
        let mut c = self.child.lock().unwrap();
        let _ = c.kill();
        let _ = c.wait();
    }

    fn reap(&self) {
        if self.wait_exit(Duration::from_millis(1500)).is_none() {
            self.kill();
        }
    }

    /// Turns "the stream ended" into an error that says what went wrong.
    fn end_error(&self) -> io::Error {
        let status = self.wait_exit(Duration::from_secs(2));
        let stderr = self.stderr_text(Duration::from_millis(500));
        classify(&self.who, status.map(|s| s.code()), &stderr, self.batch, self.policy)
    }
}

/// Maps the ssh client's exit status and stderr to an error with an actionable message.
/// The messages start with a fixed phrase the JSON interface maps to error codes.
pub fn classify(
    who: &str,
    status: Option<Option<i32>>,
    stderr: &str,
    batch: bool,
    policy: HostKeyPolicy,
) -> io::Error {
    let l = stderr.to_lowercase();
    let said = if stderr.trim().is_empty() {
        String::new()
    } else {
        format!(" ssh said: {}", stderr.trim().replace('\n', " | "))
    };
    let code_txt = match status {
        Some(Some(c)) => format!(" (ssh exit status {c})"),
        Some(None) => " (ssh was terminated by a signal)".into(),
        None => " (ssh did not exit)".into(),
    };
    let (kind, msg) = if l.contains("host key verification failed")
        || l.contains("remote host identification has changed")
        || l.contains("no matching host key")
    {
        let hint = if l.contains("identification has changed") {
            "the server's host key CHANGED since it was recorded in known_hosts. Do not continue until the server's owner confirms the new fingerprint; then remove the old entry with `ssh-keygen -R <host>`"
        } else if batch || policy == HostKeyPolicy::Strict {
            "the server's host key is not known and nothing can ask you. Connect once with `ssh <host>` in a terminal to check and record the fingerprint, or set RGIT_SSH_ACCEPT_NEW_HOSTKEY=1 to trust it on first use"
        } else {
            "the server's host key was not accepted. Check the fingerprint with the server's owner and answer `yes` only if it matches"
        };
        (
            io::ErrorKind::PermissionDenied,
            format!("ssh host key verification failed for {who}: {hint}.{said}"),
        )
    } else if l.contains("permission denied") || l.contains("too many authentication failures") {
        (
            io::ErrorKind::PermissionDenied,
            format!("ssh authentication failed for {who}: the server refused your keys or password (is your key loaded in ssh-agent or listed in ~/.ssh/config?).{said}"),
        )
    } else if l.contains("could not resolve hostname") || l.contains("name or service not known") || l.contains("nodename nor servname") {
        (
            io::ErrorKind::NotFound,
            format!("ssh could not resolve the host of {who}.{said}"),
        )
    } else if l.contains("connection refused") {
        (
            io::ErrorKind::ConnectionRefused,
            format!("ssh connection refused by {who}: nothing is accepting SSH connections at that address and port.{said}"),
        )
    } else if l.contains("timed out") || l.contains("no route to host") || l.contains("network is unreachable") {
        (
            io::ErrorKind::TimedOut,
            format!("ssh could not reach {who}: the connection timed out or the network is unreachable.{said}"),
        )
    } else if matches!(status, Some(Some(127))) || l.contains("command not found") {
        (
            io::ErrorKind::NotFound,
            format!("ssh connected to {who}, but the remote machine has no `rgit` command on its PATH for non-interactive logins; install rgit there or set RGIT_REMOTE_RGIT to its full path.{said}"),
        )
    } else if l.contains("kex_exchange_identification") || l.contains("connection closed by") || l.contains("connection reset") {
        (
            io::ErrorKind::ConnectionReset,
            format!("ssh connection to {who} was closed by the server.{said}"),
        )
    } else {
        (
            io::ErrorKind::UnexpectedEof,
            format!("ssh session with {who} ended{code_txt}.{said}"),
        )
    };
    io::Error::new(kind, msg)
}

pub struct SshTransport {
    target: SshTarget,
    opts: SshOptions,
}

impl SshTransport {
    pub fn new(target: SshTarget, opts: SshOptions) -> Result<Self, TargetError> {
        // fail at construction, not at the first connect
        validate_program(&opts.remote_program)?;
        Ok(SshTransport { target, opts })
    }

    pub fn from_url(url: &str, opts: SshOptions) -> Result<Self, TargetError> {
        Self::new(SshTarget::parse(url)?, opts)
    }

    pub fn target(&self) -> &SshTarget {
        &self.target
    }
}

fn pump_stderr(mut err: impl Read + Send + 'static, tail: Arc<Mutex<Tail>>, forward: bool) {
    std::thread::spawn(move || {
        let mut r = io::BufReader::new(&mut err);
        loop {
            let mut line = Vec::new();
            match (&mut r).take(4096).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line).trim_end().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    if forward {
                        let _ = writeln!(io::stderr(), "{text}");
                    }
                    let mut t = tail.lock().unwrap();
                    if t.lines.len() >= 12 {
                        t.lines.pop_front();
                    }
                    t.lines.push_back(text);
                }
            }
        }
        tail.lock().unwrap().done = true;
    });
}

impl Transport for SshTransport {
    fn connect(&self, timeout: Duration) -> io::Result<Box<dyn Duplex>> {
        // `wait` bounds the whole login (a person may be typing a passphrase); ssh's own
        // ConnectTimeout only covers reaching the server, so a dead host fails fast.
        let wait = timeout.max(self.opts.login_timeout);
        let args = build_args(&self.target, &self.opts, timeout.max(Duration::from_secs(10)))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let mut cmd = Command::new(&self.opts.program);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash from GUI apps
        }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "ssh client not found ({}): install the OpenSSH client or set RGIT_SSH to its path",
                        self.opts.program.to_string_lossy()
                    ),
                )
            } else {
                io::Error::new(e.kind(), format!("cannot start ssh: {e}"))
            }
        })?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let tail = Arc::new(Mutex::new(Tail::default()));
        pump_stderr(stderr, tail.clone(), self.opts.forward_stderr);

        let who = self.target.describe();
        let session = Arc::new(Session {
            child: Mutex::new(child),
            tail,
            who: who.clone(),
            batch: self.opts.batch_mode,
            policy: self.opts.host_key_policy,
        });
        let s1 = session.clone();
        let s2 = session.clone();
        let reaped = Arc::new(AtomicBool::new(false));
        let mut duplex = PipeDuplex::new(stdout, stdin, who.clone())
            .with_end_error(move || s1.end_error())
            .with_on_close(move || {
                if !reaped.swap(true, Ordering::SeqCst) {
                    s2.reap();
                }
            });
        match duplex.sync_to_marker(STDIO_MARKER, 64 * 1024, wait) {
            Ok(noise) => {
                if noise.iter().any(|b| !b.is_ascii_whitespace()) && self.opts.forward_stderr {
                    let _ = writeln!(
                        io::stderr(),
                        "note: ignored {} bytes the remote shell printed before rgit started (check its login scripts)",
                        noise.len()
                    );
                }
                Ok(Box::new(duplex))
            }
            Err(e) => {
                session.kill();
                drop(duplex);
                if e.kind() == io::ErrorKind::TimedOut {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("ssh to {who} did not produce a session within {} s{}", wait.as_secs(), {
                            let t = session.tail.lock().unwrap().text();
                            if t.is_empty() { String::new() } else { format!(". ssh said: {}", t.replace('\n', " | ")) }
                        }),
                    ));
                }
                Err(e)
            }
        }
    }

    fn describe(&self) -> String {
        self.target.describe()
    }
}
