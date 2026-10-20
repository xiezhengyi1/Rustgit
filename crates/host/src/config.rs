//! Host limits and policy.

use std::time::Duration;

use rustgit_protocol::Limits;

#[derive(Clone, Debug)]
pub struct HostConfig {
    pub limits: Limits,
    /// Allow non-fast-forward ref updates when a client asks (`force`). Off by default.
    pub allow_force: bool,
    /// Allow clients to delete branches. Off by default.
    pub allow_delete: bool,
    /// Longest a connection may stay silent between requests.
    pub idle_timeout: Duration,
    /// Longest a peer may stall in the middle of a frame.
    pub io_timeout: Duration,
    pub max_connections: usize,
    /// Connections allowed to sit in the handshake / `Hello` stage at once.
    pub max_unauthenticated: usize,
    pub max_connections_per_device: usize,
    pub max_open_sessions_per_device: usize,
    /// Staged (not yet published) bytes one upload session may hold.
    pub max_session_bytes: u64,
    /// Staged bytes across all sessions.
    pub max_staging_bytes: u64,
    /// Refuse new uploads when the repository directory already exceeds this.
    pub max_repo_bytes: Option<u64>,
    /// Objects one fetch plan may enumerate (the id list is held in memory).
    pub max_plan_objects: u64,
    /// Abandoned upload sessions older than this are deleted.
    pub staging_ttl: Duration,
    pub max_watch: Duration,
    pub log: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            limits: Limits::default(),
            allow_force: false,
            allow_delete: false,
            idle_timeout: Duration::from_secs(300),
            io_timeout: Duration::from_secs(30),
            max_connections: 64,
            max_unauthenticated: 32,
            max_connections_per_device: 8,
            max_open_sessions_per_device: 4,
            max_session_bytes: 8 << 30,
            max_staging_bytes: 32 << 30,
            max_repo_bytes: None,
            max_plan_objects: 4_000_000,
            staging_ttl: Duration::from_secs(24 * 3600),
            max_watch: Duration::from_secs(60),
            log: false,
        }
    }
}
