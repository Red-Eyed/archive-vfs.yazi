use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use flate2::read::DeflateDecoder;

use super::{
    ArchiveBackend, ArchiveEntry, EntryKind, EntrySink, ExtractionLimits, IndexedMember,
    normalize_member_path,
};
use crate::{ArchiveVfsError, Result, config::FilenamePolicy};

const LOCAL_HEADER_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EXTRA_ID: u16 = 0x0001;
const MAX_EOCD_SEARCH: u64 = 65_535 + 22;
const ENCRYPTED_FLAG: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CentralDirectory {
    offset: u64,
    size: u64,
    entries: u64,
}

pub struct ZipBackend;

impl ArchiveBackend for ZipBackend {
    fn name(&self) -> &'static str {
        "zip"
    }

    fn probe(&self, archive: &mut File) -> Result<bool> {
        archive
            .seek(SeekFrom::Start(0))
            .map_err(|source| ArchiveVfsError::io("archive", source))?;
        let signature = read_array::<4>(archive)?;
        let signature = le_u32(&signature, 0)?;
        Ok(matches!(
            signature,
            LOCAL_HEADER_SIGNATURE | EOCD_SIGNATURE | ZIP64_EOCD_SIGNATURE
        ))
    }

    fn index(
        &self,
        archive: &mut File,
        filename_policy: FilenamePolicy,
        sink: &mut dyn EntrySink,
    ) -> Result<()> {
        let directory = find_central_directory(archive)?;
        let archive_len = archive
            .metadata()
            .map_err(|source| ArchiveVfsError::io("archive", source))?
            .len();
        let directory_end = directory
            .offset
            .checked_add(directory.size)
            .ok_or_else(|| corrupt("central-directory bounds overflow"))?;
        if directory_end > archive_len {
            return Err(corrupt("central directory extends beyond archive"));
        }

        archive
            .seek(SeekFrom::Start(directory.offset))
            .map_err(|source| ArchiveVfsError::io("archive", source))?;
        for entry_id in 0..directory.entries {
            let entry = read_central_entry(archive, entry_id, filename_policy)?;
            if !entry.path.components.is_empty() {
                sink.accept(entry)?;
            }
        }
        let actual_end = archive
            .stream_position()
            .map_err(|source| ArchiveVfsError::io("archive", source))?;
        if actual_end != directory_end {
            return Err(corrupt(format!(
                "central-directory size mismatch: expected end {directory_end}, got {actual_end}"
            )));
        }
        Ok(())
    }

    fn extract(
        &self,
        archive: &mut File,
        member: &IndexedMember,
        limits: ExtractionLimits,
        output: &mut dyn Write,
    ) -> Result<u64> {
        validate_limits(member, limits)?;
        if member.flags & ENCRYPTED_FLAG != 0 {
            return Err(ArchiveVfsError::Encrypted(member.display_path.clone()));
        }

        archive
            .seek(SeekFrom::Start(member.local_header_offset))
            .map_err(|source| ArchiveVfsError::io("archive", source))?;
        let header = read_array::<30>(archive)?;
        if le_u32(&header, 0)? != LOCAL_HEADER_SIGNATURE {
            return Err(corrupt("local file header signature is invalid"));
        }
        let name_len = u64::from(le_u16(&header, 26)?);
        let extra_len = u64::from(le_u16(&header, 28)?);
        let data_offset = member
            .local_header_offset
            .checked_add(30)
            .and_then(|value| value.checked_add(name_len))
            .and_then(|value| value.checked_add(extra_len))
            .ok_or_else(|| corrupt("member data offset overflow"))?;
        archive
            .seek(SeekFrom::Start(data_offset))
            .map_err(|source| ArchiveVfsError::io("archive", source))?;

        let compressed = archive.take(member.compressed_size);
        match member.method {
            0 => copy_verified(compressed, output, member, limits),
            8 => copy_verified(DeflateDecoder::new(compressed), output, member, limits),
            method => Err(ArchiveVfsError::UnsupportedCompression {
                method,
                member: member.display_path.clone(),
            }),
        }
    }
}

