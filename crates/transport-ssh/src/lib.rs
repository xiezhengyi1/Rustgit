//! SSH transport: the application protocol over a framed byte stream carried by the system
//! `ssh` client, plus the server half (`rgit serve --stdio`).
//!
//! Client: [`SshTransport`] runs `ssh [options] -- host 'rgit serve --stdio -- /path'` through
//! `std::process::Command` with a structured argument list and implements the protocol's
//! [`Transport`](rustgit_protocol::Transport). Host-key verification is never disabled.
//! Server: [`serve::session_over`] and [`serve::OneShotListener`] serve exactly one session over
//! stdin/stdout. See docs/ssh.md.

pub mod pipe;
pub mod serve;
pub mod ssh;
pub mod target;

pub use pipe::{PipeDuplex, STDIO_MARKER};
pub use serve::{session_over, OneShotListener};
pub use ssh::{build_args, classify, remote_command, HostKeyPolicy, SshOptions, SshTransport};
pub use target::{Hop, SshTarget, TargetError};
