use std::{
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};

use crate::{ArchiveVfsError, Result};

const TAIL_FINGERPRINT_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveIdentity {
    pub canonical_path: PathBuf,
    pub size: u64,
    pub modified_ns: u128,
    pub device: u64,
    pub inode: u64,
    pub changed_ns: i128,
    pub tail_digest: blake3::Hash,
}

impl ArchiveIdentity {
    pub fn from_path(path: &Path) -> Result<Self> {
        let canonical_path = fs::canonicalize(path)
            .map_err(|source| ArchiveVfsError::io(path.to_owned(), source))?;
        let metadata = fs::metadata(&canonical_path)
            .map_err(|source| ArchiveVfsError::io(canonical_path.clone(), source))?;
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |value| value.as_nanos());
        let tail_digest = tail_digest(&canonical_path, metadata.len())?;
        let (device, inode, changed_ns) = platform_identity(&metadata);
        Ok(Self {
            canonical_path,
            size: metadata.len(),
            modified_ns,
            device,
            inode,
            changed_ns,
            tail_digest,
        })
    }

    #[must_use]
    pub fn key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        update_path(&mut hasher, &self.canonical_path);
        hasher.update(&self.size.to_le_bytes());
        hasher.update(&self.modified_ns.to_le_bytes());
        hasher.update(&self.device.to_le_bytes());
        hasher.update(&self.inode.to_le_bytes());
        hasher.update(&self.changed_ns.to_le_bytes());
        hasher.update(self.tail_digest.as_bytes());
        hasher.finalize().to_hex().to_string()
    }

    #[must_use]
    pub fn tag(&self) -> u64 {
        let digest = self.key();
        let mut bytes = [0_u8; 8];
        for (index, pair) in digest.as_bytes()[..16].chunks_exact(2).enumerate() {
            bytes[index] = hex_byte(pair);
        }
        u64::from_le_bytes(bytes) & (u64::MAX >> 1)
    }
}

fn hex_byte(pair: &[u8]) -> u8 {
    (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1])
}

const fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

fn tail_digest(path: &Path, size: u64) -> Result<blake3::Hash> {
    let mut file = File::open(path).map_err(|source| ArchiveVfsError::io(path, source))?;
    let start = size.saturating_sub(TAIL_FINGERPRINT_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|source| ArchiveVfsError::io(path, source))?;
    let mut reader = file.take(TAIL_FINGERPRINT_BYTES);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| ArchiveVfsError::io(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

#[cfg(unix)]
fn platform_identity(metadata: &Metadata) -> (u64, u64, i128) {
    let nanos = i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec());
    (metadata.dev(), metadata.ino(), nanos)
}

#[cfg(not(unix))]
fn platform_identity(_metadata: &Metadata) -> (u64, u64, i128) {
    (0, 0, 0)
}

#[cfg(unix)]
fn update_path(hasher: &mut blake3::Hasher, path: &Path) {
    hasher.update(path.as_os_str().as_bytes());
}

#[cfg(not(unix))]
fn update_path(hasher: &mut blake3::Hasher, path: &Path) {
    hasher.update(path.to_string_lossy().as_bytes());
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use tempfile::NamedTempFile;

    use super::ArchiveIdentity;

    #[test]
    fn identity_changes_when_archive_tail_changes() {
        let mut archive = NamedTempFile::new().expect("create fixture");
        archive.write_all(b"first").expect("write fixture");
        archive.flush().expect("flush fixture");
        let first = ArchiveIdentity::from_path(archive.path()).expect("first identity");

        fs::write(archive.path(), b"other").expect("replace fixture");
        let second = ArchiveIdentity::from_path(archive.path()).expect("second identity");
        assert_ne!(first.key(), second.key());
    }
}
