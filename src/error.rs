use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveVfsError {
    #[error("archive is corrupt: {0}")]
    Corrupt(String),
    #[error("invalid configuration: {0}")]
    ConfigValue(String),
    #[error("encrypted archive member is not supported: {0}")]
    Encrypted(String),
    #[error("invalid configuration in {path}: {source}")]
    InvalidConfig {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid virtual path: {0}")]
    InvalidVirtualPath(String),
    #[error("I/O error while accessing {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("archive member not found: {0}")]
    MemberNotFound(String),
    #[error("archive member exceeds configured compression ratio: {actual:.2} > {maximum:.2}")]
    RatioLimit { actual: f64, maximum: f64 },
    #[error("archive member is {actual} bytes, above the configured limit of {maximum} bytes")]
    SizeLimit { actual: u64, maximum: u64 },
    #[error("index database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("unsupported archive format")]
    UnsupportedArchive,
    #[error("unsupported ZIP compression method {method} for {member}")]
    UnsupportedCompression { method: u16, member: String },
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl ArchiveVfsError {
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, ArchiveVfsError>;
