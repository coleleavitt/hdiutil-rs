use clap::{Parser, Subcommand};
use hdiutil::{AttachOptions, CreateOptions, Fs, error::Result};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "hdiutil", about = "Minimal hdiutil for Linux (HFS+ images)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mount an HFS+ image (needs root)
    Attach {
        image: PathBuf,
        #[arg(long)]
        mountpoint: PathBuf,
        #[arg(long)]
        readonly: bool,
    },
    /// Unmount a mounted image (needs root)
    Detach {
        mountpoint: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Create an empty HFS+ image
    Create {
        path: PathBuf,
        #[arg(long)]
        size: String,
        #[arg(long, default_value = "HFS+")]
        fs: String,
        #[arg(long, default_value = "untitled")]
        volname: String,
        #[arg(long)]
        ov: bool,
    },
    /// Expand a UDIF image to a raw disk image
    Convert { image: PathBuf, #[arg(short, long)] output: PathBuf },
    /// Print the chunk table of a UDIF image
    Info { image: PathBuf },
}

/// Accept hdiutil-style single-dash options (`-mountpoint`, `-size`, ...).
fn normalize_args() -> Vec<String> {
    const LONG: &[&str] = &["mountpoint", "size", "fs", "volname", "ov", "readonly", "force"];
    std::env::args()
        .map(|a| match a.strip_prefix('-') {
            Some(rest) if LONG.contains(&rest) => format!("--{rest}"),
            _ => a,
        })
        .collect()
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Attach { image, mountpoint, readonly } => {
            let a = hdiutil::attach(&image, &mountpoint, &AttachOptions { readonly })?;
            println!("{}\t{}", a.loop_device.display(), a.mountpoint.display());
        }
        Cmd::Detach { mountpoint, force } => hdiutil::detach(&mountpoint, force)?,
        Cmd::Create { path, size, fs, volname, ov } => {
            let opts = CreateOptions { size: hdiutil::parse_size(&size)?, fs: fs.parse::<Fs>()?, volname, overwrite: ov };
            println!("created: {}", hdiutil::create(&path, &opts)?.display());
        }
        Cmd::Convert { image, output } => hdiutil::convert_to_raw(&image, &output)?,
        Cmd::Info { image } => {
            let u = hdiutil::udif::Udif::open(&image)?;
            println!("sectors: {}  data fork: {}+{}", u.sector_count, u.data_fork_offset, u.data_fork_length);
            for c in &u.chunks {
                println!("{:?}\tsector {}\tcount {}\toffset {}\tlen {}", c.kind, c.sector, c.sectors, c.offset, c.length);
            }
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run(Cli::parse_from(normalize_args())) {
        eprintln!("hdiutil: {e}");
        std::process::exit(1);
    }
}