fn find_central_directory(archive: &mut File) -> Result<CentralDirectory> {
    let len = archive
        .metadata()
        .map_err(|source| ArchiveVfsError::io("archive", source))?
        .len();
    if len < 22 {
        return Err(corrupt("file is too small to contain an EOCD record"));
    }
    let search_len = len.min(MAX_EOCD_SEARCH);
    archive
        .seek(SeekFrom::Start(len - search_len))
        .map_err(|source| ArchiveVfsError::io("archive", source))?;
    let mut tail = vec![0_u8; usize::try_from(search_len).map_err(|_| corrupt("EOCD window"))?];
    archive
        .read_exact(&mut tail)
        .map_err(|source| ArchiveVfsError::io("archive", source))?;

    let relative = (0..=tail.len() - 22)
        .rev()
        .find(|&offset| {
            le_u32(&tail, offset).ok() == Some(EOCD_SIGNATURE)
                && le_u16(&tail, offset + 20)
                    .ok()
                    .is_some_and(|comment| offset + 22 + usize::from(comment) == tail.len())
        })
        .ok_or_else(|| corrupt("end-of-central-directory record not found"))?;
    let absolute = len - search_len + u64::try_from(relative).unwrap_or(0);
    parse_eocd(archive, &tail[relative..], absolute)
}

fn parse_eocd(archive: &mut File, eocd: &[u8], eocd_offset: u64) -> Result<CentralDirectory> {
    let disk = le_u16(eocd, 4)?;
    let directory_disk = le_u16(eocd, 6)?;
    if disk != 0 || directory_disk != 0 {
        return Err(corrupt("multi-disk ZIP archives are not supported"));
    }
    let entries_on_disk = le_u16(eocd, 8)?;
    let entries = le_u16(eocd, 10)?;
    let size = le_u32(eocd, 12)?;
    let offset = le_u32(eocd, 16)?;
    let needs_zip64 = entries_on_disk == u16::MAX
        || entries == u16::MAX
        || size == u32::MAX
        || offset == u32::MAX;
    if needs_zip64 {
        parse_zip64_directory(archive, eocd_offset)
    } else {
        if entries_on_disk != entries {
            return Err(corrupt("central-directory entry counts disagree"));
        }
        Ok(CentralDirectory {
            offset: u64::from(offset),
            size: u64::from(size),
            entries: u64::from(entries),
        })
    }
}

fn parse_zip64_directory(archive: &mut File, eocd_offset: u64) -> Result<CentralDirectory> {
    let locator_offset = eocd_offset
        .checked_sub(20)
        .ok_or_else(|| corrupt("ZIP64 locator offset underflow"))?;
    archive
        .seek(SeekFrom::Start(locator_offset))
        .map_err(|source| ArchiveVfsError::io("archive", source))?;
    let locator = read_array::<20>(archive)?;
    if le_u32(&locator, 0)? != ZIP64_LOCATOR_SIGNATURE {
        return Err(corrupt("ZIP64 locator not found"));
    }
    if le_u32(&locator, 4)? != 0 || le_u32(&locator, 16)? != 1 {
        return Err(corrupt("multi-disk ZIP64 archives are not supported"));
    }
    let zip64_offset = le_u64(&locator, 8)?;
    archive
        .seek(SeekFrom::Start(zip64_offset))
        .map_err(|source| ArchiveVfsError::io("archive", source))?;
    let header = read_array::<56>(archive)?;
    if le_u32(&header, 0)? != ZIP64_EOCD_SIGNATURE {
        return Err(corrupt("ZIP64 EOCD signature is invalid"));
    }
    if le_u64(&header, 4)? < 44 {
        return Err(corrupt("ZIP64 EOCD record is too short"));
    }
    if le_u32(&header, 16)? != 0 || le_u32(&header, 20)? != 0 {
        return Err(corrupt("multi-disk ZIP64 archives are not supported"));
    }
    let entries_on_disk = le_u64(&header, 24)?;
    let entries = le_u64(&header, 32)?;
    if entries_on_disk != entries {
        return Err(corrupt("ZIP64 central-directory entry counts disagree"));
    }
    Ok(CentralDirectory {
        entries,
        size: le_u64(&header, 40)?,
        offset: le_u64(&header, 48)?,
    })
}

