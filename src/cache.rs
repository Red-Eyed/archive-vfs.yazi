use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    ArchiveVfsError, Result,
    archive::{ArchiveBackend, ExtractionLimits, IndexedMember},
    config::Config,
    identity::ArchiveIdentity,
};

const CACHE_SCHEMA: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    CREATE TABLE IF NOT EXISTS entries (
        key TEXT PRIMARY KEY,
        relative_path TEXT NOT NULL,
        size INTEGER NOT NULL,
        accessed_ns INTEGER NOT NULL
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS entries_lru ON entries(accessed_ns);
";

#[derive(Clone)]
pub struct MemberCache {
    root: PathBuf,
    max_bytes: u64,
    max_concurrent_extractions: usize,
    limits: ExtractionLimits,
}

pub struct MemberLease {
    path: PathBuf,
    lock: File,
    cache: MemberCache,
}

impl MemberLease {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open(&self) -> Result<File> {
        File::open(&self.path).map_err(|source| ArchiveVfsError::io(&self.path, source))
    }
}

impl Drop for MemberLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
        if let Err(error) = self.cache.prune() {
            tracing::warn!(%error, "failed to prune archive member cache after lease release");
        }
    }
}

impl MemberCache {
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self {
            root: config.cache_dir.clone(),
            max_bytes: config.max_cache_bytes,
            max_concurrent_extractions: config.max_concurrent_extractions.max(1),
            limits: ExtractionLimits {
                max_member_bytes: config.max_member_bytes,
                max_compression_ratio: config.max_compression_ratio,
            },
        }
    }

    pub fn lease(
        &self,
        identity: &ArchiveIdentity,
        backend: &dyn ArchiveBackend,
        member: &IndexedMember,
    ) -> Result<MemberLease> {
        self.ensure_layout()?;
        let key = member_key(identity, member.entry_id);
        let path = self.content_path(&key);
        let lock_path = self.lock_path(&key);
        let lock = open_lock(&lock_path)?;
        FileExt::lock_exclusive(&lock).map_err(|source| ArchiveVfsError::io(&lock_path, source))?;

        if !is_complete(&path, member.uncompressed_size) {
            let _slot = self.acquire_extraction_slot()?;
            self.extract(identity, backend, member, &path)?;
        }
        self.touch(&key, &path, member.uncompressed_size)?;
        self.prune_excluding(Some(&key))?;

        // flock(2) converts this exclusive lock to shared atomically on Linux.
        // The shared lease prevents eviction while Yazi reads the backing path.
        FileExt::lock_shared(&lock).map_err(|source| ArchiveVfsError::io(&lock_path, source))?;
        Ok(MemberLease {
            path,
            lock,
            cache: self.clone(),
        })
    }

    pub fn prune(&self) -> Result<u64> {
        self.prune_excluding(None)
    }

    fn prune_excluding(&self, protected_key: Option<&str>) -> Result<u64> {
        self.ensure_layout()?;
        let mut database = self.database()?;
        let total = total_size(&database)?;
        if total <= self.max_bytes {
            return Ok(0);
        }

        let candidates = {
            let mut statement = database
                .prepare("SELECT key, relative_path, size FROM entries ORDER BY accessed_ns ASC")?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        let transaction = database.transaction()?;
        let mut remaining = total;
        let mut removed = 0_u64;
        for (key, relative_path, size) in candidates {
            if remaining <= self.max_bytes {
                break;
            }
            if protected_key == Some(key.as_str()) {
                continue;
            }
            let lock_path = self.lock_path(&key);
            let lock = open_lock(&lock_path)?;
            match FileExt::try_lock_exclusive(&lock) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(source) => return Err(ArchiveVfsError::io(lock_path, source)),
            }
            let path = self.root.join(relative_path);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(ArchiveVfsError::io(&path, source)),
            }
            transaction.execute("DELETE FROM entries WHERE key = ?1", [&key])?;
            let size = u64::try_from(size).unwrap_or(0);
            remaining = remaining.saturating_sub(size);
            removed = removed.saturating_add(size);
        }
        transaction.commit()?;
        Ok(removed)
    }

    pub fn clean_partials(&self) -> Result<usize> {
        self.ensure_layout()?;
        let data = self.root.join("data");
        let mut removed = 0;
        for prefix in read_dir(&data)? {
            let prefix = prefix.map_err(|source| ArchiveVfsError::io(&data, source))?;
            if !prefix
                .file_type()
                .map_err(|source| ArchiveVfsError::io(prefix.path(), source))?
                .is_dir()
            {
                continue;
            }
            for entry in read_dir(&prefix.path())? {
                let entry = entry.map_err(|source| ArchiveVfsError::io(prefix.path(), source))?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Some((key, _)) = name.split_once(".part-") else {
                    continue;
                };
                let lock_path = self.lock_path(key);
                let lock = open_lock(&lock_path)?;
                match FileExt::try_lock_exclusive(&lock) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(source) => return Err(ArchiveVfsError::io(lock_path, source)),
                }
                if entry.path().is_file() {
                    fs::remove_file(entry.path())
                        .map_err(|source| ArchiveVfsError::io(entry.path(), source))?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    fn extract(
        &self,
        identity: &ArchiveIdentity,
        backend: &dyn ArchiveBackend,
        member: &IndexedMember,
        destination: &Path,
    ) -> Result<()> {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| ArchiveVfsError::io(parent, source))?;
        }
        Self::remove_member_partials(destination)?;
        let temporary = destination.with_extension(format!("part-{}", std::process::id()));
        let result = (|| {
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|source| ArchiveVfsError::io(&temporary, source))?;
            let mut archive = File::open(&identity.canonical_path)
                .map_err(|source| ArchiveVfsError::io(&identity.canonical_path, source))?;
            backend.extract(&mut archive, member, self.limits, &mut output)?;
            output
                .sync_all()
                .map_err(|source| ArchiveVfsError::io(&temporary, source))?;
            fs::rename(&temporary, destination)
                .map_err(|source| ArchiveVfsError::io(destination, source))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn remove_member_partials(destination: &Path) -> Result<()> {
        let Some(parent) = destination.parent() else {
            return Ok(());
        };
        let prefix = format!(
            "{}.",
            destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );
        for entry in read_dir(parent)? {
            let entry = entry.map_err(|source| ArchiveVfsError::io(parent, source))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && name.contains(".part-") {
                fs::remove_file(entry.path())
                    .map_err(|source| ArchiveVfsError::io(entry.path(), source))?;
            }
        }
        Ok(())
    }

    fn acquire_extraction_slot(&self) -> Result<ExtractionSlot> {
        let slots = self.root.join("slots");
        fs::create_dir_all(&slots).map_err(|source| ArchiveVfsError::io(&slots, source))?;
        loop {
            for index in 0..self.max_concurrent_extractions {
                let path = slots.join(format!("{index}.lock"));
                let lock = open_lock(&path)?;
                match FileExt::try_lock_exclusive(&lock) {
                    Ok(()) => return Ok(ExtractionSlot { lock }),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(source) => return Err(ArchiveVfsError::io(path, source)),
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn ensure_layout(&self) -> Result<()> {
        for directory in [
            self.root.clone(),
            self.root.join("data"),
            self.root.join("locks"),
        ] {
            fs::create_dir_all(&directory)
                .map_err(|source| ArchiveVfsError::io(directory, source))?;
        }
        let schema_lock_path = self.root.join("cache-schema.lock");
        let schema_lock = open_lock(&schema_lock_path)?;
        FileExt::lock_exclusive(&schema_lock)
            .map_err(|source| ArchiveVfsError::io(&schema_lock_path, source))?;
        let database = self.database()?;
        database.execute_batch(CACHE_SCHEMA)?;
        Ok(())
    }

    fn database(&self) -> Result<Connection> {
        let connection = Connection::open(self.root.join("cache.sqlite"))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    fn touch(&self, key: &str, path: &Path, size: u64) -> Result<()> {
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| ArchiveVfsError::Corrupt("cache entry escaped cache root".to_owned()))?;
        let accessed_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let accessed_ns = i64::try_from(accessed_ns).unwrap_or(i64::MAX);
        self.database()?.execute(
            "INSERT INTO entries(key, relative_path, size, accessed_ns)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET
                 relative_path = excluded.relative_path,
                 size = excluded.size,
                 accessed_ns = excluded.accessed_ns",
            params![
                key,
                relative.to_string_lossy(),
                i64::try_from(size).unwrap_or(i64::MAX),
                accessed_ns
            ],
        )?;
        Ok(())
    }

    fn content_path(&self, key: &str) -> PathBuf {
        self.root.join("data").join(&key[..2]).join(key)
    }

    fn lock_path(&self, key: &str) -> PathBuf {
        self.root.join("locks").join(format!("{key}.lock"))
    }
}

struct ExtractionSlot {
    lock: File,
}

impl Drop for ExtractionSlot {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}

fn member_key(identity: &ArchiveIdentity, entry_id: u64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(identity.key().as_bytes());
    hasher.update(&entry_id.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

fn is_complete(path: &Path, expected_size: u64) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == expected_size)
}

fn total_size(database: &Connection) -> Result<u64> {
    let total: Option<i64> = database
        .query_row("SELECT SUM(size) FROM entries", [], |row| row.get(0))
        .optional()?
        .flatten();
    Ok(total
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0))
}

fn open_lock(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| ArchiveVfsError::io(path, source))
}

fn read_dir(path: &Path) -> Result<fs::ReadDir> {
    fs::read_dir(path).map_err(|source| ArchiveVfsError::io(path, source))
}
