use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use opal_assets::{Asset, Event, Kind, RELEASES, Status};
use opal_c1::Led;
use opal_dai::board::BoardConfig;
use opal_dai::boot::Step;
use opal_dai::device::Device;
use opal_dai::message::{self, AudioInConfig, CameraControl, IMG_FRAME, af, awb};

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
    Run {
        /// v4l2loopback device to write video to
        #[arg(long)]
        output: Option<PathBuf>,
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
        Command::Run { output } => {
            let booted = boot(&dir)?;
            let dev = Device::connect(&booted, |m| eprintln!("[{}] {}", m.node_id_name, m.payload))?;
            let sensor = sensor_name(&dev)?;
            println!("sensor {sensor}");
            let streams = dev.start_pipeline(&opal_c1::pipeline(&dir, &sensor)?)?;
            let control = streams[opal_c1::CONTROL_STREAM];
            let mic = streams[opal_c1::MIC_STREAM];
            let led = streams[opal_c1::LED_STREAM];
            let video = Arc::new(Video::default());
            {
                let (link, video) = (dev.link().clone(), video.clone());
                let mut out = match &output {
                    Some(path) => Some(
                        opal_v4l2::Output::open(path, opal_c1::VIDEO_WIDTH, opal_c1::VIDEO_HEIGHT, opal_v4l2::NV12)
                            .with_context(|| path.display().to_string())?,
                    ),
                    None => None,
                };
                std::thread::spawn(move || {
                    while let Ok(packet) = link.read(opal_c1::VIDEO_STREAM, None) {
                        let Some((data, IMG_FRAME, _)) = message::split(&packet) else {
                            continue;
                        };
                        if let Some(out) = &mut out
                            && let Err(e) = out.write(data)
                        {
                            eprintln!("output: {e}");
                        }
                        video.frames.fetch_add(1, Ordering::Relaxed);
                        *video.last.lock().unwrap() = data.to_vec();
                    }
                });
            }
            let link = dev.link();
            let set_streaming = |on: bool| -> Result<()> {
                if on {
                    link.write(control, &opal_c1::start_control().to_packet())?;
                    link.write(led, &Led::White.packet())?;
                } else {
                    link.write(control, &opal_c1::stop_control().to_packet())?;
                    link.write(led, &Led::Off.packet())?;
                }
                Ok(())
            };
            set_streaming(true)?;
            let mut streaming = true;
            println!("running");
            for line in std::io::stdin().lines() {
                let line = line?;
                let mut words = line.split_whitespace();
                let result = match (words.next(), words.next()) {
                    (Some("quit"), _) => break,
                    (Some(cmd @ ("on" | "off")), None) => {
                        let on = cmd == "on";
                        let r = if on != streaming { set_streaming(on) } else { Ok(()) };
                        streaming = on;
                        r
                    }
                    (Some("frames"), None) => {
                        println!("frames {}", video.frames.load(Ordering::Relaxed));
                        Ok(())
                    }
                    (Some("snap"), Some(path)) => video.snap(Path::new(path)),
                    (Some("gain"), Some(db)) => match db.parse::<f32>() {
                        Ok(db) => link
                            .write(mic, &AudioInConfig::gain_db(db).to_packet())
                            .map_err(Into::into),
                        Err(e) => Err(e.into()),
                    },
                    (Some("led"), Some(color)) => match color {
                        "off" => link.write(led, &Led::Off.packet()).map_err(Into::into),
                        "white" => link.write(led, &Led::White.packet()).map_err(Into::into),
                        "blue" => link.write(led, &Led::Blue.packet()).map_err(Into::into),
                        _ => Err(anyhow::anyhow!("unknown colour")),
                    },
                    _ => match parse_control(&line) {
                        Some(c) => link.write(control, &c.to_packet()).map_err(Into::into),
                        None => Err(anyhow::anyhow!("unknown command")),
                    },
                };
                if let Err(e) = result {
                    println!("error: {e}");
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

/// Latest video frame from the camera.
#[derive(Default)]
struct Video {
    frames: AtomicU64,
    last: Mutex<Vec<u8>>,
}

impl Video {
    /// Writes the luma plane of the latest frame as a PGM image.
    fn snap(&self, path: &Path) -> Result<()> {
        let (w, h) = (opal_c1::VIDEO_WIDTH as usize, opal_c1::VIDEO_HEIGHT as usize);
        let last = self.last.lock().unwrap();
        anyhow::ensure!(last.len() >= w * h, "no frame yet");
        let mut pgm = format!("P5\n{w} {h}\n255\n").into_bytes();
        pgm.extend_from_slice(&last[..w * h]);
        std::fs::write(path, pgm)?;
        Ok(())
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
