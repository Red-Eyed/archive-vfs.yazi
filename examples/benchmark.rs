use std::{
    env,
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{Duration, Instant},
};

use archive_vfs_helper::{
    archive::ZipBackend, cache::MemberCache, config::Config, identity::ArchiveIdentity,
    index::IndexStore,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const PREVIEW_COUNT: usize = 100;
const IMAGE_BYTES: usize = 64 * 1024;
const SPARSE_OFFSET: u64 = 100 * 1024 * 1024 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("benchmark: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let command = args.next().ok_or("expected generate or measure")?;
    let workspace = PathBuf::from(args.next().ok_or("expected workspace path")?);
    match command.to_str() {
        Some("generate") => {
            let count = args
                .next()
                .ok_or("expected entry count")?
                .to_str()
                .ok_or("entry count is not UTF-8")?
                .parse()?;
            generate(&workspace, count)
        }
        Some("measure") => measure(&workspace),
        _ => Err("expected generate or measure".into()),
    }
}

fn generate(workspace: &Path, entry_count: usize) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(workspace)?;
    let archive = workspace.join("entries.zip");
    let mut writer = ZipWriter::new(File::create(&archive)?);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for index in 0..entry_count {
        writer.start_file(format!("entry-{index:09}.json"), stored)?;
    }
    let payload = image_payload();
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for index in 0..PREVIEW_COUNT {
        writer.start_file(format!("images/image-{index:03}.jpg"), deflated)?;
        writer.write_all(&payload)?;
    }
    writer.finish()?;
    write_sparse_zip64(&workspace.join("sparse-100g.zip"))?;
    Ok(())
}

fn measure(workspace: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let archive = workspace.join("entries.zip");
    let identity = ArchiveIdentity::from_path(&archive)?;
    let index_root = workspace.join("indexes");
    let started = Instant::now();
    let index = IndexStore::new(index_root.clone()).ensure(
        &identity,
        &ZipBackend,
        Config::default().filename_policy,
    )?;
    report("initial_index_ms", started.elapsed());

    let materialized = index.read_dir(&[])?;
    println!("direct_children={}", materialized.len());
    if let Some(resident_kib) = resident_kib() {
        println!("resident_with_listing_kib={resident_kib}");
    }
    drop(materialized);

    let started = Instant::now();
    for _ in 0..100 {
        let entries = index.read_dir(&[])?;
        std::hint::black_box(entries);
    }
    report_average("cached_root_listing_ms", started.elapsed(), 100);

    let image_paths = (0..PREVIEW_COUNT)
        .map(|index| {
            vec![
                b"images".to_vec(),
                format!("image-{index:03}.jpg").into_bytes(),
            ]
        })
        .collect::<Vec<_>>();
    let members = image_paths
        .iter()
        .map(|path| index.member(path))
        .collect::<Result<Vec<_>, _>>()?;

    let first_cache = cache(workspace.join("cache-first"), index_root.clone());
    let started = Instant::now();
    drop(first_cache.lease(&identity, &ZipBackend, &members[0])?);
    report("first_image_preview_ms", started.elapsed());

    let started = Instant::now();
    for _ in 0..100 {
        drop(first_cache.lease(&identity, &ZipBackend, &members[0])?);
    }
    report_average("repeated_image_preview_ms", started.elapsed(), 100);

    let sequential_cache = cache(workspace.join("cache-sequential"), index_root.clone());
    let started = Instant::now();
    for member in &members {
        drop(sequential_cache.lease(&identity, &ZipBackend, member)?);
    }
    report("sequential_100_images_ms", started.elapsed());

    let random_cache = cache(workspace.join("cache-random"), index_root);
    let started = Instant::now();
    for index in (0..PREVIEW_COUNT).map(|value| value * 37 % PREVIEW_COUNT) {
        drop(random_cache.lease(&identity, &ZipBackend, &members[index])?);
    }
    report("random_100_images_ms", started.elapsed());

    let sparse = workspace.join("sparse-100g.zip");
    let sparse_identity = ArchiveIdentity::from_path(&sparse)?;
    let started = Instant::now();
    let sparse_index = IndexStore::new(workspace.join("sparse-index")).ensure(
        &sparse_identity,
        &ZipBackend,
        Config::default().filename_policy,
    )?;
    std::hint::black_box(sparse_index.read_dir(&[])?);
    report("sparse_100g_index_and_list_ms", started.elapsed());
    println!("logical_sparse_bytes={}", sparse_identity.size);
    Ok(())
}

