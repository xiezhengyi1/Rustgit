//! What the UI is told about the connection: `direct`, `relayed` or `unreachable`.
//!
//! The state comes from Iroh's own path bookkeeping (`Connection::paths()`: the path QUIC is
//! currently sending application data on is either an IP path or a relay path), not from a
//! guess. A connection normally starts on the relay while hole punching is attempted and may
//! move to a direct path later; `used_relay` / `switches` record that history so a transfer
//! that began relayed and ended direct (or the other way round) is not misreported.

use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathState {
    /// No connection attempt yet, or one is in progress.
    Connecting,
    /// Application data flows over a direct UDP path between the two devices.
    Direct,
    /// Application data flows through the relay (still end-to-end encrypted).
    Relayed,
    /// The host could not be reached (or the connection was lost and has not come back).
    Unreachable,
}

impl PathState {
    /// The string used in `rgit --json` and the UI.
    pub fn name(self) -> &'static str {
        match self {
            PathState::Connecting => "connecting",
            PathState::Direct => "direct",
            PathState::Relayed => "relayed",
            PathState::Unreachable => "unreachable",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Status {
    pub state: PathState,
    /// The remote endpoint id (hex).
    pub remote: String,
    /// Our own endpoint id (hex), once the endpoint exists. Relay operators put this into
    /// `access.allowlist` for clients that use a persistent key.
    pub local: Option<String>,
    /// The selected path, e.g. `127.0.0.1:41234` or `relay https://relay.example.org/`.
    pub selected: Option<String>,
    /// The connection used a relay path at some time.
    pub used_relay: bool,
    /// The connection used a direct path at some time.
    pub used_direct: bool,
    /// How many times the selected path changed (direct <-> relay or address change).
    pub switches: u32,
    /// Why the host is unreachable, or why the last connection ended.
    pub error: Option<String>,
}

impl Status {
    fn new(remote: String) -> Self {
        Status {
            state: PathState::Connecting,
            remote,
            local: None,
            selected: None,
            used_relay: false,
            used_direct: false,
            switches: 0,
            error: None,
        }
    }
}

type Observer = Arc<dyn Fn(&Status) + Send + Sync>;

struct Inner {
    observer: Option<Observer>,
    status: Status,
    /// Bumped by every connection attempt so a stale connection cannot overwrite a newer one.
    generation: u64,
    /// Set when this side closed the connection on purpose.
    closing: bool,
}

/// Shared between a transport, its connections and their watcher tasks.
#[derive(Clone)]
pub struct StatusHandle(Arc<Mutex<Inner>>);

impl StatusHandle {
    pub fn new(remote: String) -> Self {
        StatusHandle(Arc::new(Mutex::new(Inner {
            observer: None,
            status: Status::new(remote),
            generation: 0,
            closing: false,
        })))
    }

    pub fn snapshot(&self) -> Status {
        self.0.lock().unwrap().status.clone()
    }

    /// Calls `f` (from an Iroh worker thread, outside any lock) whenever the state or the
    /// selected path changes. Replaces a previous observer.
    pub fn observe(&self, f: impl Fn(&Status) + Send + Sync + 'static) {
        self.0.lock().unwrap().observer = Some(Arc::new(f));
    }

    /// Applies `f` under the lock and notifies the observer if state or path changed.
    fn update(&self, f: impl FnOnce(&mut Inner)) {
        let (changed, obs, snap) = {
            let mut g = self.0.lock().unwrap();
            let before = (g.status.state, g.status.selected.clone());
            f(&mut g);
            let after = (g.status.state, g.status.selected.clone());
            (before != after, g.observer.clone(), g.status.clone())
        };
        if changed {
            if let Some(o) = obs {
                o(&snap);
            }
        }
    }

    pub(crate) fn set_local(&self, id: String) {
        self.0.lock().unwrap().status.local = Some(id);
    }

    pub(crate) fn begin(&self) -> u64 {
        let mut gen = 0;
        self.update(|g| {
            g.generation += 1;
            g.closing = false;
            // History flags describe one connection; a retry starts from a clean slate.
            // `switches` is kept so that a flapping network stays visible across reconnects.
            g.status.state = PathState::Connecting;
            g.status.selected = None;
            g.status.used_relay = false;
            g.status.used_direct = false;
            g.status.error = None;
            gen = g.generation;
        });
        gen
    }

    pub(crate) fn failed(&self, gen: u64, why: String) {
        self.update(|g| {
            if g.generation == gen {
                g.status.state = PathState::Unreachable;
                g.status.error = Some(why);
            }
        });
    }

    pub(crate) fn path(&self, gen: u64, selected: Option<(bool, String)>) {
        self.update(|g| {
            if g.generation != gen {
                return;
            }
            let Some((relay, desc)) = selected else {
                return;
            };
            let s = &mut g.status;
            if s.selected.as_deref() != Some(desc.as_str()) {
                if s.selected.is_some() {
                    s.switches += 1;
                }
                s.selected = Some(desc);
            }
            if relay {
                s.used_relay = true;
                s.state = PathState::Relayed;
            } else {
                s.used_direct = true;
                s.state = PathState::Direct;
            }
            s.error = None;
        });
    }

    pub(crate) fn closing(&self, gen: u64) {
        let mut g = self.0.lock().unwrap();
        if g.generation == gen {
            g.closing = true;
        }
    }

    /// The connection ended; if that was not our doing the host is now unreachable.
    pub(crate) fn ended(&self, gen: u64, why: String) {
        self.update(|g| {
            if g.generation == gen && !g.closing {
                g.status.state = PathState::Unreachable;
                g.status.error = Some(why);
            }
        });
    }
}
