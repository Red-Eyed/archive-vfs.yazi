use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::{ArchiveVfsError, Result, config::Config, protocol, service::ArchiveService};

#[derive(Debug, Parser)]
#[command(name = "archive-vfs-helper", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Probe {
        archive: PathBuf,
    },
    List {
        archive: PathBuf,
        inner: Option<PathBuf>,
    },
    Stat {
        archive: PathBuf,
        inner: Option<PathBuf>,
    },
    Read {
        archive: PathBuf,
        inner: PathBuf,
        #[arg(long)]
        offset: u64,
        #[arg(long)]
        len: u64,
    },
    Stream {
        archive: PathBuf,
        inner: PathBuf,
    },
    Copy {
        archive: PathBuf,
        inner: PathBuf,
        destination: PathBuf,
    },
    Lease {
        archive: PathBuf,
        inner: PathBuf,
    },
    CachePrune,
    CacheClean,
}

pub fn run() -> Result<u8> {
    let cli = Cli::parse();
    let config = Config::load()?;
    init_logging(&config);
    let service = ArchiveService::new(config);
    execute(&service, cli.command)
}

fn execute(service: &ArchiveService, command: Command) -> Result<u8> {
    match command {
        Command::Probe { archive } => Ok(u8::from(!service.recognizes(&archive)?)),
        Command::List { archive, inner } => {
            let (identity, nodes) = service.read_dir(&archive, &inner.unwrap_or_default())?;
            protocol::write_list(&mut io::stdout().lock(), &identity, &nodes)?;
            Ok(0)
        }
        Command::Stat { archive, inner } => {
            let (identity, node) = service.stat(&archive, &inner.unwrap_or_default())?;
            protocol::write_stat(&mut io::stdout().lock(), &identity, &node)?;
            Ok(0)
        }
        Command::Read {
            archive,
            inner,
            offset,
            len,
        } => {
            let lease = service.lease(&archive, &inner)?;
            let mut input = lease.open()?;
            input
                .seek(SeekFrom::Start(offset))
                .map_err(|source| ArchiveVfsError::io(lease.path(), source))?;
            io::copy(&mut input.take(len), &mut io::stdout().lock())
                .map_err(|source| ArchiveVfsError::io("protocol output", source))?;
            Ok(0)
        }
        Command::Stream { archive, inner } => {
            stream(service, &archive, &inner, &mut io::stdout().lock())?;
            Ok(0)
        }
        Command::Copy {
            archive,
            inner,
            destination,
        } => {
            copy(service, &archive, &inner, &destination)?;
            Ok(0)
        }
        Command::Lease { archive, inner } => {
            let lease = service.lease(&archive, &inner)?;
            protocol::write_path(&mut io::stdout().lock(), lease.path())?;
            io::copy(&mut io::stdin().lock(), &mut io::sink())
                .map_err(|source| ArchiveVfsError::io("lease control input", source))?;
            Ok(0)
        }
        Command::CachePrune => {
            println!("{}", service.prune_cache()?);
            Ok(0)
        }
        Command::CacheClean => {
            println!("{}", service.clean_partials()?);
            Ok(0)
        }
    }
}

fn stream(
    service: &ArchiveService,
    archive: &std::path::Path,
    inner: &std::path::Path,
    output: &mut impl Write,
) -> Result<u64> {
    let lease = service.lease(archive, inner)?;
    let mut input = lease.open()?;
    io::copy(&mut input, output).map_err(|source| ArchiveVfsError::io("stream output", source))
}

fn copy(
    service: &ArchiveService,
    archive: &std::path::Path,
    inner: &std::path::Path,
    destination: &std::path::Path,
) -> Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        ArchiveVfsError::InvalidVirtualPath("copy destination has no parent".to_owned())
    })?;
    fs::create_dir_all(parent).map_err(|source| ArchiveVfsError::io(parent, source))?;
    let temporary = destination.with_extension(format!("part-{}", std::process::id()));
    let result = (|| {
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| ArchiveVfsError::io(&temporary, source))?;
        stream(service, archive, inner, &mut output)?;
        output
            .sync_all()
            .map_err(|source| ArchiveVfsError::io(&temporary, source))?;
        fs::rename(&temporary, destination)
            .map_err(|source| ArchiveVfsError::io(destination, source))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn init_logging(config: &Config) {
    let filter = EnvFilter::builder()
        .with_default_directive(config.log_level.as_str().parse().expect("valid log level"))
        .from_env_lossy();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .try_init();
}
