use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{
    ArchiveVfsError, Result,
    archive::{ArchiveBackend, ArchiveEntry, EntryKind, EntrySink, IndexedMember},
    config::FilenamePolicy,
    identity::ArchiveIdentity,
};

const SCHEMA_VERSION: i64 = 1;
const ROOT_ID: i64 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub name: Vec<u8>,
    pub kind: EntryKind,
    pub uncompressed_size: u64,
    pub modified: Option<SystemTime>,
}

pub struct IndexStore {
    root: PathBuf,
}

impl IndexStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn ensure(
        &self,
        identity: &ArchiveIdentity,
        backend: &dyn ArchiveBackend,
        filename_policy: FilenamePolicy,
    ) -> Result<Index> {
        fs::create_dir_all(&self.root)
            .map_err(|source| ArchiveVfsError::io(self.root.clone(), source))?;
        let key = identity.key();
        let index_path = self.root.join(format!("{key}.sqlite"));
        if index_path.exists() {
            return Index::open(index_path);
        }

        let lock_path = self.root.join(format!("{key}.lock"));
        let lock = open_lock(&lock_path)?;
        lock.lock_exclusive()
            .map_err(|source| ArchiveVfsError::io(&lock_path, source))?;
        if !index_path.exists() {
            self.build(identity, backend, filename_policy, &index_path)?;
        }
        drop(lock);
        Index::open(index_path)
    }

    pub fn load(
        &self,
        identity: &ArchiveIdentity,
        backend: &dyn ArchiveBackend,
        filename_policy: FilenamePolicy,
        persist: bool,
    ) -> Result<Index> {
        if persist {
            self.ensure(identity, backend, filename_policy)
        } else {
            Self::build_transient(identity, backend, filename_policy)
        }
    }

    fn build(
        &self,
        identity: &ArchiveIdentity,
        backend: &dyn ArchiveBackend,
        filename_policy: FilenamePolicy,
        index_path: &Path,
    ) -> Result<()> {
        let temporary = self
            .root
            .join(format!("{}.part-{}", identity.key(), std::process::id()));
        if temporary.exists() {
            fs::remove_file(&temporary)
                .map_err(|source| ArchiveVfsError::io(&temporary, source))?;
        }
        let mut connection = Connection::open(&temporary)?;
        if let Err(error) = build_index(&mut connection, identity, backend, filename_policy) {
            drop(connection);
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        drop(connection);
        fs::rename(&temporary, index_path)
            .map_err(|source| ArchiveVfsError::io(index_path, source))?;
        Ok(())
    }

    fn build_transient(
        identity: &ArchiveIdentity,
        backend: &dyn ArchiveBackend,
        filename_policy: FilenamePolicy,
    ) -> Result<Index> {
        let mut connection = Connection::open_in_memory()?;
        build_index(&mut connection, identity, backend, filename_policy)?;
        Index::from_connection(PathBuf::from(":memory:"), connection)
    }
}

pub struct Index {
    path: PathBuf,
    connection: Connection,
}

impl Index {
    fn open(path: PathBuf) -> Result<Self> {
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Self::from_connection(path, connection)
    }

    fn from_connection(path: PathBuf, connection: Connection) -> Result<Self> {
        let schema: i64 = connection.query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        if schema != SCHEMA_VERSION {
            return Err(ArchiveVfsError::Corrupt(format!(
                "unsupported index schema {schema} in {}",
                path.display()
            )));
        }
        Ok(Self { path, connection })
    }

