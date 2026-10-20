//! Content-addressed chunk/object store: chunking, packs, delta encoding, index.

pub mod chunker;
pub mod codec;
pub mod error;
pub mod id;
pub mod pack;
mod store;

pub use error::{Error, Result};
pub use id::{hash_object, ObjectId, ObjectType};
pub use store::{ReindexReport, RepackOptions, RepackStats, Store, StoreOptions, StoreStats};
