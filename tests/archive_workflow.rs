use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use archive_vfs_helper::{
    ArchiveVfsError,
    archive::{ArchiveBackend, ArchiveRegistry, EntryKind, ZipBackend},
    cache::MemberCache,
    config::Config,
    identity::ArchiveIdentity,
    index::{Index, IndexStore},
};
use tempfile::TempDir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

struct Fixture {
    workspace: TempDir,
    archive: PathBuf,
    index: Index,
    identity: ArchiveIdentity,
}

impl Fixture {
    fn new(entries: &[(&str, &[u8], CompressionMethod)]) -> Self {
        let workspace = TempDir::new().expect("create fixture workspace");
        let archive = workspace.path().join("dataset [sample] '$.zip");
        write_zip(&archive, entries);
        let identity = ArchiveIdentity::from_path(&archive).expect("identify archive");
        let registry = ArchiveRegistry::with_defaults();
        let mut file = File::open(&archive).expect("open archive");
        let backend = registry
            .detect(&archive, &mut file)
            .expect("detect ZIP backend");
        let index = IndexStore::new(workspace.path().join("indexes"))
            .ensure(&identity, backend, Config::default().filename_policy)
            .expect("build index");
        Self {
            workspace,
            archive,
            index,
            identity,
        }
    }

    fn cache_config(&self) -> Config {
        Config {
            cache_dir: self.workspace.path().join("members"),
            index_dir: self.workspace.path().join("indexes"),
            ..Config::default()
        }
    }
}

#[test]
fn indexes_nested_implied_directories_in_central_order() {
    let fixture = Fixture::new(&[
        (
            "nested/first.json",
            br#"{"value":1}"#,
            CompressionMethod::Deflated,
        ),
        ("root.txt", b"root", CompressionMethod::Stored),
        ("nested/second.txt", b"second", CompressionMethod::Stored),
    ]);

    let root = fixture.index.read_dir(&[]).expect("list root");
    assert_eq!(root.len(), 2);
    assert_eq!(root[0].name, b"nested");
    assert_eq!(root[0].kind, EntryKind::Directory);
    assert_eq!(root[1].name, b"root.txt");

    let nested = fixture
        .index
        .read_dir(&[b"nested".to_vec()])
        .expect("list nested directory");
    assert_eq!(
        nested
            .iter()
            .map(|node| node.name.as_slice())
            .collect::<Vec<_>>(),
        [b"first.json".as_slice(), b"second.txt"]
    );
}

#[test]
fn preserves_explicit_empty_directories() {
    let workspace = TempDir::new().expect("create empty-directory workspace");
    let archive = workspace.path().join("empty-directory.zip");
    let file = File::create(&archive).expect("create empty-directory ZIP");
    let mut writer = ZipWriter::new(file);
    writer
        .add_directory("empty/", SimpleFileOptions::default())
        .expect("add explicit directory");
    writer.finish().expect("finish empty-directory ZIP");

    let identity = ArchiveIdentity::from_path(&archive).expect("identify archive");
    let index = IndexStore::new(workspace.path().join("indexes"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index archive");
    let root = index.read_dir(&[]).expect("list archive root");
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].name, b"empty");
    assert_eq!(root[0].kind, EntryKind::Directory);
    assert!(
        index
            .read_dir(&[b"empty".to_vec()])
            .expect("list empty directory")
            .is_empty()
    );
}

