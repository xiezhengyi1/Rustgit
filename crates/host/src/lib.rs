//! Hosting: serves one bare project repository over the sync protocol with serialized,
//! durable writes. See docs/protocol.md.

pub mod auth;
pub mod authz;
pub mod config;
pub mod host;
pub mod hostid;
pub mod keystore;
pub mod server;
pub mod staging;

pub use auth::{AdmitRequest, Admission, Authorizer, DeviceAuthorizer, Op, OpenAccess};
pub use authz::{AuthStore, DeviceRecord, InvitationRecord, InvitationStatus};
pub use hostid::{ensure_host_identity, host_account, rotate_host_identity, HostKeyError, Rotation};
pub use keystore::{KeyError, KeyStorage};
pub use config::HostConfig;
pub use host::{maintenance_lock, Error, Host, Result, HOSTED_MARKER};
pub use server::{run, spawn, RunningHost};
