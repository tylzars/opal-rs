use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use opal_assets::{Event, RELEASES, Status};

/// `assets/` at the repository root.
const ASSET_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

#[derive(Parser)]
#[command(version, about = "Opal C1 tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage camera files
    Assets {
        #[command(subcommand)]
        command: AssetsCommand,
    },
    /// List attached cameras
    List,
}

#[derive(Subcommand)]
enum AssetsCommand {
    /// Extract camera files from an Opal Composer .dmg
    Extract {
        /// Disk image [default: assets/<release>.dmg]
        dmg: Option<PathBuf>,
    },
    /// Check extracted files
    Verify,
}

fn asset_dir() -> PathBuf {
    let dir = Path::new(ASSET_DIR);
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let dir = asset_dir();
    match cli.command {
        Command::Assets { command } => match command {
            AssetsCommand::Extract { dmg } => {
                let dmg = dmg.unwrap_or_else(|| dir.join(RELEASES[0].dmg_name));
                opal_assets::extract(&dmg, &dir, |event| match event {
                    Event::Identified(Some(release)) => println!("{}", release.label),
                    Event::Identified(None) => println!("unknown disk image"),
                    Event::Wrote(asset, _) => println!("{}", asset.file_name),
                })
                .with_context(|| dmg.display().to_string())?;
                Ok(ExitCode::SUCCESS)
            }
            AssetsCommand::Verify => {
                let mut ok = true;
                for (asset, status) in opal_assets::verify(&dir)? {
                    match status {
                        Status::Ok => println!("ok      {}", asset.file_name),
                        Status::Missing => {
                            ok = false;
                            println!("missing {}", asset.file_name);
                        }
                        Status::Mismatch(_) => {
                            ok = false;
                            println!("bad     {}", asset.file_name);
                        }
                    }
                }
                Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE })
            }
        },
        Command::List => {
            for d in opal_xlink::list()? {
                println!("{:?} {:04x} {}", d.state, d.state.pid(), d.serial().unwrap_or("-"));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
