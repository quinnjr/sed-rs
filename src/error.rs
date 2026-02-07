use std::path::PathBuf;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("invalid regex: {0}")]
    Regex(#[from] regex::Error),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("failed to persist file: {0}")]
    TempfilePersist(#[from] tempfile::PersistError),

    #[error("invalid path: {0}")]
    InvalidPath(PathBuf),

    #[error("parse error: {0}")]
    Parse(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