    pub fn node(&self, components: &[Vec<u8>]) -> Result<Node> {
        let node = self
            .lookup_node_id(components)?
            .ok_or_else(|| ArchiveVfsError::MemberNotFound(display_components(components)))?;
        self.connection
            .query_row(
                "SELECT name, kind, uncompressed_size, modified_secs FROM nodes WHERE id = ?1",
                [node],
                decode_node,
            )
            .map_err(Into::into)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read_dir(&self, components: &[Vec<u8>]) -> Result<Vec<Node>> {
        let parent = self
            .lookup_node_id(components)?
            .ok_or_else(|| ArchiveVfsError::MemberNotFound(display_components(components)))?;
        let mut statement = self.connection.prepare(
            "SELECT name, kind, uncompressed_size, modified_secs
             FROM nodes WHERE parent_id = ?1 ORDER BY ordinal, id",
        )?;
        let rows = statement.query_map([parent], decode_node)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn member(&self, components: &[Vec<u8>]) -> Result<IndexedMember> {
        let node = self
            .lookup_node_id(components)?
            .ok_or_else(|| ArchiveVfsError::MemberNotFound(display_components(components)))?;
        self.connection
            .query_row(
                "SELECT entry_id, compressed_size, uncompressed_size, crc32, method, flags,
                        local_header_offset
                 FROM nodes WHERE id = ?1 AND kind = 0 AND entry_id IS NOT NULL",
                [node],
                |row| {
                    Ok(IndexedMember {
                        entry_id: get_u64(row, 0)?,
                        display_path: display_components(components),
                        compressed_size: get_u64(row, 1)?,
                        uncompressed_size: get_u64(row, 2)?,
                        crc32: get_u32(row, 3)?,
                        method: get_u16(row, 4)?,
                        flags: get_u16(row, 5)?,
                        local_header_offset: get_u64(row, 6)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| ArchiveVfsError::MemberNotFound(display_components(components)))
    }

    fn lookup_node_id(&self, components: &[Vec<u8>]) -> Result<Option<i64>> {
        let mut parent = ROOT_ID;
        for component in components {
            let Some(found) = self
                .connection
                .query_row(
                    "SELECT id FROM nodes WHERE parent_id = ?1 AND name = ?2",
                    params![parent, component],
                    |row| row.get(0),
                )
                .optional()?
            else {
                return Ok(None);
            };
            parent = found;
        }
        Ok(Some(parent))
    }
}

fn build_index(
    connection: &mut Connection,
    identity: &ArchiveIdentity,
    backend: &dyn ArchiveBackend,
    filename_policy: FilenamePolicy,
) -> Result<()> {
    initialize_schema(connection, identity, backend.name())?;
    let transaction = connection.transaction()?;
    let mut sink = SqliteSink::new(transaction);
    let mut archive = File::open(&identity.canonical_path)
        .map_err(|source| ArchiveVfsError::io(&identity.canonical_path, source))?;
    backend.index(&mut archive, filename_policy, &mut sink)?;
    sink.finish()
}

fn decode_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let kind_value: i64 = row.get(1)?;
    let size: i64 = row.get(2)?;
    let modified_secs: Option<i64> = row.get(3)?;
    Ok(Node {
        name: row.get(0)?,
        kind: decode_kind(kind_value)?,
        uncompressed_size: u64::try_from(size)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, size))?,
        modified: modified_secs.and_then(system_time_from_seconds),
    })
}

struct SqliteSink<'connection> {
    transaction: rusqlite::Transaction<'connection>,
    ordinal: i64,
    duplicates: u64,
}

impl<'connection> SqliteSink<'connection> {
    fn new(transaction: rusqlite::Transaction<'connection>) -> Self {
        Self {
            transaction,
            ordinal: 0,
            duplicates: 0,
        }
    }

    fn finish(self) -> Result<()> {
        self.transaction.execute(
            "INSERT INTO metadata(key, value) VALUES ('duplicates', ?1)",
            [i64::try_from(self.duplicates).unwrap_or(i64::MAX)],
        )?;
        self.transaction.commit()?;
        Ok(())
    }

