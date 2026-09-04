use std::borrow::Cow;

use crc32fast::hash;
use oem_cp::{code_table::DECODING_TABLE_CP437, decode_string_complete_table};

use crate::config::FilenamePolicy;

const UTF8_FLAG: u16 = 1 << 11;
const UNICODE_PATH_EXTRA_ID: u16 = 0x7075;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedPath {
    pub components: Vec<Vec<u8>>,
    pub is_directory: bool,
}

pub fn normalize_member_path(
    raw_name: &[u8],
    flags: u16,
    extra: &[u8],
    policy: FilenamePolicy,
) -> NormalizedPath {
    let decoded = decode_name(raw_name, flags, extra, policy);
    let is_directory = decoded.ends_with(b"/") || decoded.ends_with(b"\\");
    let components = decoded
        .split(|byte| matches!(byte, b'/' | b'\\'))
        .filter_map(normalize_component)
        .collect();
    NormalizedPath {
        components,
        is_directory,
    }
}

fn decode_name<'a>(
    raw_name: &'a [u8],
    flags: u16,
    extra: &'a [u8],
    policy: FilenamePolicy,
) -> Cow<'a, [u8]> {
    match policy {
        FilenamePolicy::Raw => Cow::Borrowed(raw_name),
        FilenamePolicy::LossyUtf8 => String::from_utf8_lossy(raw_name)
            .into_owned()
            .into_bytes()
            .into(),
        FilenamePolicy::Standard => {
            if let Some(unicode) = unicode_path(raw_name, extra) {
                return Cow::Owned(unicode.to_owned());
            }
            if flags & UTF8_FLAG != 0 {
                return match std::str::from_utf8(raw_name) {
                    Ok(_) => Cow::Borrowed(raw_name),
                    Err(_) => Cow::Owned(escape_bytes(raw_name)),
                };
            }
            Cow::Owned(decode_string_complete_table(raw_name, &DECODING_TABLE_CP437).into_bytes())
        }
    }
}

fn unicode_path<'a>(raw_name: &[u8], mut extra: &'a [u8]) -> Option<&'a [u8]> {
    while extra.len() >= 4 {
        let field_id = u16::from_le_bytes([extra[0], extra[1]]);
        let size = usize::from(u16::from_le_bytes([extra[2], extra[3]]));
        extra = extra.get(4..)?;
        let value = extra.get(..size)?;
        if field_id == UNICODE_PATH_EXTRA_ID
            && value.len() >= 5
            && value[0] == 1
            && u32::from_le_bytes([value[1], value[2], value[3], value[4]]) == hash(raw_name)
            && std::str::from_utf8(&value[5..]).is_ok()
        {
            return Some(&value[5..]);
        }
        extra = extra.get(size..)?;
    }
    None
}

fn normalize_component(component: &[u8]) -> Option<Vec<u8>> {
    if component.is_empty() || component == b"." {
        return None;
    }
    if component == b".." {
        return Some(b"%2E%2E".to_vec());
    }

    let mut normalized = Vec::with_capacity(component.len());
    for &byte in component {
        match byte {
            0 => normalized.extend_from_slice(b"%00"),
            b'%' => normalized.extend_from_slice(b"%25"),
            _ => normalized.push(byte),
        }
    }
    (!normalized.is_empty()).then_some(normalized)
}

fn escape_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(bytes.len() * 3);
    for byte in bytes {
        escaped.extend_from_slice(format!("%{byte:02X}").as_bytes());
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_stays_inside_virtual_root() {
        let path = normalize_member_path(
            b"../../outside/file.txt",
            UTF8_FLAG,
            &[],
            FilenamePolicy::Standard,
        );
        assert_eq!(
            path.components,
            [b"%2E%2E".as_slice(), b"%2E%2E", b"outside", b"file.txt"]
        );
    }

    #[test]
    fn cp437_name_uses_zip_default_encoding() {
        let path = normalize_member_path(b"caf\x82.txt", 0, &[], FilenamePolicy::Standard);
        assert_eq!(path.components, ["café.txt".as_bytes()]);
    }

    #[test]
    fn valid_unicode_extra_field_overrides_legacy_name() {
        let raw = b"legacy.txt";
        let name = "дані.txt".as_bytes();
        let mut value = vec![1];
        value.extend_from_slice(&hash(raw).to_le_bytes());
        value.extend_from_slice(name);
        let mut extra = UNICODE_PATH_EXTRA_ID.to_le_bytes().to_vec();
        extra.extend_from_slice(
            &u16::try_from(value.len())
                .expect("test Unicode extra field fits in u16")
                .to_le_bytes(),
        );
        extra.extend_from_slice(&value);
        let path = normalize_member_path(raw, 0, &extra, FilenamePolicy::Standard);
        assert_eq!(path.components, [name]);
    }
}
