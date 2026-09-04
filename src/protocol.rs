use std::{
    io::{self, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{ArchiveVfsError, Result, archive::EntryKind, identity::ArchiveIdentity, index::Node};

pub const LIST_MAGIC: &[u8; 8] = b"AVFSL1\0\0";
pub const STAT_MAGIC: &[u8; 8] = b"AVFSS1\0\0";
pub const PATH_MAGIC: &[u8; 8] = b"AVFSP1\0\0";

pub fn write_list(
    output: &mut impl Write,
    identity: &ArchiveIdentity,
    nodes: &[Node],
) -> Result<()> {
    output.write_all(LIST_MAGIC).map_err(protocol_io)?;
    write_u64(output, usize_to_u64(nodes.len())?)?;
    write_identity(output, identity)?;
    for node in nodes {
        write_node(output, node)?;
    }
    output.flush().map_err(protocol_io)
}

pub fn write_stat(output: &mut impl Write, identity: &ArchiveIdentity, node: &Node) -> Result<()> {
    output.write_all(STAT_MAGIC).map_err(protocol_io)?;
    write_identity(output, identity)?;
    write_node(output, node)?;
    output.flush().map_err(protocol_io)
}

pub fn write_path(output: &mut impl Write, path: &Path) -> Result<()> {
    output.write_all(PATH_MAGIC).map_err(protocol_io)?;
    let bytes = path_bytes(path);
    write_u32(output, usize_to_u32(bytes.len())?)?;
    output.write_all(bytes.as_ref()).map_err(protocol_io)?;
    output.flush().map_err(protocol_io)
}

fn write_identity(output: &mut impl Write, identity: &ArchiveIdentity) -> Result<()> {
    write_u64(output, identity.size)?;
    write_u64(output, u128_to_u64(identity.modified_ns))?;
    write_u64(output, identity.tag())
}

fn write_node(output: &mut impl Write, node: &Node) -> Result<()> {
    let kind = match node.kind {
        EntryKind::File => 0,
        EntryKind::Directory => 1,
    };
    output.write_all(&[kind]).map_err(protocol_io)?;
    write_u64(output, node.uncompressed_size)?;
    write_i64(output, timestamp_seconds(node.modified))?;
    write_u32(output, usize_to_u32(node.name.len())?)?;
    output.write_all(&node.name).map_err(protocol_io)
}

fn timestamp_seconds(timestamp: Option<SystemTime>) -> i64 {
    timestamp
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .and_then(|value| i64::try_from(value.as_secs()).ok())
        .unwrap_or(-1)
}

fn write_u32(output: &mut impl Write, value: u32) -> Result<()> {
    output.write_all(&value.to_le_bytes()).map_err(protocol_io)
}

fn write_u64(output: &mut impl Write, value: u64) -> Result<()> {
    output.write_all(&value.to_le_bytes()).map_err(protocol_io)
}

fn write_i64(output: &mut impl Write, value: i64) -> Result<()> {
    output.write_all(&value.to_le_bytes()).map_err(protocol_io)
}

fn usize_to_u32(value: usize) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| ArchiveVfsError::Protocol("protocol string exceeds 4 GiB".to_owned()))
}

fn usize_to_u64(value: usize) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| ArchiveVfsError::Protocol("record count exceeds u64".to_owned()))
}

fn u128_to_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn protocol_io(source: io::Error) -> ArchiveVfsError {
    ArchiveVfsError::io("protocol output", source)
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;

    std::borrow::Cow::Borrowed(path.as_os_str().as_bytes())
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    std::borrow::Cow::Owned(path.to_string_lossy().into_owned().into_bytes())
}
