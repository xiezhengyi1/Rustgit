use crate::id::ObjectId;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("index database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("object {0} not found")]
    NotFound(ObjectId),
    #[error("corrupt data: {0}")]
    Corrupt(String),
    #[error("malformed object: {0}")]
    Malformed(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("limit exceeded: {0}")]
    Limit(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn malformed(msg: impl Into<String>) -> Self {
        Error::Malformed(msg.into())
    }
    pub fn corrupt(msg: impl Into<String>) -> Self {
        Error::Corrupt(msg.into())
    }
}
