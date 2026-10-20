//! Output mode: human text by default, one JSON object with `--json`
//! (see docs/gui-interface.md).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use rustgit_core::Error;
use serde_json::{json, Value};

static JSON: AtomicBool = AtomicBool::new(false);
static RESULT: Mutex<Option<String>> = Mutex::new(None);

pub const JSON_INTERFACE: u32 = 1;

pub fn set_json(on: bool) {
    JSON.store(on, Ordering::Relaxed);
}

pub fn is_json() -> bool {
    JSON.load(Ordering::Relaxed)
}

/// Human-readable line, suppressed in JSON mode so stdout stays machine-readable.
#[macro_export]
macro_rules! say {
    ($($t:tt)*) => { if !$crate::out::is_json() { println!($($t)*) } };
}

#[macro_export]
macro_rules! say_raw {
    ($($t:tt)*) => { if !$crate::out::is_json() { print!($($t)*) } };
}

/// Records the command's result (serialized immediately; nothing is built or stored in
/// human-output mode when used through `result!`).
pub fn store<T: serde::Serialize>(v: &T) {
    let text = serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string());
    *RESULT.lock().unwrap() = Some(text);
}

#[macro_export]
macro_rules! result {
    ($e:expr) => {
        if $crate::out::is_json() {
            $crate::out::store(&$e)
        }
    };
}

/// The stored result as JSON text (for the final `{"ok":true,"data":...}` line).
pub fn take_result_raw() -> String {
    let raw = RESULT.lock().unwrap().take().unwrap_or_else(|| "{}".to_string());
    // Commands that used an iroh remote report how they were connected (direct | relayed |
    // unreachable) next to their result.
    match crate::iroh::connection_json() {
        Some(conn) => match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Object(mut o)) => {
                o.insert("connection".into(), conn);
                Value::Object(o).to_string()
            }
            _ => raw,
        },
        None => raw,
    }
}

pub fn take_result() -> Value {
    serde_json::from_str(&take_result_raw()).unwrap_or_else(|_| json!({}))
}

/// Progress/event line (JSON mode only).
pub fn event(v: Value) {
    if is_json() {
        use std::io::Write;
        println!("{v}");
        let _ = std::io::stdout().flush();
    }
}

pub fn error_parts(e: &Error) -> (&'static str, Vec<String>) {
    match e {
        Error::NotARepository(_) => ("not_a_repository", vec![]),
        Error::DirtyWorkingTree(p) => ("dirty_working_tree", p.clone()),
        Error::NothingToCommit => ("nothing_to_commit", vec![]),
        Error::MergeInProgress => ("merge_in_progress", vec![]),
        Error::NoMerge => ("no_merge", vec![]),
        Error::UnresolvedConflicts(_) => ("unresolved_conflicts", vec![]),
        Error::BranchNotFound(_) => ("branch_not_found", vec![]),
        Error::BranchExists(_) => ("branch_exists", vec![]),
        Error::InvalidName(_) => ("invalid_name", vec![]),
        Error::Unportable(p) => ("unportable_paths", p.clone()),
        Error::RefConflict { .. } => ("ref_conflict", vec![]),
        Error::UnknownRevision(_) => ("unknown_revision", vec![]),
        Error::Invalid(_) => ("invalid", vec![]),
        Error::Other(m) => {
            if let Some((code, _)) = crate::auth::split_code(m) {
                return (code, vec![]);
            }
            let l = m.to_lowercase();
            if l.contains("ssh host key verification failed") {
                ("ssh_host_key_failed", vec![])
            } else if l.contains("ssh authentication failed") {
                ("ssh_auth_failed", vec![])
            } else if l.contains("ssh client not found") {
                ("ssh_unavailable", vec![])
            } else if l.contains("remote machine has no `rgit` command") {
                ("remote_rgit_missing", vec![])
            } else if l.contains("out of space") || l.contains("outofspace") {
                ("out_of_space", vec![])
            } else if l.contains("quota") {
                ("quota_exceeded", vec![])
            } else if l.contains("permission") || l.contains("not authorized") {
                ("permission_denied", vec![])
            } else if l.contains("connect") || l.contains("unreachable") || l.contains("refused") {
                ("host_unreachable", vec![])
            } else {
                ("other", vec![])
            }
        }
        _ => ("other", vec![]),
    }
}

pub fn error_json(e: &Error) -> Value {
    let (code, paths) = error_parts(e);
    let text = e.to_string();
    let message = match e {
        Error::Other(m) => crate::auth::split_code(m).map(|(_, rest)| rest.to_string()).unwrap_or(text),
        _ => text,
    };
    let mut v = json!({"code": code, "message": message});
    if !paths.is_empty() {
        v["paths"] = json!(paths);
    }
    if let Some(conn) = crate::iroh::connection_json() {
        v["connection"] = conn;
    }
    json!({"ok": false, "error": v})
}
