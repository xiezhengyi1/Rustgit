//! The server side of the SSH transport: one protocol session over this process's stdin and
//! stdout (`rgit serve --stdio`), usable as an sshd forced command or subsystem.
//!
//! stdout carries protocol frames only; anything a program prints for humans must go to stderr.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustgit_protocol::{Duplex, Listener};

use crate::pipe::{PipeDuplex, STDIO_MARKER};

/// Announces the server on `writer` (a line the client scans for, so noise printed before it
/// by shell start-up files is ignored) and wraps the pair as a [`PipeDuplex`].
pub fn session_over(
    reader: impl Read + Send + 'static,
    mut writer: impl Write + Send + 'static,
    label: &str,
) -> io::Result<PipeDuplex> {
    writer.write_all(b"\n")?;
    writer.write_all(STDIO_MARKER)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(PipeDuplex::new(reader, writer, label))
}

/// A duplex that records when it is dropped.
struct Tracked {
    inner: Box<dyn Duplex>,
    finished: Arc<AtomicBool>,
}

impl Read for Tracked {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.inner.read(b)
    }
}
impl Write for Tracked {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.inner.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
impl Duplex for Tracked {
    fn set_timeouts(&mut self, r: Option<Duration>, w: Option<Duration>) -> io::Result<()> {
        self.inner.set_timeouts(r, w)
    }
    fn shutdown(&mut self) -> io::Result<()> {
        self.inner.shutdown()
    }
    fn peer(&self) -> String {
        self.inner.peer()
    }
}
impl Drop for Tracked {
    fn drop(&mut self) {
        let _ = self.inner.shutdown();
        self.finished.store(true, Ordering::SeqCst);
    }
}

/// A [`Listener`] that yields exactly one connection and then nothing.
pub struct OneShotListener {
    conn: Mutex<Option<Box<dyn Duplex>>>,
    label: String,
}

impl OneShotListener {
    /// Returns the listener and a flag that becomes true once the connection has ended.
    pub fn new(conn: Box<dyn Duplex>, label: &str) -> (Self, Arc<AtomicBool>) {
        let finished = Arc::new(AtomicBool::new(false));
        let tracked = Tracked {
            inner: conn,
            finished: finished.clone(),
        };
        (
            OneShotListener {
                conn: Mutex::new(Some(Box::new(tracked))),
                label: label.to_string(),
            },
            finished,
        )
    }
}

impl Listener for OneShotListener {
    fn accept(&self, wait: Duration) -> io::Result<Option<Box<dyn Duplex>>> {
        if let Some(c) = self.conn.lock().unwrap().take() {
            return Ok(Some(c));
        }
        std::thread::sleep(wait.min(Duration::from_millis(50)));
        Ok(None)
    }
    fn local_addr(&self) -> String {
        self.label.clone()
    }
}
