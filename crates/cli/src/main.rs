mod daemon;
mod settings;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
    /// Boot and keep the camera running, controlled with `ctl`
    Run {
        /// v4l2loopback device to write video to
        #[arg(long)]
        output: Option<PathBuf>,
        /// Background blur radius in pixels, 0 for none [default: saved setting]
        #[arg(long)]
        blur: Option<usize>,
        /// Colour lookup table (.cube) to apply [default: saved setting]
        #[arg(long)]
        lut: Option<PathBuf>,
        /// Settings file [default: ~/.config/opal-rs/settings]
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Send a command to a running camera, e.g. `ctl status` or `ctl focus 120`
    Ctl {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
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

fn wait_for_camera(timeout: Duration) -> Result<Option<opal_xlink::Found>> {
    let start = Instant::now();
    loop {
        if let Some(d) = opal_xlink::list()?.into_iter().next() {
            return Ok(Some(d));
        }
        if start.elapsed() > timeout {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Each boot makes the camera drop off USB and come back a few times, so boots are spaced out.
const MIN_BOOT_INTERVAL: Duration = Duration::from_secs(60);

fn boot_stamp() -> PathBuf {
    std::env::temp_dir().join("opal-rs-last-boot")
}

/// Restarts after the camera goes away are limited, so a camera that keeps failing is left alone.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(600);

/// Time left before another boot is allowed.
fn boot_wait() -> Duration {
    std::fs::metadata(boot_stamp())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or(Duration::ZERO, |since| MIN_BOOT_INTERVAL.saturating_sub(since))
}

fn run_session(dir: &Path, output: Option<PathBuf>, settings: settings::Settings, overrides: &[String]) -> Result<()> {
    let booted = boot(dir)?;
    let dev = Device::connect(&booted, |m| eprintln!("[{}] {}", m.node_id_name, m.payload))?;
    let sensor = sensor_name(&dev)?;
    let low_power = settings.low_power();
    // Low power mode streams 1080p, as the camera's own USB video has no other size.
    let resolution = if low_power {
        opal_c1::Resolution::P1080
    } else {
        settings.resolution()
    };
    println!(
        "sensor {sensor} {}{}",
        resolution.name(),
        if low_power { " low power" } else { "" }
    );
    daemon::run(dev, dir, &sensor, resolution, low_power, output, settings, overrides)
}

fn check_boot_interval() -> Result<()> {
    let Ok(modified) = std::fs::metadata(boot_stamp()).and_then(|m| m.modified()) else {
        return Ok(());
    };
    let since = modified.elapsed().unwrap_or_default();
    if since < MIN_BOOT_INTERVAL {
        anyhow::bail!(
            "camera was booted {}s ago, wait {}s",
            since.as_secs(),
            (MIN_BOOT_INTERVAL - since).as_secs()
        );
    }
    Ok(())
}

fn boot(dir: &Path) -> Result<opal_xlink::Found> {
    let device = wait_for_camera(Duration::from_secs(20))?.context("no camera found")?;
    check_boot_interval()?;
    std::fs::write(boot_stamp(), b"")?;
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
        Command::Run {
            output,
            blur,
            lut,
            settings,
        } => {
            let settings_path = settings.unwrap_or_else(settings::default_path);
            let settings = settings::Settings::load(&settings_path);
            let mut overrides = Vec::new();
            if let Some(r) = blur {
                overrides.push(format!("blur {r}"));
            }
            if let Some(p) = lut {
                overrides.push(format!("lut {}", p.display()));
            }
            let mut settings = Some(settings);
            let mut restarts: Vec<Instant> = Vec::new();
            loop {
                let settings = match settings.take() {
                    Some(s) => s,
                    None => settings::Settings::load(&settings_path),
                };
                let result = run_session(&dir, output.clone(), settings, &overrides);
                if daemon::take_restart() && result.is_ok() {
                    std::thread::sleep(boot_wait());
                    continue;
                }
                if daemon::stopped() {
                    result?;
                    break;
                }
                let err = result.err().unwrap_or_else(|| anyhow::anyhow!("session ended"));
                restarts.retain(|t| t.elapsed() < RESTART_WINDOW);
                if restarts.len() >= MAX_RESTARTS {
                    return Err(err.context("too many restarts"));
                }
                restarts.push(Instant::now());
                eprintln!("{err:#}; waiting for the camera");
                wait_for_camera(Duration::MAX)?;
                std::thread::sleep(boot_wait());
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Ctl { command } => Ok(if daemon::send(&command)? {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }),
        Command::List => {
            for d in opal_xlink::list()? {
                println!("{:?} {:04x} {}", d.state, d.state.pid(), d.serial().unwrap_or("-"));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn sensor_name(dev: &Device) -> Result<String> {
    let names = dev.call("getCameraSensorNames", vec![])?;
    let first = names
        .as_array()
        .and_then(|a| a.first())
        .and_then(|p| p.as_array())
        .context("no camera sensor")?;
    Ok(first
        .get(1)
        .and_then(|s| s.as_str())
        .context("no camera sensor")?
        .to_owned())
}
