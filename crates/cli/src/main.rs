use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use opal_assets::{Asset, Event, Kind, RELEASES, Status};
use opal_dai::board::BoardConfig;
use opal_dai::boot::Step;
use opal_dai::device::Device;

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
    /// Boot the camera firmware into RAM
    Boot,
    /// Boot and print device information
    Info,
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

fn firmware_asset() -> &'static Asset {
    opal_assets::ASSETS
        .iter()
        .find(|a| a.kind == Kind::DeviceFirmware)
        .expect("manifest has firmware")
}

fn boot(dir: &Path) -> Result<opal_xlink::Found> {
    let device = opal_xlink::list()?.into_iter().next().context("no camera found")?;
    let firmware = opal_assets::load(dir, firmware_asset())?;
    let config = BoardConfig {
        uvc_enable: Some(true),
        ..Default::default()
    };
    Ok(opal_dai::boot::boot(&device, &config, &firmware, |step| match step {
        Step::Rebooting => println!("rebooting to bootloader"),
        Step::Bootloader(v, kind) => println!("bootloader {v} type {kind}"),
        Step::Sending(n) => println!("sending {n} bytes"),
        Step::Booted => println!("booted"),
    })?)
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
        Command::Boot => {
            let booted = boot(&dir)?;
            println!("{:?} {:04x}", booted.state, booted.state.pid());
            Ok(ExitCode::SUCCESS)
        }
        Command::Info => {
            let booted = boot(&dir)?;
            let dev = Device::connect(&booted, |m| eprintln!("[{}] {}", m.node_id_name, m.payload))?;
            for method in [
                "getMxId",
                "getCameraSensorNames",
                "getConnectedCameras",
                "getUsbSpeed",
                "getChipTemperature",
            ] {
                match dev.call(method, vec![]) {
                    Ok(v) => println!("{method}: {v:?}"),
                    Err(e) => println!("{method}: {e}"),
                }
            }
            dev.close();
            Ok(ExitCode::SUCCESS)
        }
        Command::List => {
            for d in opal_xlink::list()? {
                println!("{:?} {:04x} {}", d.state, d.state.pid(), d.serial().unwrap_or("-"));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
