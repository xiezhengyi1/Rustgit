//! Crash-injection hooks for tests.
//!
//! `RGIT_FAILPOINT=name[:n][,name[:n]...]` makes the process exit abruptly (status 86, no
//! destructors, no flushing beyond what already reached the OS) the n-th time (default 1)
//! execution reaches the named point. Unset, every hook is a cheap no-op. This is a
//! process-crash simulation, not a power-loss simulation: bytes already handed to the OS
//! survive it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

struct Point {
    at: u64,
    seen: AtomicU64,
}

fn table() -> &'static HashMap<String, Point> {
    static T: OnceLock<HashMap<String, Point>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        if let Ok(v) = std::env::var("RGIT_FAILPOINT") {
            for part in v.split(',').filter(|p| !p.is_empty()) {
                let (name, n) = match part.split_once(':') {
                    Some((a, b)) => (a, b.parse().unwrap_or(1)),
                    None => (part, 1),
                };
                m.insert(
                    name.to_string(),
                    Point {
                        at: n,
                        seen: AtomicU64::new(0),
                    },
                );
            }
        }
        m
    })
}

pub const CRASH_STATUS: i32 = 86;

pub fn hit(name: &str) {
    if let Some(p) = table().get(name) {
        if p.seen.fetch_add(1, Ordering::SeqCst) + 1 == p.at {
            eprintln!("failpoint {name} reached: exiting");
            std::process::exit(CRASH_STATUS);
        }
    }
}
