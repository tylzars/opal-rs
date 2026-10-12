mod daemon;
mod mains;
mod output;
mod platform;
mod settings;
mod stickers;
mod webcam;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Boot in low power mode and keep the camera's own 1080p USB video on until Ctrl-C (no effects)
    Webcam {
        /// Light flicker: off, 50, 60 or auto (by time zone)
        #[arg(long, default_value = "auto")]
        flicker: String,
    },
    /// Boot and keep the camera running, controlled with `ctl`
    Run {
        /// v4l2loopback device to write video to (Linux; Windows always uses the "Opal C1 Virtual" camera)
        #[arg(long)]
        output: Option<PathBuf>,
        /// Background blur radius in pixels of a 1080p picture, 0 for none [default: saved setting]
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

/// Camera files: `OPAL_RS_ASSETS`, else `assets/` in the source tree this was built from, else `assets/` next to
/// the program (as in a release).
fn asset_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OPAL_RS_ASSETS") {
        return dir.into();
    }
    let built = Path::new(ASSET_DIR);
    if built.is_dir() {
        return built.canonicalize().unwrap_or_else(|_| built.to_path_buf());
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.canonicalize().ok()?.parent()?.join("assets")))
        .unwrap_or_else(|| "assets".into())
}

static STOP: AtomicBool = AtomicBool::new(false);

/// Stops on Ctrl-C, or closing the console on Windows, or SIGTERM on Unix.
fn on_stop() -> Result<()> {
    ctrlc::set_handler(request_stop).context("stop handler")
}

/// Asks everything to stop, as `ctl quit` does.
fn request_stop() {
    STOP.store(true, Ordering::Relaxed);
}

/// True once a stop was asked for.
fn stopped() -> bool {
    STOP.load(Ordering::Relaxed)
}

/// True when every camera file is in `dir` and matches the manifest.
fn assets_ready(dir: &Path) -> bool {
    opal_assets::verify(dir).is_ok_and(|r| r.iter().all(|(_, s)| matches!(s, Status::Ok)))
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
        if start.elapsed() > timeout || stopped() {
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

/// Sleeps for `d`, or less if the service is told to stop.
fn sleep_unless_stopped(d: Duration) {
    let end = Instant::now() + d;
    while !stopped() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(250).min(end - Instant::now()));
    }
}

/// Time left before another boot is allowed.
fn boot_wait() -> Duration {
    std::fs::metadata(boot_stamp())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or(Duration::ZERO, |since| MIN_BOOT_INTERVAL.saturating_sub(since))
}

fn run_session(dir: &Path, output: Option<PathBuf>, settings: settings::Settings, overrides: &[String]) -> Result<()> {
    let low_power = settings.low_power();
    let booted = boot(dir, low_power)?;
    let dev = Device::connect(&booted, |m| eprintln!("[{}] {}", m.node_id_name, m.payload))?;
    let sensor = sensor_name(&dev)?;
    let info = device_info(&dev);
    let slow_usb = matches!(info.usb.as_str(), "low" | "full" | "high");
    // Low power mode streams 1080p, as the camera's own USB video has no other size.
    let resolution = if low_power {
        opal_c1::Resolution::P1080
    } else if slow_usb && settings.resolution() != opal_c1::Resolution::P720 {
        println!(
            "camera is on a USB {} port, using 720p; plug it into a USB 3 port for more",
            info.usb
        );
        opal_c1::Resolution::P720
    } else {
        settings.resolution()
    };
    println!(
        "sensor {sensor} {}{}",
        resolution.name(),
        if low_power { " low power" } else { "" }
    );
    daemon::run(
        dev, dir, &sensor, resolution, low_power, output, settings, overrides, info,
    )
}

fn device_info(dev: &Device) -> daemon::DeviceInfo {
    use opal_dai::msgpack::Value;
    let serial = match dev.call("getMxId", vec![]) {
        Ok(Value::Str(s)) => s,
        _ => "-".into(),
    };
    let usb = match dev.call("getUsbSpeed", vec![]) {
        Ok(Value::Int(n)) => n,
        Ok(Value::UInt(n)) => n as i64,
        _ => 0,
    };
    let usb = ["unknown", "low", "full", "high", "super", "super-plus"]
        .get(usb as usize)
        .unwrap_or(&"unknown");
    // The firmware file name ends in its commit.
    let name = firmware_asset().file_name.trim_end_matches(".cmd");
    let firmware = name.rsplit('-').next().unwrap_or(name);
    daemon::DeviceInfo {
        serial,
        usb: (*usb).into(),
        firmware: firmware[..firmware.len().min(8)].into(),
    }
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

/// Boots the camera. Its own USB video output is only turned on for low power mode.
fn boot(dir: &Path, usb_video: bool) -> Result<opal_xlink::Found> {
    let device = wait_for_camera(Duration::from_secs(20))?.context("no camera found")?;
    check_boot_interval()?;
    std::fs::write(boot_stamp(), b"")?;
    let firmware = opal_assets::load(dir, firmware_asset())?;
    let config = BoardConfig {
        uvc_enable: Some(usb_video),
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
            let booted = boot(&dir, false)?;
            println!("{:?} {:04x}", booted.state, booted.state.pid());
            Ok(ExitCode::SUCCESS)
        }
        Command::Info => {
            let booted = boot(&dir, false)?;
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
        Command::Webcam { flicker } => {
            webcam::run(&dir, &flicker)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run {
            output,
            blur,
            lut,
            settings,
        } => {
            on_stop()?;
            let settings_path = settings.unwrap_or_else(settings::default_path);
            let settings = settings::Settings::load(&settings_path);
            // Checked before the camera is touched: a boot that cannot go on only churns USB.
            if !assets_ready(&dir) {
                anyhow::bail!(
                    "camera files missing in {}: open the control panel, or run `opal-rs assets extract <disk image>`",
                    dir.display()
                );
            }
            if let Some(path) = output.as_ref().filter(|_| !settings.low_power()) {
                if !path.exists() {
                    eprintln!("waiting for {}; is v4l2loopback loaded?", path.display());
                }
                while !path.exists() && !stopped() {
                    std::thread::sleep(Duration::from_secs(1));
                }
                if stopped() {
                    return Ok(ExitCode::SUCCESS);
                }
            }
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
                    sleep_unless_stopped(boot_wait());
                    if !stopped() {
                        continue;
                    }
                }
                if stopped() {
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
                sleep_unless_stopped(boot_wait());
                if stopped() {
                    break;
                }
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
