use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use opal_assets::{Event, Status};

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
}

#[derive(Subcommand)]
enum AssetsCommand {
    /// Extract camera files from an Opal Composer .dmg
    Extract {
        dmg: PathBuf,
        /// Output directory
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Check extracted files
    Verify {
        /// Asset directory
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Print the default asset directory
    Path,
}

fn default_asset_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().context("no data directory")?;
    Ok(base.join("opal-rs").join("assets"))
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Command::Assets { command } => match command {
            AssetsCommand::Extract { dmg, dir } => {
                let dir = dir.map_or_else(default_asset_dir, Ok)?;
                opal_assets::extract(&dmg, &dir, |event| match event {
                    Event::Identified(Some(release)) => println!("{}", release.label),
                    Event::Identified(None) => println!("unknown disk image"),
                    Event::Wrote(_, path) => println!("{}", path.display()),
                })
                .with_context(|| dmg.display().to_string())?;
                Ok(ExitCode::SUCCESS)
            }
            AssetsCommand::Verify { dir } => {
                let dir = dir.map_or_else(default_asset_dir, Ok)?;
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
            AssetsCommand::Path => {
                println!("{}", default_asset_dir()?.display());
                Ok(ExitCode::SUCCESS)
            }
        },
    }
}
