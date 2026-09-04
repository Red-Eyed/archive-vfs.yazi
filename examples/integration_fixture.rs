use std::{env, fs::File, io::Write, path::PathBuf, process::ExitCode};

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5, 0x1c, 0x0c,
    0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc, 0xff, 0x1f, 0x00,
    0x03, 0x03, 0x02, 0x00, 0xef, 0xa3, 0x99, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

fn main() -> ExitCode {
    match generate() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("integration fixture: {error}");
            ExitCode::FAILURE
        }
    }
}

fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(env::args_os().nth(1).ok_or("expected output ZIP path")?);
    let mut writer = ZipWriter::new(File::create(path)?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, content) in [
        ("image.png", PNG_1X1),
        ("metadata.json", br#"{"dataset":"archive-vfs","items":1}"#),
        (
            "nested/readme.txt",
            b"Text preview from a nested directory.\n",
        ),
        (
            "nested/example.rs",
            b"fn main() { println!(\"archive-vfs\"); }\n",
        ),
    ] {
        writer.start_file(name, options)?;
        writer.write_all(content)?;
    }
    writer.finish()?;
    Ok(())
}
