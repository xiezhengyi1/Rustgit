use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Store(#[from] rustgit_store::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("metadata database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("not a rustgit project (no .rgit found from {0})")]
    NotARepository(PathBuf),
    #[error("a project already exists at {0}")]
    AlreadyExists(PathBuf),
    #[error("uncommitted changes would be overwritten: {}", .0.join(", "))]
    DirtyWorkingTree(Vec<String>),
    #[error("nothing to commit")]
    NothingToCommit,
    #[error("a merge is in progress; resolve conflicts and commit, or run `merge --abort`")]
    MergeInProgress,
    #[error("no merge is in progress")]
    NoMerge,
    #[error("{0} conflict(s) remain unresolved")]
    UnresolvedConflicts(usize),
    #[error("branch '{0}' not found")]
    BranchNotFound(String),
    #[error("branch '{0}' already exists")]
    BranchExists(String),
    #[error("invalid name '{0}'")]
    InvalidName(String),
    #[error("cannot delete the current branch '{0}'")]
    DeleteCurrent(String),
    #[error("reference '{name}' changed concurrently (fetch and merge required)")]
    RefConflict { name: String },
    #[error("checkout would create paths that are not portable: {}", .0.join("; "))]
    Unportable(Vec<String>),
    #[error("'{0}' changed while it was being read; try again")]
    FileChanged(String),
    #[error("unknown revision '{0}'")]
    UnknownRevision(String),
    #[error("invalid object: {0}")]
    Invalid(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
