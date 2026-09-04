use std::{
    env, fs,
    path::{Path, PathBuf},
};

use directories::BaseDirs;
use serde::Deserialize;

use crate::{ArchiveVfsError, Result};

pub const CONFIG_ENV: &str = "ARCHIVE_VFS_CONFIG";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum FilenamePolicy {
    #[default]
    Standard,
    Raw,
    LossyUtf8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub cache_dir: PathBuf,
    pub max_cache_bytes: u64,
    pub max_concurrent_extractions: usize,
    pub max_member_bytes: u64,
    pub max_compression_ratio: f64,
    pub index_dir: PathBuf,
    pub persist_indexes: bool,
    pub filename_policy: FilenamePolicy,
    pub log_level: LogLevel,
    pub archive_extensions: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        let (cache_dir, index_dir) = default_cache_dirs();
        Self {
            cache_dir,
            max_cache_bytes: 2 * 1024 * 1024 * 1024,
            max_concurrent_extractions: 2,
            max_member_bytes: 8 * 1024 * 1024 * 1024,
            max_compression_ratio: 1_000.0,
            index_dir,
            persist_indexes: true,
            filename_policy: FilenamePolicy::Standard,
            log_level: LogLevel::Info,
            archive_extensions: vec!["zip".to_owned(), "zipx".to_owned()],
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Self::default().validated();
        }
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|source| ArchiveVfsError::io(path, source))?;
        let config: Self =
            toml::from_str(&text).map_err(|source| ArchiveVfsError::InvalidConfig {
                path: path.to_owned(),
                source,
            })?;
        config.validated()
    }

    #[must_use]
    pub fn recognizes(&self, path: &Path) -> bool {
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            return false;
        };
        self.archive_extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(extension))
    }

    fn validated(self) -> Result<Self> {
        if self.max_concurrent_extractions == 0 {
            return Err(ArchiveVfsError::ConfigValue(
                "max_concurrent_extractions must be at least 1".to_owned(),
            ));
        }
        if !self.max_compression_ratio.is_finite() || self.max_compression_ratio < 1.0 {
            return Err(ArchiveVfsError::ConfigValue(
                "max_compression_ratio must be finite and at least 1".to_owned(),
            ));
        }
        if self.archive_extensions.is_empty()
            || self
                .archive_extensions
                .iter()
                .any(|extension| extension.is_empty() || extension.contains(['/', '\\']))
        {
            return Err(ArchiveVfsError::ConfigValue(
                "archive_extensions must contain non-empty filename extensions".to_owned(),
            ));
        }
        Ok(self)
    }
}

fn config_path() -> PathBuf {
    if let Some(path) = env::var_os(CONFIG_ENV) {
        return PathBuf::from(path);
    }
    BaseDirs::new().map_or_else(
        || PathBuf::from("archive-vfs.toml"),
        |dirs| dirs.config_dir().join("yazi/archive-vfs.toml"),
    )
}

fn default_cache_dirs() -> (PathBuf, PathBuf) {
    BaseDirs::new().map_or_else(
        || {
            (
                PathBuf::from(".cache/archive-vfs/members"),
                PathBuf::from(".cache/archive-vfs/indexes"),
            )
        },
        |dirs| {
            let root = dirs.cache_dir().join("archive-vfs");
            (root.join("members"), root.join("indexes"))
        },
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::Config;

    #[test]
    fn extension_matching_is_case_insensitive() {
        let config = Config::default();
        assert!(config.recognizes(Path::new("dataset.ZIP")));
        assert!(!config.recognizes(Path::new("dataset.tar")));
    }
}
