//! Version-control engine: logical objects, working tree, commits, branches, merge.

pub mod diff;
pub mod error;
pub mod maintenance;
pub mod merge;
pub mod objects;
pub mod ops;
pub mod repo;
pub mod worktree;

pub use error::{Error, Result};
pub use repo::{Conflict, Flat, FlatEntry, MergeState, Repo};
pub use rustgit_store::{ObjectId, ObjectType};