fn read_central_entry(
    archive: &mut File,
    entry_id: u64,
    filename_policy: FilenamePolicy,
) -> Result<ArchiveEntry> {
    let fixed = read_array::<46>(archive)?;
    if le_u32(&fixed, 0)? != CENTRAL_HEADER_SIGNATURE {
        return Err(corrupt(format!(
            "central-directory entry {entry_id} has an invalid signature"
        )));
    }
    let flags = le_u16(&fixed, 8)?;
    let method = le_u16(&fixed, 10)?;
    let modified_time = le_u16(&fixed, 12)?;
    let modified_date = le_u16(&fixed, 14)?;
    let crc32 = le_u32(&fixed, 16)?;
    let compressed_32 = le_u32(&fixed, 20)?;
    let uncompressed_32 = le_u32(&fixed, 24)?;
    let name_len = usize::from(le_u16(&fixed, 28)?);
    let extra_len = usize::from(le_u16(&fixed, 30)?);
    let comment_len = usize::from(le_u16(&fixed, 32)?);
    let disk_start_16 = le_u16(&fixed, 34)?;
    let local_offset_32 = le_u32(&fixed, 42)?;

    let mut name = vec![0_u8; name_len];
    archive
        .read_exact(&mut name)
        .map_err(|source| ArchiveVfsError::io("archive", source))?;
    let mut extra = vec![0_u8; extra_len];
    archive
        .read_exact(&mut extra)
        .map_err(|source| ArchiveVfsError::io("archive", source))?;
    archive
        .seek(SeekFrom::Current(
            i64::try_from(comment_len).map_err(|_| corrupt("ZIP comment length overflow"))?,
        ))
        .map_err(|source| ArchiveVfsError::io("archive", source))?;

    let zip64 = parse_zip64_extra(
        &extra,
        Zip64Needs::from_headers(
            uncompressed_32,
            compressed_32,
            local_offset_32,
            disk_start_16,
        ),
    )?;
    let disk_start = zip64.disk_start.unwrap_or(u32::from(disk_start_16));
    if disk_start != 0 {
        return Err(corrupt("multi-disk ZIP entries are not supported"));
    }
    let path = normalize_member_path(&name, flags, &extra, filename_policy);
    let kind = if path.is_directory {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    Ok(ArchiveEntry {
        entry_id,
        path,
        kind,
        compressed_size: zip64.compressed.unwrap_or(u64::from(compressed_32)),
        uncompressed_size: zip64.uncompressed.unwrap_or(u64::from(uncompressed_32)),
        crc32,
        method,
        flags,
        local_header_offset: zip64.local_offset.unwrap_or(u64::from(local_offset_32)),
        modified: dos_time(modified_date, modified_time),
    })
}

#[derive(Default)]
struct Zip64Values {
    uncompressed: Option<u64>,
    compressed: Option<u64>,
    local_offset: Option<u64>,
    disk_start: Option<u32>,
}

#[derive(Clone, Copy)]
struct Zip64Needs(u8);

impl Zip64Needs {
    const UNCOMPRESSED: u8 = 1;
    const COMPRESSED: u8 = 1 << 1;
    const LOCAL_OFFSET: u8 = 1 << 2;
    const DISK_START: u8 = 1 << 3;

    const fn from_headers(
        uncompressed: u32,
        compressed: u32,
        local_offset: u32,
        disk_start: u16,
    ) -> Self {
        let mut flags = 0;
        if uncompressed == u32::MAX {
            flags |= Self::UNCOMPRESSED;
        }
        if compressed == u32::MAX {
            flags |= Self::COMPRESSED;
        }
        if local_offset == u32::MAX {
            flags |= Self::LOCAL_OFFSET;
        }
        if disk_start == u16::MAX {
            flags |= Self::DISK_START;
        }
        Self(flags)
    }

    const fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

fn parse_zip64_extra(mut extra: &[u8], needs: Zip64Needs) -> Result<Zip64Values> {
    while extra.len() >= 4 {
        let field_id = le_u16(extra, 0)?;
        let size = usize::from(le_u16(extra, 2)?);
        extra = extra
            .get(4..)
            .ok_or_else(|| corrupt("truncated ZIP extra field"))?;
        let value = extra
            .get(..size)
            .ok_or_else(|| corrupt("truncated ZIP extra field value"))?;
        if field_id == ZIP64_EXTRA_ID {
            return parse_zip64_values(value, needs);
        }
        extra = &extra[size..];
    }
    if needs.0 != 0 {
        return Err(corrupt("required ZIP64 extended information is missing"));
    }
    Ok(Zip64Values::default())
}

fn parse_zip64_values(value: &[u8], needs: Zip64Needs) -> Result<Zip64Values> {
    let mut cursor = 0;
    let mut values = Zip64Values::default();
    if needs.contains(Zip64Needs::UNCOMPRESSED) {
        values.uncompressed = Some(take_u64(value, &mut cursor)?);
    }
    if needs.contains(Zip64Needs::COMPRESSED) {
        values.compressed = Some(take_u64(value, &mut cursor)?);
    }
    if needs.contains(Zip64Needs::LOCAL_OFFSET) {
        values.local_offset = Some(take_u64(value, &mut cursor)?);
    }
    if needs.contains(Zip64Needs::DISK_START) {
        values.disk_start = Some(take_u32(value, &mut cursor)?);
    }
    Ok(values)
}

fn validate_limits(member: &IndexedMember, limits: ExtractionLimits) -> Result<()> {
    if member.uncompressed_size > limits.max_member_bytes {
        return Err(ArchiveVfsError::SizeLimit {
            actual: member.uncompressed_size,
            maximum: limits.max_member_bytes,
        });
    }
    let ratio = if member.compressed_size == 0 {
        if member.uncompressed_size == 0 {
            1.0
        } else {
            f64::INFINITY
        }
    } else {
        compression_ratio(member.uncompressed_size, member.compressed_size)
    };
    if ratio > limits.max_compression_ratio {
        return Err(ArchiveVfsError::RatioLimit {
            actual: ratio,
            maximum: limits.max_compression_ratio,
        });
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn compression_ratio(uncompressed_size: u64, compressed_size: u64) -> f64 {
    uncompressed_size as f64 / compressed_size as f64
}

fn copy_verified(
    mut input: impl Read,
    output: &mut dyn Write,
    member: &IndexedMember,
    limits: ExtractionLimits,
) -> Result<u64> {
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut total = 0_u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|source| ArchiveVfsError::io("archive member", source))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| corrupt("member size overflow"))?)
            .ok_or_else(|| corrupt("member size overflow"))?;
        if total > limits.max_member_bytes || total > member.uncompressed_size {
            return Err(ArchiveVfsError::SizeLimit {
                actual: total,
                maximum: limits.max_member_bytes.min(member.uncompressed_size),
            });
        }
        hasher.update(&buffer[..read]);
        output
            .write_all(&buffer[..read])
            .map_err(|source| ArchiveVfsError::io("cache file", source))?;
    }
    if total != member.uncompressed_size {
        return Err(corrupt(format!(
            "member size mismatch for {}: expected {}, got {total}",
            member.display_path, member.uncompressed_size
        )));
    }
    let actual_crc = hasher.finalize();
    if actual_crc != member.crc32 {
        return Err(corrupt(format!(
            "CRC mismatch for {}: expected {:08x}, got {actual_crc:08x}",
            member.display_path, member.crc32
        )));
    }
    Ok(total)
}

