use std::{fs, io::Write, path::Path};

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[test]
fn probe_list_offset_read_and_copy_work_end_to_end() {
    let workspace = TempDir::new().expect("create CLI workspace");
    let archive = workspace.path().join("dataset [one].zip");
    write_zip(&archive, "nested/data.json", br#"{"answer":42}"#);
    let config = write_config(workspace.path());

    cargo_bin_cmd!("archive-vfs-helper")
        .env("ARCHIVE_VFS_CONFIG", &config)
        .arg("probe")
        .arg(&archive)
        .assert()
        .success();

    let list = cargo_bin_cmd!("archive-vfs-helper")
        .env("ARCHIVE_VFS_CONFIG", &config)
        .arg("list")
        .arg(&archive)
        .output()
        .expect("run list command");
    assert!(
        list.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(&list.stdout[..8], b"AVFSL1\0\0");
    assert_eq!(
        u64::from_le_bytes(list.stdout[8..16].try_into().expect("count bytes")),
        1
    );

    let read = cargo_bin_cmd!("archive-vfs-helper")
        .env("ARCHIVE_VFS_CONFIG", &config)
        .args(["read"])
        .arg(&archive)
        .arg("nested/data.json")
        .args(["--offset", "10", "--len", "2"])
        .output()
        .expect("run offset read");
    assert!(read.status.success());
    assert_eq!(read.stdout, b"42");

    let destination = workspace.path().join("copy/out.json");
    cargo_bin_cmd!("archive-vfs-helper")
        .env("ARCHIVE_VFS_CONFIG", &config)
        .arg("copy")
        .arg(&archive)
        .arg("nested/data.json")
        .arg(&destination)
        .assert()
        .success();
    assert_eq!(
        fs::read(destination).expect("read copied member"),
        br#"{"answer":42}"#
    );
}

fn write_zip(path: &Path, name: &str, content: &[u8]) {
    let mut writer = ZipWriter::new(fs::File::create(path).expect("create ZIP fixture"));
    writer
        .start_file(
            name,
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .expect("start ZIP member");
    writer.write_all(content).expect("write ZIP member");
    writer.finish().expect("finish ZIP fixture");
}

fn write_config(workspace: &Path) -> std::path::PathBuf {
    let path = workspace.join("archive-vfs.toml");
    let cache = workspace.join("cache");
    let indexes = workspace.join("indexes");
    fs::write(
        &path,
        format!(
            "cache_dir = \"{}\"\nindex_dir = \"{}\"\nlog_level = \"off\"\n",
            cache.display(),
            indexes.display()
        ),
    )
    .expect("write helper config");
    path
}
