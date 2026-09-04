fn main() {
    match archive_vfs_helper::cli::run() {
        Ok(code) => std::process::exit(i32::from(code)),
        Err(error) => {
            eprintln!("archive-vfs-helper: {error}");
            std::process::exit(2);
        }
    }
}