fn cache(cache_dir: PathBuf, index_dir: PathBuf) -> MemberCache {
    MemberCache::from_config(&Config {
        cache_dir,
        index_dir,
        max_cache_bytes: 512 * 1024 * 1024,
        log_level: archive_vfs_helper::config::LogLevel::Off,
        ..Config::default()
    })
}

fn image_payload() -> Vec<u8> {
    let mut state = 0x9e37_79b9_u32;
    (0..IMAGE_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn report(name: &str, elapsed: Duration) {
    println!("{name}={:.3}", elapsed.as_secs_f64() * 1000.0);
}

fn report_average(name: &str, elapsed: Duration, iterations: u32) {
    println!(
        "{name}={:.3}",
        elapsed.as_secs_f64() * 1000.0 / f64::from(iterations)
    );
}

fn resident_kib() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p"])
        .arg(std::process::id().to_string())
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_sparse_zip64(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = File::create(path)?;
    let name = b"tiny.txt";
    let payload = b"tiny";
    let crc = crc32fast::hash(payload);
    write_local_entry(&mut file, name, payload, crc)?;
    file.seek(SeekFrom::Start(SPARSE_OFFSET))?;
    write_central_entry(&mut file, name, payload.len(), crc)?;
    let central_size = 46 + u64::try_from(name.len())?;
    write_zip64_end_records(&mut file, SPARSE_OFFSET + central_size, central_size)?;
    file.sync_all()?;
    Ok(())
}

fn write_local_entry(
    file: &mut File,
    name: &[u8],
    payload: &[u8],
    crc: u32,
) -> std::io::Result<()> {
    let mut header = Vec::with_capacity(30);
    push_u32(&mut header, 0x0403_4b50);
    push_u16(&mut header, 20);
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u32(&mut header, crc);
    push_u32(
        &mut header,
        u32::try_from(payload.len()).unwrap_or(u32::MAX),
    );
    push_u32(
        &mut header,
        u32::try_from(payload.len()).unwrap_or(u32::MAX),
    );
    push_u16(&mut header, u16::try_from(name.len()).unwrap_or(u16::MAX));
    push_u16(&mut header, 0);
    file.write_all(&header)?;
    file.write_all(name)?;
    file.write_all(payload)
}

fn write_central_entry(
    file: &mut File,
    name: &[u8],
    payload_len: usize,
    crc: u32,
) -> std::io::Result<()> {
    let mut header = Vec::with_capacity(46);
    push_u32(&mut header, 0x0201_4b50);
    push_u16(&mut header, 45);
    push_u16(&mut header, 20);
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u32(&mut header, crc);
    push_u32(&mut header, u32::try_from(payload_len).unwrap_or(u32::MAX));
    push_u32(&mut header, u32::try_from(payload_len).unwrap_or(u32::MAX));
    push_u16(&mut header, u16::try_from(name.len()).unwrap_or(u16::MAX));
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u16(&mut header, 0);
    push_u32(&mut header, 0);
    push_u32(&mut header, 0);
    file.write_all(&header)?;
    file.write_all(name)
}

fn write_zip64_end_records(
    file: &mut File,
    zip64_offset: u64,
    central_size: u64,
) -> std::io::Result<()> {
    let mut records = Vec::with_capacity(98);
    push_u32(&mut records, 0x0606_4b50);
    push_u64(&mut records, 44);
    push_u16(&mut records, 45);
    push_u16(&mut records, 45);
    push_u32(&mut records, 0);
    push_u32(&mut records, 0);
    push_u64(&mut records, 1);
    push_u64(&mut records, 1);
    push_u64(&mut records, central_size);
    push_u64(&mut records, SPARSE_OFFSET);
    push_u32(&mut records, 0x0706_4b50);
    push_u32(&mut records, 0);
    push_u64(&mut records, zip64_offset);
    push_u32(&mut records, 1);
    push_u32(&mut records, 0x0605_4b50);
    push_u16(&mut records, 0);
    push_u16(&mut records, 0);
    push_u16(&mut records, u16::MAX);
    push_u16(&mut records, u16::MAX);
    push_u32(&mut records, u32::MAX);
    push_u32(&mut records, u32::MAX);
    push_u16(&mut records, 0);
    file.write_all(&records)
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}
