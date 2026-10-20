//! Versioned synchronization protocol: messages, framing, transports and the client state
//! machine. The host side lives in `rustgit-host`. See docs/protocol.md.

pub mod client;
pub mod error;
pub mod failpoint;
pub mod frame;
pub mod identity;
pub mod invite;
pub mod msg;
pub mod plan;
pub mod secure;
pub mod transport;

pub use client::{
    clone_into, session_id, with_retries, CancelToken, Client, ClientConfig, FetchOutcome,
    PushOptions, PushOutcome, Security, TransferStats, PROTOCOL_MAX_VERSION, PROTOCOL_MIN_VERSION,
};
pub use error::{Detail, Error, ErrorCode, Result, WireError};
pub use frame::Channel;
pub use identity::{
    DeviceKey, EnrollmentSecret, Fingerprint, PublicKey, Role, Succession,
};
pub use invite::{Invitation, InviteParseError};
pub use msg::*;
pub use transport::{Duplex, Listener, TcpListenerAdapter, TcpTransport, Transport};
