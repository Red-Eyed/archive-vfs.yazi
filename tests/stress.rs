use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    time::Instant,
};

use archive_vfs_helper::{
    archive::{EntryKind, ZipBackend},
    config::Config,
    identity::ArchiveIdentity,
    index::IndexStore,
};
use tempfile::TempDir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[test]
fn indexes_one_hundred_thousand_direct_children() {
    let workspace = TempDir::new().expect("create stress workspace");
    let archive = workspace.path().join("100k.zip");
    let mut writer = ZipWriter::new(File::create(&archive).expect("create stress ZIP"));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for index in 0..100_000 {
        writer
            .start_file(format!("member-{index:06}.txt"), options)
            .expect("start stress member");
    }
    writer.finish().expect("finish stress ZIP");

    let started = Instant::now();
    let identity = ArchiveIdentity::from_path(&archive).expect("identify stress archive");
    let index = IndexStore::new(workspace.path().join("indexes"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index stress archive");
    let indexing = started.elapsed();
    let listing_started = Instant::now();
    let children = index.read_dir(&[]).expect("list stress archive");
    let listing = listing_started.elapsed();

    assert_eq!(children.len(), 100_000);
    assert_eq!(children[0].kind, EntryKind::File);
    assert_eq!(children[0].name, b"member-000000.txt");
    assert_eq!(children[99_999].name, b"member-099999.txt");
    eprintln!("100k indexing={indexing:?}, listing={listing:?}");
}

#[test]
fn indexes_sparse_zip64_with_central_directory_beyond_four_gibibytes() {
    let workspace = TempDir::new().expect("create sparse ZIP64 workspace");
    let archive = workspace.path().join("sparse-zip64.zip");
    let mut file = File::create(&archive).expect("create sparse ZIP64 fixture");
    let name = b"tiny.txt";
    let payload = b"tiny";
    let crc = crc32fast::hash(payload);
    write_local_entry(&mut file, name, payload, crc);

    let central_offset = u64::from(u32::MAX) + 4096;
    file.seek(SeekFrom::Start(central_offset))
        .expect("seek sparse central directory");
    write_central_entry(&mut file, name, payload.len(), crc);
    let central_size = 46 + u64::try_from(name.len()).expect("name length fits u64");
    let zip64_eocd_offset = central_offset + central_size;
    write_zip64_end_records(&mut file, zip64_eocd_offset, central_offset, central_size);
    file.sync_all().expect("sync sparse ZIP64 fixture");

    let identity = ArchiveIdentity::from_path(&archive).expect("identify sparse ZIP64 archive");
    let index = IndexStore::new(workspace.path().join("indexes"))
        .ensure(&identity, &ZipBackend, Config::default().filename_policy)
        .expect("index sparse ZIP64 archive");
    let member = index
        .member(&[name.to_vec()])
        .expect("lookup sparse ZIP64 member");
    assert_eq!(member.uncompressed_size, 4);
    assert_eq!(member.local_header_offset, 0);
}

fn write_local_entry(file: &mut File, name: &[u8], payload: &[u8], crc: u32) {
    write_u32(file, 0x0403_4b50);
    write_u16(file, 20);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u32(file, crc);
    write_u32(
        file,
        u32::try_from(payload.len()).expect("payload length fits u32"),
    );
    write_u32(
        file,
        u32::try_from(payload.len()).expect("payload length fits u32"),
    );
    write_u16(
        file,
        u16::try_from(name.len()).expect("name length fits u16"),
    );
    write_u16(file, 0);
    file.write_all(name).expect("write local name");
    file.write_all(payload).expect("write local payload");
}

fn write_central_entry(file: &mut File, name: &[u8], payload_len: usize, crc: u32) {
    write_u32(file, 0x0201_4b50);
    write_u16(file, 45);
    write_u16(file, 20);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u32(file, crc);
    write_u32(
        file,
        u32::try_from(payload_len).expect("payload length fits u32"),
    );
    write_u32(
        file,
        u32::try_from(payload_len).expect("payload length fits u32"),
    );
    write_u16(
        file,
        u16::try_from(name.len()).expect("name length fits u16"),
    );
    write_u16(file, 0);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u32(file, 0);
    write_u32(file, 0);
    file.write_all(name).expect("write central name");
}

fn write_zip64_end_records(file: &mut File, eocd_offset: u64, directory_offset: u64, size: u64) {
    write_u32(file, 0x0606_4b50);
    write_u64(file, 44);
    write_u16(file, 45);
    write_u16(file, 45);
    write_u32(file, 0);
    write_u32(file, 0);
    write_u64(file, 1);
    write_u64(file, 1);
    write_u64(file, size);
    write_u64(file, directory_offset);

    write_u32(file, 0x0706_4b50);
    write_u32(file, 0);
    write_u64(file, eocd_offset);
    write_u32(file, 1);

    write_u32(file, 0x0605_4b50);
    write_u16(file, 0);
    write_u16(file, 0);
    write_u16(file, u16::MAX);
    write_u16(file, u16::MAX);
    write_u32(file, u32::MAX);
    write_u32(file, u32::MAX);
    write_u16(file, 0);
}

fn write_u16(file: &mut File, value: u16) {
    file.write_all(&value.to_le_bytes()).expect("write u16");
}

fn write_u32(file: &mut File, value: u32) {
    file.write_all(&value.to_le_bytes()).expect("write u32");
}

fn write_u64(file: &mut File, value: u64) {
    file.write_all(&value.to_le_bytes()).expect("write u64");
}
