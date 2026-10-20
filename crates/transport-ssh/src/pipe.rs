//! A [`Duplex`] over any pair of blocking byte streams (a child's stdout/stdin, this
//! process's own stdin/stdout, or two OS pipes in tests).
//!
//! Pipes have no read/write timeouts, but the protocol's `Channel` depends on them (idle
//! detection, stalled frames). A reader thread moves data into a small bounded queue and a
//! writer thread drains another one, so `read`/`write`/`flush` can honour deadlines and so a
//! stuck peer cannot block the caller forever. The queues are bounded (about 1 MiB each), which
//! also gives backpressure.
//!
//! The byte stream is never interpreted here, except for [`PipeDuplex::sync_to_marker`], which
//! lets a client skip noise (a login banner printed by the remote shell's rc files) before the
//! server's `RGIT-STDIO/1` line.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustgit_protocol::Duplex;

/// Sent by `rgit serve --stdio` before the first frame. A client scans for this line.
pub const STDIO_MARKER: &[u8] = b"RGIT-STDIO/1";

const CHUNK: usize = 64 * 1024;
const QUEUE: usize = 16;

enum Incoming {
    Data(Vec<u8>),
    Eof,
    Err(io::Error),
}

#[derive(Default)]
struct WriteState {
    /// chunks handed to the writer thread and not yet written
    pending: usize,
    failed: Option<(io::ErrorKind, String)>,
}

type EofHook = Box<dyn Fn() -> io::Error + Send + Sync>;

pub struct PipeDuplex {
    rx: Receiver<Incoming>,
    buf: VecDeque<u8>,
    eof: bool,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    tx: Option<SyncSender<Vec<u8>>>,
    wstate: Arc<(Mutex<WriteState>, Condvar)>,
    writer: Option<JoinHandle<()>>,
    label: String,
    /// Builds a richer error when the stream ends or breaks (e.g. `ssh`'s exit status and stderr).
    on_end: Option<EofHook>,
    on_close: Option<Box<dyn FnOnce() + Send>>,
    closed: Arc<AtomicBool>,
}

impl PipeDuplex {
    pub fn new(
        mut reader: impl Read + Send + 'static,
        mut writer: impl Write + Send + 'static,
        label: impl Into<String>,
    ) -> Self {
        let (itx, irx) = sync_channel::<Incoming>(QUEUE);
        std::thread::Builder::new()
            .name("rgit-pipe-reader".into())
            .spawn(move || loop {
                let mut b = vec![0u8; CHUNK];
                match reader.read(&mut b) {
                    Ok(0) => {
                        let _ = itx.send(Incoming::Eof);
                        return;
                    }
                    Ok(n) => {
                        b.truncate(n);
                        if itx.send(Incoming::Data(b)).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        let _ = itx.send(Incoming::Err(e));
                        return;
                    }
                }
            })
            .expect("spawn reader thread");

        let (otx, orx) = sync_channel::<Vec<u8>>(QUEUE);
        let wstate = Arc::new((Mutex::new(WriteState::default()), Condvar::new()));
        let ws = wstate.clone();
        let wt = std::thread::Builder::new()
            .name("rgit-pipe-writer".into())
            .spawn(move || {
                while let Ok(chunk) = orx.recv() {
                    let r = writer.write_all(&chunk).and_then(|_| writer.flush());
                    let (m, cv) = &*ws;
                    let mut st = m.lock().unwrap();
                    st.pending = st.pending.saturating_sub(1);
                    if let Err(e) = r {
                        st.failed = Some((e.kind(), e.to_string()));
                        cv.notify_all();
                        return; // dropping `orx` makes later sends fail
                    }
                    cv.notify_all();
                }
                let _ = writer.flush();
            })
            .expect("spawn writer thread");

        PipeDuplex {
            rx: irx,
            buf: VecDeque::new(),
            eof: false,
            read_timeout: None,
            write_timeout: None,
            tx: Some(otx),
            wstate,
            writer: Some(wt),
            label: label.into(),
            on_end: None,
            on_close: None,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Replaces the plain "closed"/"broken pipe" errors by `f()`'s.
    pub fn with_end_error(mut self, f: impl Fn() -> io::Error + Send + Sync + 'static) -> Self {
        self.on_end = Some(Box::new(f));
        self
    }

    /// Runs once when the duplex is shut down or dropped (used to reap the `ssh` child).
    pub fn with_on_close(mut self, f: impl FnOnce() + Send + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    fn end_error(&self, fallback: io::Error) -> io::Error {
        match &self.on_end {
            Some(f) => f(),
            None => fallback,
        }
    }

    fn next_chunk(&mut self, deadline: Option<Instant>) -> io::Result<bool> {
        if self.eof {
            return Ok(false);
        }
        let got = match deadline {
            None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(d) => self.rx.recv_timeout(d.saturating_duration_since(Instant::now())),
        };
        match got {
            Ok(Incoming::Data(d)) => {
                self.buf.extend(d);
                Ok(true)
            }
            Ok(Incoming::Eof) | Err(RecvTimeoutError::Disconnected) => {
                self.eof = true;
                Ok(false)
            }
            Ok(Incoming::Err(e)) => {
                self.eof = true;
                Err(self.end_error(e))
            }
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{}: read timed out", self.label),
            )),
        }
    }

    /// Discards bytes until a whole line equal to `marker` has been read (anything before it
    /// is returned as noise, bounded by `limit`). Bytes after the marker stay readable.
    pub fn sync_to_marker(
        &mut self,
        marker: &[u8],
        limit: usize,
        timeout: Duration,
    ) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut noise: Vec<u8> = Vec::new();
        loop {
            // look for "\nMARKER\n" or "MARKER\n" at the very start
            let (a, b) = self.buf.as_slices();
            let mut all: Vec<u8> = Vec::with_capacity(a.len() + b.len());
            all.extend_from_slice(a);
            all.extend_from_slice(b);
            let mut line_start = 0usize;
            while let Some(nl) = all[line_start..].iter().position(|&c| c == b'\n') {
                let end = line_start + nl;
                let line = &all[line_start..end];
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line == marker {
                    noise.extend_from_slice(&all[..line_start]);
                    self.buf.drain(..end + 1);
                    return Ok(noise);
                }
                line_start = end + 1;
            }
            if all.len() > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{}: no RGIT-STDIO marker within {limit} bytes; the remote command is not `rgit serve --stdio`",
                        self.label
                    ),
                ));
            }
            // complete lines before `line_start` are noise; keep the partial tail
            if line_start > 0 {
                noise.extend_from_slice(&all[..line_start]);
                self.buf.drain(..line_start);
            }
            if !self.next_chunk(Some(deadline))? {
                let fallback = io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{}: connection closed before the server announced itself", self.label),
                );
                return Err(self.end_error(fallback));
            }
        }
    }

    fn close_once(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        // Let the writer finish what is queued, then end it (this drops the child's stdin,
        // which the remote side sees as end-of-input).
        self.tx = None;
        if let Some(w) = self.writer.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !w.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if w.is_finished() {
                let _ = w.join();
            }
            // else: stuck in a blocked write; `on_close` kills the child, which unblocks it
        }
        if let Some(f) = self.on_close.take() {
            f();
        }
    }
}