#[test]
fn extracts_stored_deflated_empty_unicode_and_metacharacter_names() {
    let fixture = Fixture::new(&[
        ("stored.txt", b"stored", CompressionMethod::Stored),
        (
            "deflated.json",
            br#"{"ok":true}"#,
            CompressionMethod::Deflated,
        ),
        ("empty.txt", b"", CompressionMethod::Stored),
        (
            "дані/[a] '$.txt",
            "привіт".as_bytes(),
            CompressionMethod::Deflated,
        ),
    ]);
    let cache = MemberCache::from_config(&fixture.cache_config());
    let backend = ZipBackend;

    for (components, expected) in [
        (vec![b"stored.txt".to_vec()], b"stored".as_slice()),
        (
            vec![b"deflated.json".to_vec()],
            br#"{"ok":true}"#.as_slice(),
        ),
        (vec![b"empty.txt".to_vec()], b"".as_slice()),
        (
            vec!["дані".as_bytes().to_vec(), b"[a] '$.txt".to_vec()],
            "привіт".as_bytes(),
        ),
    ] {
        let member = fixture.index.member(&components).expect("lookup member");
        let lease = cache
            .lease(&fixture.identity, &backend, &member)
            .expect("materialize member");
        let mut actual = Vec::new();
        lease
            .open()
            .expect("open cached member")
            .read_to_end(&mut actual)
            .expect("read cached member");
        assert_eq!(actual, expected);
    }
}

#[test]
fn duplicate_member_names_use_last_central_directory_entry() {
    let workspace = TempDir::new().expect("create duplicate fixture workspace");
    let archive = workspace.path().join("duplicates.zip");
    write_zip(
        &archive,
        &[
            ("duplicate-a.txt", b"first", CompressionMethod::Stored),
            ("duplicate-b.txt", b"second", CompressionMethod::Stored),
        ],
    );
    replace_all(&archive, b"duplicate-b.txt", b"duplicate-a.txt");
    let identity = ArchiveIdentity::from_path(&archive).expect("identify duplicate fixture");
    let index = IndexStore::new(workspace.path().join("indexes"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index duplicate fixture");
    let member = index
        .member(&[b"duplicate-a.txt".to_vec()])
        .expect("lookup duplicate");
    assert_eq!(member.entry_id, 1);

    let config = Config {
        cache_dir: workspace.path().join("members"),
        ..Config::default()
    };
    let cache = MemberCache::from_config(&config);
    let lease = cache
        .lease(&identity, &ZipBackend, &member)
        .expect("extract winning duplicate");
    assert_eq!(fs::read(lease.path()).expect("read winner"), b"second");
}

#[test]
fn traversal_components_are_visible_but_cannot_escape() {
    let fixture = Fixture::new(&[("../../outside.txt", b"contained", CompressionMethod::Stored)]);
    let root = fixture.index.read_dir(&[]).expect("list root");
    assert_eq!(root[0].name, b"%2E%2E");
    let member = fixture
        .index
        .member(&[
            b"%2E%2E".to_vec(),
            b"%2E%2E".to_vec(),
            b"outside.txt".to_vec(),
        ])
        .expect("lookup sanitized traversal");
    let cache = MemberCache::from_config(&fixture.cache_config());
    let lease = cache
        .lease(&fixture.identity, &ZipBackend, &member)
        .expect("extract sanitized traversal");
    assert!(lease.path().starts_with(&fixture.cache_config().cache_dir));
}

#[test]
fn forced_zip64_member_metadata_is_indexed_and_extracted() {
    let workspace = TempDir::new().expect("create fixture workspace");
    let archive = workspace.path().join("zip64.zip");
    let file = File::create(&archive).expect("create ZIP64 fixture");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true);
    writer
        .start_file("large-metadata.bin", options)
        .expect("start ZIP64 member");
    writer
        .write_all(b"small payload")
        .expect("write ZIP64 member");
    writer.finish().expect("finish ZIP64 fixture");

    let identity = ArchiveIdentity::from_path(&archive).expect("identify ZIP64 fixture");
    let index = IndexStore::new(workspace.path().join("indexes"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index ZIP64 fixture");
    let member = index
        .member(&[b"large-metadata.bin".to_vec()])
        .expect("lookup ZIP64 member");
    assert_eq!(member.uncompressed_size, 13);

    let config = Config {
        cache_dir: workspace.path().join("members"),
        ..Config::default()
    };
    let lease = MemberCache::from_config(&config)
        .lease(&identity, &ZipBackend, &member)
        .expect("extract ZIP64 member");
    assert_eq!(
        fs::read(lease.path()).expect("read ZIP64 member"),
        b"small payload"
    );
}

#[test]
fn archive_replacement_creates_a_new_identity_and_index() {
    let workspace = TempDir::new().expect("create fixture workspace");
    let archive = workspace.path().join("replace.zip");
    write_zip(&archive, &[("old.txt", b"old", CompressionMethod::Stored)]);
    let first_identity = ArchiveIdentity::from_path(&archive).expect("first identity");
    let store = IndexStore::new(workspace.path().join("indexes"));
    let first = store
        .ensure(
            &first_identity,
            &ZipBackend,
            Config::default().filename_policy,
        )
        .expect("first index");
    let first_path = first.path().to_owned();
    drop(first);

    write_zip(&archive, &[("new.txt", b"new", CompressionMethod::Stored)]);
    let second_identity = ArchiveIdentity::from_path(&archive).expect("second identity");
    let second = store
        .ensure(
            &second_identity,
            &ZipBackend,
            Config::default().filename_policy,
        )
        .expect("second index");
    assert_ne!(first_identity.key(), second_identity.key());
    assert_ne!(first_path, second.path());
    assert!(second.member(&[b"new.txt".to_vec()]).is_ok());
}

#[test]
fn concurrent_reads_share_one_complete_cache_entry() {
    let fixture = Fixture::new(&[(
        "image.bin",
        &vec![42; 512 * 1024],
        CompressionMethod::Deflated,
    )]);
    let member = Arc::new(
        fixture
            .index
            .member(&[b"image.bin".to_vec()])
            .expect("lookup concurrent member"),
    );
    let identity = Arc::new(fixture.identity.clone());
    let config = Arc::new(fixture.cache_config());
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let member = Arc::clone(&member);
        let identity = Arc::clone(&identity);
        let config = Arc::clone(&config);
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            barrier.wait();
            let cache = MemberCache::from_config(&config);
            let lease = cache
                .lease(&identity, &ZipBackend, &member)
                .expect("concurrent lease");
            fs::read(lease.path()).expect("read concurrent cache entry")
        }));
    }
    for task in threads {
        assert_eq!(task.join().expect("join reader"), vec![42; 512 * 1024]);
    }
    let data_files = files_below(&fixture.cache_config().cache_dir.join("data"));
    assert_eq!(data_files.len(), 1);
}