fn dos_time(date: u16, time: u16) -> Option<SystemTime> {
    let year = i32::from((date >> 9) & 0x7f) + 1980;
    let month = u32::from((date >> 5) & 0x0f);
    let day = u32::from(date & 0x1f);
    let hour = u64::from((time >> 11) & 0x1f);
    let minute = u64::from((time >> 5) & 0x3f);
    let second = u64::from(time & 0x1f) * 2;
    let days = days_from_civil(year, month, day)?;
    let seconds = u64::try_from(days).ok()?.checked_mul(86_400)?;
    let seconds = seconds
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let adjusted_year = year - i32::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = i32::try_from(month).ok()? + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + i32::try_from(day).ok()? - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(i64::from(era * 146_097 + day_of_era - 719_468))
}

const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn read_array<const N: usize>(reader: &mut impl Read) -> Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    reader
        .read_exact(&mut bytes)
        .map_err(|source| ArchiveVfsError::io("archive", source))?;
    Ok(bytes)
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    let value = le_u64(bytes, *cursor)?;
    *cursor += 8;
    Ok(value)
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32> {
    let value = le_u32(bytes, *cursor)?;
    *cursor += 4;
    Ok(value)
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| corrupt("truncated ZIP record"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| corrupt("truncated ZIP record"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn le_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| corrupt("truncated ZIP record"))?;
    Ok(u64::from_le_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

fn corrupt(message: impl Into<String>) -> ArchiveVfsError {
    ArchiveVfsError::Corrupt(message.into())
}

#[cfg(test)]
mod tests {
    use super::{days_from_civil, dos_time};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn civil_date_conversion_matches_unix_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), Some(0));
        assert_eq!(days_from_civil(2000, 1, 1), Some(10_957));
    }

    #[test]
    fn invalid_dos_date_has_no_timestamp() {
        assert_eq!(dos_time(0, 0), None);
    }

    #[test]
    fn valid_dos_date_is_typed_system_time() {
        let date = (1 << 5) | 1;
        assert_eq!(
            dos_time(date, 0),
            Some(UNIX_EPOCH + Duration::from_secs(315_532_800))
        );
    }
}
