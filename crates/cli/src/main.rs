#[cfg(unix)]
mod cameras;
mod daemon;
mod mains;
mod output;
mod platform;
mod settings;
mod stickers;
mod uninstall;
mod webcam;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use opal_assets::{Asset, Event, Kind, RELEASES, Status};
use opal_dai::board::BoardConfig;
use opal_dai::boot::Step;
use opal_dai::device::Device;
use opal_xlink::LockExt;

/// `assets/` at the repository root. Left out of packaged builds (`OPAL_RS_PACKAGED` set), which have no source tree.
const ASSET_DIR: Option<&str> = match option_env!("OPAL_RS_PACKAGED") {
    Some(_) => None,
    None => Some(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets")),
};

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
    /// List the virtual cameras, or add and remove extra ones (one per app that should use the Opal C1 at the
    /// same time; Linux)
    Cameras {
        #[command(subcommand)]
        command: Option<CamerasCommand>,
    },
    /// Remove opal-rs, its settings and camera files, and what it set up on this computer
    Uninstall {
        /// Keep settings, presets, stickers and camera files
        #[arg(long)]
        keep_data: bool,
        /// Do not ask first
        #[arg(long)]
        yes: bool,
        /// Only list what would be removed
        #[arg(long)]
        dry_run: bool,
    },
    /// Boot in low power mode and keep the camera's own 1080p USB video on until Ctrl-C (no effects; Linux)
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
enum CamerasCommand {
    /// Add a camera with this name
    Add { name: String },
    /// Remove the camera with this name
    Remove { name: String },
    /// Make changes that waited for a camera to be free take effect now
    Apply,
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

/// Camera files: `OPAL_RS_ASSETS`; else `assets/` in the source tree this was built from or next to the program,
/// whichever holds the firmware; else the source tree's `assets/` if it exists (where its disk image goes); else the
/// user's data folder, which installed copies use as they cannot write next to themselves.
fn asset_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OPAL_RS_ASSETS").filter(|d| !d.is_empty()) {
        return dir.into();
    }
    let built = ASSET_DIR.map(PathBuf::from);
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.canonicalize().ok()?.parent()?.join("assets")));
    let holds_firmware = |d: &PathBuf| d.join(firmware_asset().file_name).is_file();
    let found = [built.clone(), beside]
        .into_iter()
        .flatten()
        .find(holds_firmware)
        .or_else(|| built.filter(|b| b.is_dir()));
    match found {
        Some(d) => d.canonicalize().unwrap_or(d),
        None => platform::data_dir().join("assets"),
    }
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

/// What the service waits for while no camera session runs: a code of [`opal_ipc::waiting_text`].
static WAITING: Mutex<&str> = Mutex::new("starting");

fn set_waiting(code: &'static str) {
    *WAITING.locked() = code;
}

/// Why no session started, when it is to be waited out without touching the camera.
#[derive(Debug)]
enum Wait {
    /// No camera is plugged in.
    Camera,
    /// The camera is on a USB 2 connection.
    Usb2,
    /// Windows 10, which has no virtual camera.
    #[cfg(windows)]
    Windows10,
    /// The virtual camera could not be opened; the camera was not touched.
    Output(String),
}

impl std::fmt::Display for Wait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Wait::Camera => "no camera found",
            Wait::Usb2 => "the camera is connected over USB 2",
            #[cfg(windows)]
            Wait::Windows10 => "this Windows has no virtual camera (Windows 11 is needed)",
            Wait::Output(e) => return write!(f, "the virtual camera did not open: {e}"),
        })
    }
}

impl std::error::Error for Wait {}