#[test]
fn eviction_skips_an_active_member_lease() {
    let fixture = Fixture::new(&[
        ("active.bin", b"aaa", CompressionMethod::Stored),
        ("idle.bin", b"bbb", CompressionMethod::Stored),
    ]);
    let mut config = fixture.cache_config();
    config.max_cache_bytes = 3;
    let cache = MemberCache::from_config(&config);
    let active_member = fixture
        .index
        .member(&[b"active.bin".to_vec()])
        .expect("active member");
    let idle_member = fixture
        .index
        .member(&[b"idle.bin".to_vec()])
        .expect("idle member");
    let active = cache
        .lease(&fixture.identity, &ZipBackend, &active_member)
        .expect("active lease");
    let active_path = active.path().to_owned();
    let idle_path = {
        let idle = cache
            .lease(&fixture.identity, &ZipBackend, &idle_member)
            .expect("idle lease");
        idle.path().to_owned()
    };

    assert_eq!(cache.prune().expect("prune already-bounded cache"), 0);
    assert!(active_path.exists());
    assert!(!idle_path.exists());
}

#[test]
fn malformed_flagged_utf8_name_is_escaped_without_panicking() {
    let workspace = TempDir::new().expect("create malformed-name workspace");
    let archive = workspace.path().join("malformed-name.zip");
    write_zip(
        &archive,
        &[("badx.txt", b"data", CompressionMethod::Stored)],
    );
    replace_all(&archive, b"badx.txt", b"bad\xff.txt");
    patch_u16_after_signature(&archive, 0x0201_4b50, 8, 1 << 11);
    patch_u16_after_signature(&archive, 0x0403_4b50, 6, 1 << 11);

    let identity = ArchiveIdentity::from_path(&archive).expect("identify malformed-name archive");
    let index = IndexStore::new(workspace.path().join("indexes"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index malformed-name archive");
    let root = index.read_dir(&[]).expect("list malformed-name archive");
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].name, b"%2562%2561%2564%25FF%252E%2574%2578%2574");
}

