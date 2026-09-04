mod path;
mod zip;

use std::{fs::File, io::Write, path::Path, time::SystemTime};

pub use path::{NormalizedPath, normalize_member_path};
pub use zip::ZipBackend;

use crate::{Result, config::FilenamePolicy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveEntry {
    pub entry_id: u64,
    pub path: NormalizedPath,
    pub kind: EntryKind,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub crc32: u32,
    pub method: u16,
    pub flags: u16,
    pub local_header_offset: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedMember {
    pub entry_id: u64,
    pub display_path: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub crc32: u32,
    pub method: u16,
    pub flags: u16,
    pub local_header_offset: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ExtractionLimits {
    pub max_member_bytes: u64,
    pub max_compression_ratio: f64,
}

pub trait EntrySink {
    fn accept(&mut self, entry: ArchiveEntry) -> Result<()>;
}

pub trait ArchiveBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn probe(&self, archive: &mut File) -> Result<bool>;
    fn index(
        &self,
        archive: &mut File,
        filename_policy: FilenamePolicy,
        sink: &mut dyn EntrySink,
    ) -> Result<()>;
    fn extract(
        &self,
        archive: &mut File,
        member: &IndexedMember,
        limits: ExtractionLimits,
        output: &mut dyn Write,
    ) -> Result<u64>;
}

#[derive(Default)]
pub struct ArchiveRegistry {
    backends: Vec<Box<dyn ArchiveBackend>>,
}

impl ArchiveRegistry {
    #[must_use]
    pub fn with_defaults() -> Self {
        Self {
            backends: vec![Box::new(ZipBackend)],
        }
    }

    pub fn detect<'a>(
        &'a self,
        _archive_path: &Path,
        archive: &mut File,
    ) -> Result<&'a dyn ArchiveBackend> {
        for backend in &self.backends {
            if backend.probe(archive)? {
                return Ok(backend.as_ref());
            }
        }
        Err(crate::ArchiveVfsError::UnsupportedArchive)
    }
}