impl Read for PipeDuplex {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.buf.is_empty() {
            let deadline = self.read_timeout.map(|t| Instant::now() + t);
            if !self.next_chunk(deadline)? {
                // end of stream: for a peer that died, say why instead of "closed"
                if self.on_end.is_some() {
                    return Err(self.end_error(io::Error::from(io::ErrorKind::UnexpectedEof)));
                }
                return Ok(0);
            }
        }
        let n = out.len().min(self.buf.len());
        for (i, b) in self.buf.drain(..n).enumerate() {
            out[i] = b;
        }
        Ok(n)
    }
}

impl PipeDuplex {
    fn adjust_pending(&self, add: bool) {
        let mut st = self.wstate.0.lock().unwrap();
        if add {
            st.pending += 1;
        } else {
            st.pending = st.pending.saturating_sub(1);
        }
    }

    fn failure(&self) -> Option<io::Error> {
        let st = self.wstate.0.lock().unwrap();
        st.failed.as_ref().map(|(k, m)| io::Error::new(*k, m.clone()))
    }
}

impl Write for PipeDuplex {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if let Some(e) = self.failure() {
            return Err(self.end_error_if_broken(e));
        }
        let n = data.len().min(CHUNK);
        let deadline = self.write_timeout.map(|t| Instant::now() + t);
        if self.tx.is_none() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "connection is shut down"));
        }
        self.adjust_pending(true);
        let mut chunk = data[..n].to_vec();
        loop {
            let sent = self.tx.as_ref().unwrap().try_send(chunk);
            match sent {
                Ok(()) => return Ok(n),
                Err(TrySendError::Disconnected(_)) => {
                    self.adjust_pending(false);
                    let e = io::Error::new(io::ErrorKind::BrokenPipe, "the connection's write side has failed");
                    return Err(self.end_error_if_broken(e));
                }
                Err(TrySendError::Full(c)) => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        self.adjust_pending(false);
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("{}: write timed out", self.label),
                        ));
                    }
                    chunk = c;
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }

    /// Waits until everything written so far has been handed to the OS (or fails/times out).
    fn flush(&mut self) -> io::Result<()> {
        let deadline = self.write_timeout.map(|t| Instant::now() + t);
        let (m, cv) = &*self.wstate;
        let mut st = m.lock().unwrap();
        loop {
            if let Some((k, msg)) = &st.failed {
                let e = io::Error::new(*k, msg.clone());
                drop(st);
                return Err(self.end_error_if_broken(e));
            }
            if st.pending == 0 {
                return Ok(());
            }
            match deadline {
                None => st = cv.wait(st).unwrap(),
                Some(d) => {
                    let left = d.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("{}: write timed out", self.label),
                        ));
                    }
                    st = cv.wait_timeout(st, left).unwrap().0;
                }
            }
        }
    }
}

impl PipeDuplex {
    fn end_error_if_broken(&self, e: io::Error) -> io::Error {
        if matches!(
            e.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
        ) {
            self.end_error(e)
        } else {
            e
        }
    }
}

impl Duplex for PipeDuplex {
    fn set_timeouts(&mut self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()> {
        if read.is_some() {
            self.read_timeout = read;
        }
        if write.is_some() {
            self.write_timeout = write;
        }
        Ok(())
    }

    fn shutdown(&mut self) -> io::Result<()> {
        self.close_once();
        Ok(())
    }

    fn peer(&self) -> String {
        self.label.clone()
    }
}

impl Drop for PipeDuplex {
    fn drop(&mut self) {
        self.close_once();
    }
}