#[test]
fn encrypted_member_is_rejected_on_access() {
    let fixture = Fixture::new(&[("secret.txt", b"secret", CompressionMethod::Stored)]);
    let config = fixture.cache_config();
    drop(fixture.index);
    patch_u16_after_signature(&fixture.archive, 0x0201_4b50, 8, 1);
    patch_u16_after_signature(&fixture.archive, 0x0403_4b50, 6, 1);
    let identity =
        ArchiveIdentity::from_path(&fixture.archive).expect("identify encrypted archive");
    let index = IndexStore::new(fixture.workspace.path().join("encrypted-index"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index encrypted member metadata");
    let member = index
        .member(&[b"secret.txt".to_vec()])
        .expect("lookup encrypted member");
    let error = MemberCache::from_config(&config)
        .lease(&identity, &ZipBackend, &member)
        .err()
        .expect("encrypted member must fail");
    assert!(matches!(error, ArchiveVfsError::Encrypted(_)));
}

#[test]
fn corrupted_member_crc_is_rejected_and_partial_is_removed() {
    let fixture = Fixture::new(&[("payload.bin", b"unique-payload", CompressionMethod::Stored)]);
    let config = fixture.cache_config();
    drop(fixture.index);
    replace_all(&fixture.archive, b"unique-payload", b"broken-payload");
    let identity =
        ArchiveIdentity::from_path(&fixture.archive).expect("identify corrupted archive");
    let index = IndexStore::new(fixture.workspace.path().join("crc-index"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index corrupted archive");
    let member = index
        .member(&[b"payload.bin".to_vec()])
        .expect("lookup corrupted member");
    let cache = MemberCache::from_config(&config);
    let error = cache
        .lease(&identity, &ZipBackend, &member)
        .err()
        .expect("CRC mismatch must fail");
    assert!(matches!(error, ArchiveVfsError::Corrupt(_)));
    assert!(files_below(&config.cache_dir.join("data")).is_empty());
}

#[test]
fn member_size_and_ratio_limits_are_enforced_before_extraction() {
    let fixture = Fixture::new(&[(
        "compressible.bin",
        &vec![0; 64 * 1024],
        CompressionMethod::Deflated,
    )]);
    let member = fixture
        .index
        .member(&[b"compressible.bin".to_vec()])
        .expect("lookup compressible member");
    let mut output = Vec::new();
    let size_error = ZipBackend
        .extract(
            &mut File::open(&fixture.archive).expect("open archive"),
            &member,
            archive_vfs_helper::archive::ExtractionLimits {
                max_member_bytes: 1024,
                max_compression_ratio: 1000.0,
            },
            &mut output,
        )
        .expect_err("member size limit must fail");
    assert!(matches!(size_error, ArchiveVfsError::SizeLimit { .. }));

    let ratio_error = ZipBackend
        .extract(
            &mut File::open(&fixture.archive).expect("open archive"),
            &member,
            archive_vfs_helper::archive::ExtractionLimits {
                max_member_bytes: 128 * 1024,
                max_compression_ratio: 2.0,
            },
            &mut output,
        )
        .expect_err("compression ratio limit must fail");
    assert!(matches!(ratio_error, ArchiveVfsError::RatioLimit { .. }));
}

#[test]
fn abandoned_partial_files_are_removed() {
    let fixture = Fixture::new(&[("file.txt", b"data", CompressionMethod::Stored)]);
    let config = fixture.cache_config();
    let partial_dir = config.cache_dir.join("data/ab");
    fs::create_dir_all(&partial_dir).expect("create partial directory");
    fs::write(partial_dir.join("abcdef.part-123"), b"partial").expect("write partial");
    let cache = MemberCache::from_config(&config);
    assert_eq!(cache.clean_partials().expect("clean partials"), 1);
    assert!(files_below(&partial_dir).is_empty());
}

#[test]
fn corrupted_central_directory_is_rejected() {
    let workspace = TempDir::new().expect("create fixture workspace");
    let archive = workspace.path().join("corrupt.zip");
    write_zip(
        &archive,
        &[("file.txt", b"data", CompressionMethod::Stored)],
    );
    patch_first_signature(&archive, 0x0201_4b50, 0x0201_4b51);
    let identity = ArchiveIdentity::from_path(&archive).expect("identify corrupt archive");
    let error = IndexStore::new(workspace.path().join("indexes"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .err()
        .expect("corrupt central directory must fail");
    assert!(matches!(error, ArchiveVfsError::Corrupt(_)));
}

#[test]
fn unsupported_compression_method_is_reported() {
    let fixture = Fixture::new(&[("file.txt", b"data", CompressionMethod::Stored)]);
    drop(fixture.index);
    patch_u16_after_signature(&fixture.archive, 0x0201_4b50, 10, 99);
    patch_u16_after_signature(&fixture.archive, 0x0403_4b50, 8, 99);
    let identity = ArchiveIdentity::from_path(&fixture.archive).expect("identify patched archive");
    let index = IndexStore::new(fixture.workspace.path().join("patched-index"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index unsupported method");
    let member = index
        .member(&[b"file.txt".to_vec()])
        .expect("lookup member");
    let mut output = Vec::new();
    let error = ZipBackend
        .extract(
            &mut File::open(&fixture.archive).expect("open archive"),
            &member,
            archive_vfs_helper::archive::ExtractionLimits {
                max_member_bytes: 1024,
                max_compression_ratio: 100.0,
            },
            &mut output,
        )
        .expect_err("unsupported compression must fail");
    assert!(matches!(
        error,
        ArchiveVfsError::UnsupportedCompression { method: 99, .. }
    ));
}

fn write_zip(path: &Path, entries: &[(&str, &[u8], CompressionMethod)]) {
    let file = File::create(path).expect("create ZIP fixture");
    let mut writer = ZipWriter::new(file);
    for (name, content, method) in entries {
        writer
            .start_file(
                *name,
                SimpleFileOptions::default().compression_method(*method),
            )
            .expect("start fixture entry");
        writer.write_all(content).expect("write fixture entry");
    }
    writer.finish().expect("finish ZIP fixture");
}

fn files_below(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(path) else {
        return files;
    };
    for entry in entries {
        let entry = entry.expect("read cache entry");
        if entry.file_type().expect("cache entry type").is_dir() {
            files.extend(files_below(&entry.path()));
        } else {
            files.push(entry.path());
        }
    }
    files
}

fn patch_first_signature(path: &Path, signature: u32, replacement: u32) {
    let mut bytes = fs::read(path).expect("read fixture for patching");
    let signature = signature.to_le_bytes();
    let offset = bytes
        .windows(signature.len())
        .position(|window| window == signature)
        .expect("find signature");
    bytes[offset..offset + 4].copy_from_slice(&replacement.to_le_bytes());
    fs::write(path, bytes).expect("write patched fixture");
}

fn patch_u16_after_signature(path: &Path, signature: u32, relative: usize, value: u16) {
    let mut file = File::options()
        .read(true)
        .write(true)
        .open(path)
        .expect("open fixture for patching");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("read fixture");
    let signature = signature.to_le_bytes();
    let offset = bytes
        .windows(signature.len())
        .position(|window| window == signature)
        .expect("find signature");
    file.seek(SeekFrom::Start(
        u64::try_from(offset + relative).expect("patch offset fits u64"),
    ))
    .expect("seek patch offset");
    file.write_all(&value.to_le_bytes()).expect("write patch");
}

fn replace_all(path: &Path, needle: &[u8], replacement: &[u8]) {
    assert_eq!(needle.len(), replacement.len());
    let mut bytes = fs::read(path).expect("read fixture for replacement");
    let mut offset = 0;
    while let Some(found) = bytes[offset..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let start = offset + found;
        bytes[start..start + needle.len()].copy_from_slice(replacement);
        offset = start + needle.len();
    }
    fs::write(path, bytes).expect("write replaced fixture");
}
