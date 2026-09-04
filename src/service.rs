use std::{
    fs::File,
    path::{Path, PathBuf},
};

use crate::{
    Result,
    archive::{ArchiveBackend, ArchiveRegistry, IndexedMember},
    cache::{MemberCache, MemberLease},
    config::Config,
    identity::ArchiveIdentity,
    index::{Index, IndexStore, Node},
};

pub struct ArchiveService {
    config: Config,
    registry: ArchiveRegistry,
}

pub struct ArchiveView<'service> {
    pub identity: ArchiveIdentity,
    pub backend: &'service dyn ArchiveBackend,
    pub index: Index,
}

impl ArchiveService {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            registry: ArchiveRegistry::with_defaults(),
        }
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn recognizes(&self, archive_path: &Path) -> Result<bool> {
        if !self.config.recognizes(archive_path) {
            return Ok(false);
        }
        let mut archive = File::open(archive_path)
            .map_err(|source| crate::ArchiveVfsError::io(archive_path, source))?;
        Ok(self.registry.detect(archive_path, &mut archive).is_ok())
    }

    pub fn open(&self, archive_path: &Path) -> Result<ArchiveView<'_>> {
        let identity = ArchiveIdentity::from_path(archive_path)?;
        let mut archive = File::open(&identity.canonical_path)
            .map_err(|source| crate::ArchiveVfsError::io(&identity.canonical_path, source))?;
        let backend = self
            .registry
            .detect(&identity.canonical_path, &mut archive)?;
        let index = IndexStore::new(self.config.index_dir.clone()).load(
            &identity,
            backend,
            self.config.filename_policy,
            self.config.persist_indexes,
        )?;
        Ok(ArchiveView {
            identity,
            backend,
            index,
        })
    }

    pub fn read_dir(
        &self,
        archive_path: &Path,
        inner_path: &Path,
    ) -> Result<(ArchiveIdentity, Vec<Node>)> {
        let view = self.open(archive_path)?;
        let nodes = view.index.read_dir(&virtual_components(inner_path))?;
        Ok((view.identity, nodes))
    }

    pub fn stat(&self, archive_path: &Path, inner_path: &Path) -> Result<(ArchiveIdentity, Node)> {
        let view = self.open(archive_path)?;
        let node = view.index.node(&virtual_components(inner_path))?;
        Ok((view.identity, node))
    }

    pub fn lease(&self, archive_path: &Path, inner_path: &Path) -> Result<MemberLease> {
        let view = self.open(archive_path)?;
        let member = view.index.member(&virtual_components(inner_path))?;
        MemberCache::from_config(&self.config).lease(&view.identity, view.backend, &member)
    }

    pub fn member(
        &self,
        archive_path: &Path,
        inner_path: &Path,
    ) -> Result<(ArchiveView<'_>, IndexedMember)> {
        let view = self.open(archive_path)?;
        let member = view.index.member(&virtual_components(inner_path))?;
        Ok((view, member))
    }

    pub fn prune_cache(&self) -> Result<u64> {
        MemberCache::from_config(&self.config).prune()
    }

    pub fn clean_partials(&self) -> Result<usize> {
        MemberCache::from_config(&self.config).clean_partials()
    }
}

#[cfg(unix)]
fn virtual_components(path: &Path) -> Vec<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;

    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.as_bytes().to_vec()),
            _ => None,
        })
        .collect()
}

#[cfg(not(unix))]
fn virtual_components(path: &Path) -> Vec<Vec<u8>> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => {
                Some(value.to_string_lossy().as_bytes().to_vec())
            }
            _ => None,
        })
        .collect()
}

#[must_use]
pub fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        PathBuf::from(OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
    }
}
