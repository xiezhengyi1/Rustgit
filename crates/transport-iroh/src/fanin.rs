//! Serve several listeners (e.g. loopback TCP and Iroh) with one host accept loop.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use rustgit_protocol::{Duplex, Listener};

pub struct FanInListener {
    rx: Receiver<Box<dyn Duplex>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    desc: String,
}

impl FanInListener {
    pub fn new(listeners: Vec<Box<dyn Listener>>) -> Self {
        let (tx, rx) = sync_channel::<Box<dyn Duplex>>(16);
        let stop = Arc::new(AtomicBool::new(false));
        let desc = listeners
            .iter()
            .map(|l| l.local_addr())
            .collect::<Vec<_>>()
            .join(", ");
        let threads = listeners
            .into_iter()
            .map(|l| {
                let tx = tx.clone();
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match l.accept(Duration::from_millis(200)) {
                            Ok(Some(d)) => {
                                // Blocks (back-pressure) when the host is busy; stop is
                                // re-checked via the disconnected channel.
                                if tx.send(d).is_err() {
                                    break;
                                }
                            }
                            Ok(None) => {}
                            Err(_) => std::thread::sleep(Duration::from_millis(200)),
                        }
                    }
                })
            })
            .collect();
        FanInListener {
            rx,
            stop,
            threads,
            desc,
        }
    }
}

impl Listener for FanInListener {
    fn accept(&self, wait: Duration) -> io::Result<Option<Box<dyn Duplex>>> {
        match self.rx.recv_timeout(wait) {
            Ok(d) => Ok(Some(d)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "all listeners ended",
            )),
        }
    }
    fn local_addr(&self) -> String {
        self.desc.clone()
    }
}

impl Drop for FanInListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