/// The settings file the service runs with, for changes made while no camera session runs.
static SETTINGS_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Answers the control channel while no camera session does: `status` says what the service waits for.
fn serve_idle(listener: Arc<opal_ipc::Listener>, outputs: Arc<Mutex<Option<output::Outputs>>>) {
    while !stopped() {
        if daemon::SERVING.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        match listener.accept() {
            Ok(Some(stream)) => {
                if let Err(e) = answer_idle(stream, &outputs) {
                    eprintln!("control: {e}");
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                eprintln!("control: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn answer_idle(stream: opal_ipc::Stream, outputs: &Mutex<Option<output::Outputs>>) -> Result<()> {
    use std::io::{BufRead, Read, Write};
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    std::io::BufReader::new(Read::take(&stream, 4096)).read_line(&mut line)?;
    let line = line.trim();
    let reply = match line.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["status"] => format!("waiting {}", *WAITING.locked()),
        // The control panel shows the saved settings meanwhile.
        ["settings"] => match SETTINGS_PATH.get() {
            Some(p) => {
                let saved = settings::Settings::load(p).commands().collect::<Vec<_>>().join("; ");
                if saved.is_empty() { "ok".to_owned() } else { saved }
            }
            None => "ok".to_owned(),
        },
        ["quit"] => {
            request_stop();
            "ok".to_owned()
        }
        // The virtual cameras stay open between sessions, so changing them works the same without a camera.
        ["cameras", "release"] => {
            if let Some(o) = outputs.locked().as_mut() {
                o.release();
            }
            "ok".to_owned()
        }
        ["cameras", "open"] => {
            if let Some(o) = outputs.locked().as_mut() {
                o.reopen();
            }
            "ok".to_owned()
        }
        _ => "error: the camera is not ready".to_owned(),
    };
    let mut stream = stream;
    writeln!(stream, "{reply}")?;
    Ok(())
}

/// Waits, without touching the camera, until `ready` holds. False if the service was told to stop first.
fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
    while !stopped() {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    false
}

/// The first camera, as the system lists it now. Listing can fail for a moment while a device is being plugged in;
/// that is the same as not found yet.
fn first_camera() -> Option<opal_xlink::Found> {
    opal_xlink::list().ok().and_then(|l| l.into_iter().next())
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

fn wait_for_camera(timeout: Duration) -> Option<opal_xlink::Found> {
    let start = Instant::now();
    let mut said = false;
    loop {
        if let Some(d) = first_camera() {
            if said {
                set_waiting("starting");
            }
            return Some(d);
        }
        if start.elapsed() > timeout || stopped() {
            return None;
        }
        // Not at once: a restarting camera is off USB for a moment.
        if !said && start.elapsed() > Duration::from_secs(3) {
            set_waiting("camera");
            said = true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Each boot makes the camera drop off USB and come back a few times, so boots are spaced out.
const MIN_BOOT_INTERVAL: Duration = Duration::from_secs(60);

/// Marks of the last boot: this user's, and one in the temp folder. On Linux that one is shared: the first user to
/// boot makes it and every user reads it (only its maker can update it). On Windows the temp folder is per user.
fn boot_stamps() -> [PathBuf; 2] {
    [
        platform::data_dir().join("last-boot"),
        std::env::temp_dir().join("opal-rs-last-boot"),
    ]
}

/// When this process last booted the camera. Kept in memory too, as file times cannot always be trusted.
static LAST_BOOT: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Time since the camera was last booted, by this process or by any of the marks.
fn since_boot() -> Option<Duration> {
    let here = LAST_BOOT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .map(|t| t.elapsed());
    let now = std::time::SystemTime::now();
    boot_stamps()
        .iter()
        .filter_map(|p| read_mark(p))
        .map(|t| age(t, now))
        .chain(here)
        .min()
}

/// The time written in a mark, or for an older empty mark its file time.
fn read_mark(path: &Path) -> Option<std::time::SystemTime> {
    let text = std::fs::read_to_string(path).ok()?;
    match text.trim().parse::<u64>() {
        Ok(ms) => Some(std::time::UNIX_EPOCH + Duration::from_millis(ms)),
        Err(_) => std::fs::metadata(path).and_then(|m| m.modified()).ok(),
    }
}

/// How long ago `mark` was. A mark slightly in the future is new. Only one far in the future (the clock was set
/// back) counts as old, or it would block boots for good.
fn age(mark: std::time::SystemTime, now: std::time::SystemTime) -> Duration {
    match now.duration_since(mark) {
        Ok(d) => d,
        Err(e) if e.duration() > MIN_BOOT_INTERVAL => Duration::MAX,
        Err(_) => Duration::ZERO,
    }
}

/// Records a boot. The time goes inside the marks: Windows leaves a file's time alone when an empty file is
/// written again, so an empty mark kept its first time and boots were not spaced out.
fn mark_boot() -> Result<()> {
    *LAST_BOOT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let [own, shared] = boot_stamps();
    std::fs::create_dir_all(platform::data_dir())?;
    std::fs::write(&own, format!("{ms}\n")).with_context(|| own.display().to_string())?;
    // Belongs to whoever made it first; the others can still read it.
    let _ = std::fs::write(shared, format!("{ms}\n"));
    Ok(())
}

/// Held while this process may boot or use the camera, so a second opal-rs of the same user cannot reboot it.
fn lock_camera() -> Result<std::fs::File> {
    let dir = platform::data_dir();
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("camera.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            anyhow::bail!("opal-rs is already using the camera; `opal-rs ctl quit` stops the service")
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Restarts after the camera goes away are limited, so a camera that keeps failing is left alone.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(600);
/// Time a camera just plugged in gets before it is booted.
const PLUGGED_SETTLE: Duration = Duration::from_secs(3);
/// How often a virtual camera that did not open is tried again.
const OUTPUT_RETRY: Duration = Duration::from_secs(10);
/// A camera that resets itself as it fails is gone for a second or so; someone unplugging it, for longer.
const REPLUG_GONE: Duration = Duration::from_secs(3);

/// Waits for the camera to be unplugged and plugged in again: gone for at least [`REPLUG_GONE`], then back. Already
/// unplugged counts as the first half.
fn wait_replug() -> bool {
    let mut gone_since: Option<Instant> = None;
    wait_until(|| match first_camera() {
        Some(_) => gone_since.take().is_some_and(|t| t.elapsed() >= REPLUG_GONE),
        None => {
            gone_since.get_or_insert_with(Instant::now);
            false
        }
    })
}

/// Sleeps for `d`, or less if the service is told to stop.
fn sleep_unless_stopped(d: Duration) {
    let end = Instant::now() + d;
    while !stopped() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(250).min(end - Instant::now()));
    }
}

/// Time left before another boot is allowed.
fn boot_wait() -> Duration {
    since_boot().map_or(Duration::ZERO, |since| MIN_BOOT_INTERVAL.saturating_sub(since))
}

/// The virtual cameras and the control channel, kept from one camera session to the next so apps keep them while
/// the camera restarts.
struct Kept {
    outputs: Arc<Mutex<Option<output::Outputs>>>,
    registration: Option<output::Registration>,
    /// Answered by the session while one runs and by [`serve_idle`] otherwise.
    listener: Arc<opal_ipc::Listener>,
}

fn run_session(
    dir: &Path,
    output: Option<&Path>,
    settings: settings::Settings,
    overrides: &[String],
    kept: &mut Kept,
    planned: bool,
) -> Result<()> {
    let low_power = settings.low_power();
    // In low power mode apps get the camera's own USB video on Linux. On Windows that stalls, so there the video
    // still comes through here to the virtual camera, only without networks or effects.
    let mode = match (low_power, cfg!(unix)) {
        (false, _) => opal_c1::Mode::Full,
        (true, true) => opal_c1::Mode::Webcam,
        (true, false) => opal_c1::Mode::Plain,
    };
    let webcam = mode == opal_c1::Mode::Webcam;
    // Windows 10 has no virtual camera, so there is nothing to give apps: the camera is left alone.
    #[cfg(windows)]
    if !opal_vcam::host::available() {
        return Err(Wait::Windows10.into());
    }
    // Opened before the boot, so one that cannot open does not cost a boot. When apps use the camera itself, the
    // virtual camera goes.
    if webcam {
        *kept.outputs.locked() = None;
        kept.registration = None;
    } else if kept.outputs.locked().is_none() {
        let (w, h) = settings.resolution().size();
        match output::open(output.map(Path::to_path_buf), w as usize, h as usize) {
            Ok(Some((o, r))) => {
                *kept.outputs.locked() = Some(o);
                kept.registration = Some(r);
            }
            Ok(None) => {}
            Err(e) => return Err(Wait::Output(format!("{e:#}")).into()),
        }
    }
    let booted = boot(dir, webcam, planned)?;
    let dev = Device::connect(&booted, |m| eprintln!("[{}] {}", m.node_id_name, m.payload))?;
    let info = device_info(&dev)?;
    let sensor = &info.sensor;
    // The link can come back slower after the boot. The camera, left without a pipeline, goes back to its own
    // firmware by itself.
    if matches!(info.usb.as_str(), "low" | "full" | "high") {
        return Err(opal_dai::boot::Error::SlowUsb.into());
    }
    // Low power mode is always 1080p: the only size the camera's own USB video sends, and on Windows the least work.
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
    if let Some(o) = kept.outputs.locked().as_mut() {
        let (w, h) = resolution.size();
        o.set_size(w as usize, h as usize);
    }
    daemon::run(
        dev,
        info,
        dir,
        (resolution, mode),
        (kept.outputs.clone(), &kept.listener),
        settings,
        overrides,
    )
}

fn device_info(dev: &Device) -> Result<daemon::DeviceInfo> {
    // Asked first: this is the order of calls tested on the camera.
    let sensor = sensor_name(dev)?;
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
    Ok(daemon::DeviceInfo {
        sensor,
        serial,
        usb: (*usb).into(),
        firmware: firmware[..firmware.len().min(8)].into(),
    })
}

fn check_boot_interval() -> Result<()> {
    let Some(since) = since_boot() else {
        return Ok(());
    };
    if since < MIN_BOOT_INTERVAL {
        anyhow::bail!(
            "camera was booted {}s ago, wait {}s",
            since.as_secs(),
            (MIN_BOOT_INTERVAL - since).as_secs()
        );
    }
    Ok(())
}

/// Boots the camera, with its own USB video output on when `usb_video`. `planned` is a restart asked
/// for (a new resolution or power mode): a camera waiting in its bootloader after the last session is booted right
/// away, as the camera's own app does, rather than after the spacing kept between other boots.
fn boot(dir: &Path, usb_video: bool, planned: bool) -> Result<opal_xlink::Found> {
    let device = wait_for_camera(Duration::from_secs(20)).ok_or(Wait::Camera)?;
    // Booting on USB 2 has stopped the camera within seconds.
    if device.slow_usb() {
        return Err(Wait::Usb2.into());
    }
    if !(planned && device.state == opal_xlink::State::Bootloader) {
        check_boot_interval()?;
    }
    mark_boot()?;
    let firmware = opal_assets::load(dir, firmware_asset())?;
    let config = BoardConfig {
        uvc_enable: Some(usb_video),
        ..Default::default()
    };
    Ok(opal_dai::boot::boot(&device, &config, &firmware, |step| match step {
        Step::Rebooting(usb) => println!("{} rebooting to bootloader, usb {usb}", platform::log_time()),
        Step::Bootloader(v, kind, usb) => println!("{} bootloader {v} type {kind}, usb {usb}", platform::log_time()),
        Step::Sending(n) => println!("sending {n} bytes"),
        Step::Booted(usb) => println!("{} booted, usb {usb}", platform::log_time()),
    })?)
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    // Run as administrator through pkexec only to change the virtual cameras.
    if std::env::var_os("PKEXEC_UID").is_some() && !matches!(cli.command, Command::Cameras { command: Some(_) }) {
        anyhow::bail!("only `opal-rs cameras add` and `remove` run through pkexec");
    }
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
        Command::Webcam { flicker } => {
            // On Windows the camera's own USB video stalls (see `run_session`).
            if cfg!(windows) {
                anyhow::bail!("webcam is only on Linux");
            }
            let _lock = lock_camera()?;
            webcam::run(&dir, &flicker)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run {
            output,
            blur,
            lut,
            settings,
        } => {
            let _lock = lock_camera()?;
            on_stop()?;
            let settings_path = settings.unwrap_or_else(settings::default_path);
            let _ = SETTINGS_PATH.set(settings_path.clone());
            let settings = settings::Settings::load(&settings_path);
            // Checked before the camera is touched: a boot that cannot go on only churns USB.
            if !assets_ready(&dir) {
                anyhow::bail!(
                    "camera files missing in {}: open the control panel, or run `opal-rs assets extract <disk image>`",
                    dir.display()
                );
            }
            let listener =
                Arc::new(opal_ipc::Listener::bind().with_context(|| opal_ipc::location().display().to_string())?);
            let outputs: Arc<Mutex<Option<output::Outputs>>> = Arc::default();
            {
                let (listener, outputs) = (listener.clone(), outputs.clone());
                std::thread::spawn(move || serve_idle(listener, outputs));
            }
            if let Some(path) = output.as_ref().filter(|_| !settings.low_power()) {
                if !path.exists() {
                    eprintln!("waiting for {}; is v4l2loopback loaded?", path.display());
                    set_waiting("output");
                }
                if !wait_until(|| path.exists()) {
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
            let mut planned = false;
            let mut last_wait: Option<String> = None;
            let mut kept = Kept {
                outputs,
                registration: None,
                listener,
            };
            loop {
                set_waiting("starting");
                let settings = match settings.take() {
                    Some(s) => s,
                    None => settings::Settings::load(&settings_path),
                };
                let result = run_session(&dir, output.as_deref(), settings, &overrides, &mut kept, planned);
                let was_planned = std::mem::take(&mut planned);
                if let Some(o) = kept.outputs.locked().as_mut() {
                    o.blank();
                }
                if daemon::take_restart() && result.is_ok() {
                    // The session ended with a reset, so the camera comes back as its bootloader within seconds.
                    let start = Instant::now();
                    planned = wait_until(|| {
                        start.elapsed() > Duration::from_secs(10)
                            || first_camera().is_some_and(|c| c.state == opal_xlink::State::Bootloader)
                    }) && first_camera().is_some_and(|c| c.state == opal_xlink::State::Bootloader);
                    if !planned {
                        sleep_unless_stopped(boot_wait());
                    }
                    if !stopped() {
                        continue;
                    }
                }
                if stopped() {
                    result?;
                    break;
                }
                let err = result.err().unwrap_or_else(|| anyhow::anyhow!("session ended"));
                // Nothing was sent to the camera: wait for it to be plugged in, or to be on USB 3.
                if let Some(wait) = err.downcast_ref::<Wait>() {
                    // Once per reason, not on every retry.
                    let said = err.to_string();
                    if last_wait.as_deref() != Some(said.as_str()) {
                        eprintln!("{} {said}; waiting", platform::log_time());
                        last_wait = Some(said);
                    }
                    let ready = match wait {
                        Wait::Camera => {
                            set_waiting("camera");
                            // A camera just plugged in is still starting up; booting it at once fails.
                            let ready = wait_until(|| first_camera().is_some());
                            sleep_unless_stopped(PLUGGED_SETTLE);
                            ready && !stopped()
                        }
                        Wait::Usb2 => {
                            set_waiting("usb2");
                            wait_until(|| first_camera().is_some_and(|c| !c.slow_usb()))
                        }
                        // Until the service is stopped.
                        #[cfg(windows)]
                        Wait::Windows10 => {
                            set_waiting("windows10");
                            wait_until(|| false)
                        }
                        // Not the camera's fault, so not counted against it: tried again in a while. The camera was
                        // not touched, so a planned restart stays planned.
                        Wait::Output(_) => {
                            planned = was_planned;
                            set_waiting("output");
                            sleep_unless_stopped(OUTPUT_RETRY);
                            !stopped()
                        }
                    };
                    if !ready {
                        break;
                    }
                    continue;
                }
                restarts.retain(|t| t.elapsed() < RESTART_WINDOW);
                if restarts.len() >= MAX_RESTARTS {
                    // Left alone until it is plugged in again, rather than booted over and over.
                    eprintln!(
                        "{} {err:#}; too many restarts, waiting for the camera to be plugged in again",
                        platform::log_time()
                    );
                    set_waiting("replug");
                    if !wait_replug() {
                        break;
                    }
                    restarts.clear();
                    continue;
                }
                restarts.push(Instant::now());
                eprintln!("{} {err:#}; waiting for the camera", platform::log_time());
                let slow = matches!(
                    err.downcast_ref::<opal_dai::boot::Error>(),
                    Some(opal_dai::boot::Error::SlowUsb)
                );
                // Booted onto USB 2: the USB 3 link sometimes does not come up as the camera starts. Starting it again
                // from its bootloader usually brings it; counted above like any other failed start.
                if slow && let Some(camera) = first_camera().filter(|c| c.state == opal_xlink::State::Booted) {
                    eprintln!("{} starting the camera again for USB 3", platform::log_time());
                    set_waiting("starting");
                    if opal_xlink::bootloader::reboot_to_bootloader(&camera.info).is_ok() {
                        let start = Instant::now();
                        planned = wait_until(|| {
                            start.elapsed() > Duration::from_secs(10)
                                || first_camera().is_some_and(|c| c.state == opal_xlink::State::Bootloader)
                        }) && first_camera().is_some_and(|c| c.state == opal_xlink::State::Bootloader);
                        if planned {
                            continue;
                        }
                    }
                }
                // Says what it waits for as that changes: unplugged, or plugged in on USB 2.
                if !wait_until(|| {
                    let camera = first_camera();
                    set_waiting(match &camera {
                        None => "camera",
                        Some(c) if slow || c.slow_usb() => "usb2",
                        Some(_) => "starting",
                    });
                    camera.is_some_and(|c| !c.slow_usb())
                }) {
                    break;
                }
                set_waiting("starting");
                // Also time for a camera just plugged in again to start up.
                sleep_unless_stopped(boot_wait().max(PLUGGED_SETTLE));
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
        Command::Uninstall {
            keep_data,
            yes,
            dry_run,
        } => Ok(if uninstall::uninstall(keep_data, yes, dry_run)? {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }),
        Command::Cameras { command } => camera_command(command),
        Command::List => {
            for d in opal_xlink::list()? {
                let speed = d.info.speed().map_or("-".to_owned(), |s| format!("{s:?}"));
                println!(
                    "{:?} {:04x} {} {speed}",
                    d.state,
                    d.state.pid(),
                    d.serial().unwrap_or("-")
                );
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

/// `opal-rs cameras`. Changes run as administrator; the service hands its cameras back first so the module can
/// reload with the new set, unless an app is using one (then it would not reload anyway).
#[cfg(unix)]
fn camera_command(command: Option<CamerasCommand>) -> Result<ExitCode> {
    let Some(command) = command else {
        match opal_ipc::request("cameras", Duration::from_secs(3)) {
            Ok(lines) if !lines.starts_with("error") => {
                for line in lines.split(" | ") {
                    let mut w = line.splitn(4, ' ');
                    let (nr, state, apps, name) = (w.next(), w.next(), w.next(), w.next());
                    let (Some(nr), Some(state), Some(apps), Some(name)) = (nr, state, apps, name) else {
                        continue;
                    };
                    let note = match (state, apps) {
                        ("restart", _) => "after the computer restarts".to_owned(),
                        ("leaving", _) => "removed; goes away when the computer restarts".to_owned(),
                        ("on", "-") => "free".to_owned(),
                        ("on", apps) => format!("used by {}", apps.replace(',', ", ")),
                        _ => "not in use".to_owned(),
                    };
                    println!("/dev/video{nr}  {name}  ({note})");
                }
            }
            _ => {
                for c in cameras::configured() {
                    let note = if c.present() {
                        ""
                    } else {
                        "  (after the computer restarts)"
                    };
                    println!("{}  {}{note}", c.path().display(), c.name);
                }
            }
        }
        return Ok(ExitCode::SUCCESS);
    };
    let (verb, name) = match &command {
        CamerasCommand::Add { name } => ("add", Some(name)),
        CamerasCommand::Remove { name } => ("remove", Some(name)),
        CamerasCommand::Apply => ("apply", None),
    };
    if unsafe { libc::geteuid() } == 0 {
        let applied = match &command {
            CamerasCommand::Add { name } => cameras::add(name)?,
            CamerasCommand::Remove { name } => cameras::remove(name)?,
            CamerasCommand::Apply => cameras::reload()?,
        };
        let done = match (verb, name) {
            ("add", Some(name)) => format!("Added {name}. "),
            ("remove", Some(name)) => format!("Removed {name}. "),
            _ => String::new(),
        };
        match applied {
            cameras::Applied::Now if name.is_none() => println!("The cameras are up to date."),
            cameras::Applied::Now => println!("{}", done.trim_end()),
            cameras::Applied::AfterRestart => println!(
                "{done}A camera is in use, so this takes effect when the computer restarts, or with Apply once no \
                 app uses one."
            ),
        }
        return Ok(ExitCode::SUCCESS);
    }
    let current = cameras::configured();
    match &command {
        CamerasCommand::Add { name } => cameras::check_name(name, &current)?,
        CamerasCommand::Remove { name } => {
            if !current.iter().skip(1).any(|c| c.name == *name) {
                anyhow::bail!("there is no extra camera called {name}");
            }
        }
        CamerasCommand::Apply => {}
    }
    // An idle service answers with an error: nothing is in use then.
    let in_use = opal_ipc::request("cameras", Duration::from_secs(3)).is_ok_and(|l| {
        !l.starts_with("error:")
            && l.split(" | ")
                .any(|c| c.split(' ').nth(2).is_some_and(|apps| apps != "-"))
    });
    let released = !in_use && opal_ipc::request("cameras release", Duration::from_secs(3)).is_ok();
    let exe = std::env::current_exe()?.display().to_string();
    let mut args = vec![exe, "cameras".into(), verb.into()];
    args.extend(name.cloned());
    let status = uninstall::as_admin(&args).status();
    if released {
        let _ = opal_ipc::request("cameras open", Duration::from_secs(3));
    }
    // pkexec's own codes (126: no password given) pass through for the control panel.
    Ok(match status?.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code.clamp(1, 255) as u8),
        None => ExitCode::FAILURE,
    })
}

#[cfg(windows)]
fn camera_command(_: Option<CamerasCommand>) -> Result<ExitCode> {
    if opal_vcam::host::available() {
        println!("Windows shares a camera between apps itself, so there is one camera, Opal C1 Virtual.");
    } else {
        println!("This Windows has no virtual camera (Windows 11 is needed); apps use the camera's own picture.");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_mark_holds_its_time() {
        let path = std::env::temp_dir().join(format!("opal-rs-mark-test-{}", std::process::id()));
        std::fs::write(&path, "1700000000123\n").unwrap();
        assert_eq!(
            read_mark(&path),
            Some(std::time::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123))
        );
        // An empty mark from an older version falls back to its file time.
        std::fs::write(&path, "").unwrap();
        assert!(read_mark(&path).is_some_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(60)));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn boot_mark_age() {
        let now = std::time::SystemTime::now();
        assert_eq!(age(now - Duration::from_secs(5), now), Duration::from_secs(5));
        // A mark a moment ahead of the clock was just made.
        assert_eq!(age(now + Duration::from_millis(3), now), Duration::ZERO);
        assert_eq!(age(now + Duration::from_secs(3600), now), Duration::MAX);
    }
}
