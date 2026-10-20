//! Iroh transport for the rustgit sync protocol.
//!
//! Carries the unchanged application protocol (`rustgit-protocol` frames) over one QUIC
//! bidirectional stream per protocol connection. Iroh provides:
//!
//! * identity by public key (the *endpoint id*), which survives IP address changes;
//! * TLS 1.3 inside QUIC, keyed by those identities, so a relay only forwards ciphertext;
//! * hole punching with a relay fallback, and path migration while a connection is open.
//!
//! See `docs/decisions.md` (D20) for the evaluation and `docs/relay.md` for running a relay.

mod config;
mod duplex;
mod endpoint;
mod fanin;
mod listener;
mod status;
mod transport;
mod url;

pub use config::{
    endpoint_id_of, load_ca_pem, load_or_create_secret_key, IrohConfig, RelaySetting,
};
pub use duplex::IrohDuplex;
pub use endpoint::ALPN;
pub use fanin::FanInListener;
pub use listener::IrohListener;
pub use status::{PathState, Status, StatusHandle};
pub use transport::IrohTransport;
pub use url::{IrohUrl, UrlError, SCHEME};

/// Version of the Iroh crates this transport was built and tested against.
pub const IROH_VERSION: &str = "1.3.0";