    fn ensure_directory(&self, parent: i64, name: &[u8]) -> Result<i64> {
        self.transaction.execute(
            "INSERT INTO nodes(parent_id, name, kind, ordinal)
             VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(parent_id, name) DO UPDATE SET kind = 1",
            params![parent, name, self.ordinal],
        )?;
        self.transaction
            .query_row(
                "SELECT id FROM nodes WHERE parent_id = ?1 AND name = ?2",
                params![parent, name],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }
}

impl EntrySink for SqliteSink<'_> {
    fn accept(&mut self, entry: ArchiveEntry) -> Result<()> {
        let mut parent = ROOT_ID;
        let last = entry.path.components.len() - 1;
        for component in &entry.path.components[..last] {
            parent = self.ensure_directory(parent, component)?;
        }
        let name = &entry.path.components[last];
        let existing_entry: Option<Option<i64>> = self
            .transaction
            .query_row(
                "SELECT entry_id FROM nodes WHERE parent_id = ?1 AND name = ?2",
                params![parent, name],
                |row| row.get(0),
            )
            .optional()?;
        if existing_entry.flatten().is_some() {
            self.duplicates += 1;
        }
        let kind = encode_kind(entry.kind);
        let modified_secs = entry
            .modified
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .and_then(|value| i64::try_from(value.as_secs()).ok());
        self.transaction.execute(
            "INSERT INTO nodes(
                parent_id, name, kind, ordinal, entry_id, compressed_size,
                uncompressed_size, crc32, method, flags, local_header_offset, modified_secs
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(parent_id, name) DO UPDATE SET
                kind = excluded.kind,
                ordinal = excluded.ordinal,
                entry_id = excluded.entry_id,
                compressed_size = excluded.compressed_size,
                uncompressed_size = excluded.uncompressed_size,
                crc32 = excluded.crc32,
                method = excluded.method,
                flags = excluded.flags,
                local_header_offset = excluded.local_header_offset,
                modified_secs = excluded.modified_secs",
            params![
                parent,
                name,
                kind,
                self.ordinal,
                to_i64(entry.entry_id)?,
                to_i64(entry.compressed_size)?,
                to_i64(entry.uncompressed_size)?,
                i64::from(entry.crc32),
                i64::from(entry.method),
                i64::from(entry.flags),
                to_i64(entry.local_header_offset)?,
                modified_secs,
            ],
        )?;
        self.ordinal = self.ordinal.saturating_add(1);
        Ok(())
    }
}

fn initialize_schema(
    connection: &Connection,
    identity: &ArchiveIdentity,
    backend: &str,
) -> Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = MEMORY;
         CREATE TABLE metadata (
             key TEXT PRIMARY KEY,
             value
         ) WITHOUT ROWID;
         CREATE TABLE nodes (
             id INTEGER PRIMARY KEY,
             parent_id INTEGER REFERENCES nodes(id),
             name BLOB NOT NULL,
             kind INTEGER NOT NULL,
             ordinal INTEGER NOT NULL,
             entry_id INTEGER,
             compressed_size INTEGER NOT NULL DEFAULT 0,
             uncompressed_size INTEGER NOT NULL DEFAULT 0,
             crc32 INTEGER NOT NULL DEFAULT 0,
             method INTEGER NOT NULL DEFAULT 0,
             flags INTEGER NOT NULL DEFAULT 0,
             local_header_offset INTEGER NOT NULL DEFAULT 0,
             modified_secs INTEGER,
             UNIQUE(parent_id, name)
         );
         CREATE INDEX nodes_parent_order ON nodes(parent_id, ordinal, id);",
    )?;
    connection.execute(
        "INSERT INTO nodes(id, parent_id, name, kind, ordinal) VALUES (?1, NULL, X'', 1, 0)",
        [ROOT_ID],
    )?;
    connection.execute(
        "INSERT INTO metadata(key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION],
    )?;
    connection.execute(
        "INSERT INTO metadata(key, value) VALUES ('identity', ?1)",
        [identity.key()],
    )?;
    connection.execute(
        "INSERT INTO metadata(key, value) VALUES ('backend', ?1)",
        [backend],
    )?;
    Ok(())
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

const fn encode_kind(kind: EntryKind) -> i64 {
    match kind {
        EntryKind::File => 0,
        EntryKind::Directory => 1,
    }
}

fn decode_kind(value: i64) -> rusqlite::Result<EntryKind> {
    match value {
        0 => Ok(EntryKind::File),
        1 => Ok(EntryKind::Directory),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(1, value)),
    }
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        ArchiveVfsError::Corrupt(format!("value {value} exceeds SQLite integer range"))
    })
}

fn get_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn get_u32(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u32> {
    let value: i64 = row.get(index)?;
    u32::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn get_u16(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u16> {
    let value: i64 = row.get(index)?;
    u16::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn system_time_from_seconds(seconds: i64) -> Option<SystemTime> {
    u64::try_from(seconds)
        .ok()
        .and_then(|value| UNIX_EPOCH.checked_add(std::time::Duration::from_secs(value)))
}

fn display_components(components: &[Vec<u8>]) -> String {
    components
        .iter()
        .map(|component| String::from_utf8_lossy(component))
        .collect::<Vec<_>>()
        .join("/")
}
