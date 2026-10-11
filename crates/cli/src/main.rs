use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use opal_assets::{Asset, Event, Kind, RELEASES, Status};
use opal_c1::Led;
use opal_dai::board::BoardConfig;
use opal_dai::boot::Step;
use opal_dai::device::Device;
use opal_dai::message::{AudioInConfig, CameraControl, af, awb};

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
    /// Boot and keep the camera running
    Run,
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

fn boot(dir: &Path) -> Result<opal_xlink::Found> {
    let device = wait_for_camera(Duration::from_secs(20))?.context("no camera found")?;
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
        Command::Run => {
            let booted = boot(&dir)?;
            let dev = Device::connect(&booted, |m| eprintln!("[{}] {}", m.node_id_name, m.payload))?;
            let sensor = sensor_name(&dev)?;
            println!("sensor {sensor}");
            let streams = dev.start_pipeline(&opal_c1::pipeline(&dir, &sensor)?)?;
            let control = streams[opal_c1::CONTROL_STREAM];
            let mic = streams[opal_c1::MIC_STREAM];
            let led = streams[opal_c1::LED_STREAM];
            println!("running");
            for line in std::io::stdin().lines() {
                let line = line?;
                let mut words = line.split_whitespace();
                match (words.next(), words.next().and_then(|w| w.parse::<f32>().ok())) {
                    (Some("quit"), _) => break,
                    (Some("gain"), Some(db)) => dev.link().write(mic, &AudioInConfig::gain_db(db).to_packet())?,
                    (Some("led"), _) => match line.split_whitespace().nth(1) {
                        Some("off") => dev.link().write(led, &Led::Off.packet())?,
                        Some("white") => dev.link().write(led, &Led::White.packet())?,
                        Some("blue") => dev.link().write(led, &Led::Blue.packet())?,
                        _ => println!("?"),
                    },
                    _ => match parse_control(&line) {
                        Some(c) => dev.link().write(control, &c.to_packet())?,
                        None => println!("?"),
                    },
                }
            }
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

/// One control command per line, e.g. `focus 120` or `exposure 10000 400`.
fn parse_control(line: &str) -> Option<CameraControl> {
    let mut words = line.split_whitespace();
    let cmd = words.next()?;
    let args: Vec<i64> = words.map(|w| w.parse().ok()).collect::<Option<_>>()?;
    let mut c = CameraControl::default();
    match (cmd, args.as_slice()) {
        ("focus", [p]) => c.auto_focus_mode(af::OFF).manual_focus(*p as u8),
        ("af", []) => c.auto_focus_mode(af::CONTINUOUS_VIDEO),
        ("exposure", [us, iso]) => c.manual_exposure(*us as u32, *iso as u32),
        ("ae", []) => c.auto_exposure(),
        ("ev", [v]) => c.exposure_compensation(*v as i8),
        ("wb", [k]) => c.white_balance(*k as u16),
        ("awb", []) => c.auto_white_balance(awb::AUTO),
        ("brightness", [v]) => c.brightness(*v as i8),
        ("contrast", [v]) => c.contrast(*v as i8),
        ("saturation", [v]) => c.saturation(*v as i8),
        ("sharpness", [v]) => c.sharpness(*v as u8),
        _ => return None,
    };
    Some(c)
}
