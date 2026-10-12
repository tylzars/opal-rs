//! Long-running camera session controlled over a Unix socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use opal_c1::Led;
use opal_c1::PREVIEW_SIZE;
use opal_c1::faces::{self, Tracker};
use opal_c1::hands::{self, Gesture, Gestures, HandBox, View};
use opal_c1::{Branch, Resolution};
use opal_dai::device::Device;
use opal_dai::message::{self, AudioInConfig, CameraControl, IMG_FRAME, NnConfig, NnData, af, antibanding, awb};
use opal_effects::{BackgroundBlur, Cube, Framing, Mask, Speed, YuvLut, crop_scale};
use opal_xlink::{Link, StreamId};

use crate::settings::Settings;

/// How long the camera keeps streaming after the last app closes the output.
const OFF_DELAY: Duration = Duration::from_secs(2);
/// The camera takes a few seconds to produce frames after a start, and stopping it before then can bring the
/// firmware down. So it is only stopped once frames have been coming for this long.
const STOP_AFTER: Duration = Duration::from_secs(2);
/// Time a stop needs before the camera can be rebooted.
const STOP_SETTLE: Duration = Duration::from_secs(3);
/// In low power mode, time after a start by which the camera is taken to be sending video.
const LOW_POWER_START: Duration = Duration::from_secs(3);
/// Shortest time from a stop to the next start.
const RESTART_AFTER: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(250);
const TEMPERATURE_PERIOD: Duration = Duration::from_secs(5);
/// Blur radius for `blur on`, in frame pixels.
const DEFAULT_BLUR: usize = 24;
/// How far each new mask moves the smoothed one.
const MASK_FOLLOW: f32 = 0.5;

static TERMINATE: AtomicBool = AtomicBool::new(false);
static RESTART: AtomicBool = AtomicBool::new(false);

/// True, once, after a command asked for the session to be rebuilt (a new resolution).
pub fn take_restart() -> bool {
    RESTART.swap(false, Ordering::Relaxed)
}

extern "C" fn on_signal(_: libc::c_int) {
    TERMINATE.store(true, Ordering::Relaxed);
}

/// True once a stop was requested by a signal or `ctl quit`.
pub fn stopped() -> bool {
    TERMINATE.load(Ordering::Relaxed)
}

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("opal-rs.sock")
}

/// Who decides whether the camera streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Stream while an app has the output open, or always without an output.
    Auto,
    On,
    Off,
}

struct State {
    mode: Mode,
    streaming: bool,
    consumers: usize,
    last_consumer: Option<Instant>,
    last_change: Instant,
    /// Frame count when streaming last started, and when the first frame after that arrived.
    start_frames: u64,
    first_frame: Option<Instant>,
}

/// Fixed facts about the connected camera, shown in status.
pub struct DeviceInfo {
    pub serial: String,
    /// USB link speed: high, super and so on.
    pub usb: String,
    /// Short commit of the camera firmware.
    pub firmware: String,
}

/// Background blur settings and the latest person mask.
#[derive(Default)]
struct Blur {
    /// In frame pixels; 0 is off.
    radius: usize,
    /// Picture shown instead of the background, as an NV12 frame.
    background: Option<(String, Arc<Vec<u8>>)>,
    mask: Option<Mask>,
    masks: u64,
    layout: String,
}

/// A picture, scaled and cropped to cover a `width` x `height` frame, as full-range BT.601 NV12.
fn load_background(path: &Path, width: u32, height: u32) -> Result<Vec<u8>> {
    let img = image::open(path).with_context(|| path.display().to_string())?.to_rgb8();
    let scale = (width as f32 / img.width() as f32).max(height as f32 / img.height() as f32);
    let (cw, ch) = ((width as f32 / scale) as u32, (height as f32 / scale) as u32);
    let (cx, cy) = (
        (img.width() - cw.min(img.width())) / 2,
        (img.height() - ch.min(img.height())) / 2,
    );
    let crop = image::imageops::crop_imm(&img, cx, cy, cw, ch).to_image();
    let rgb = image::imageops::resize(&crop, width, height, image::imageops::FilterType::Triangle);
    let (w, h) = (width as usize, height as usize);
    let mut nv12 = vec![0u8; w * h * 3 / 2];
    let luma = |p: &image::Rgb<u8>| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
    for (i, p) in rgb.pixels().enumerate() {
        nv12[i] = luma(p).round() as u8;
    }
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let p = rgb.get_pixel(2 * x as u32, 2 * y as u32);
            let l = luma(p);
            let u = 128.0 + (p[2] as f32 - l) / 1.772;
            let v = 128.0 + (p[0] as f32 - l) / 1.402;
            let at = w * h + y * w + 2 * x;
            nv12[at] = u.round().clamp(0.0, 255.0) as u8;
            nv12[at + 1] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    Ok(nv12)
}

/// Colour lookup table applied to every frame.
struct Look {
    name: String,
    lut: Arc<YuvLut>,
    amount: f32,
}

/// A built-in look by name, or a .cube file.
fn load_look(look: &str, amount: f32) -> Result<Look> {
    let (name, cube) = match opal_effects::builtin_look(look) {
        Some(cube) => (look.to_owned(), cube),
        None => {
            let path = Path::new(look);
            let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
            let cube = Cube::parse(&text).with_context(|| path.display().to_string())?;
            (
                path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                cube,
            )
        }
    };
    Ok(Look {
        name,
        lut: Arc::new(YuvLut::new(&cube)),
        amount,
    })
}

/// Auto framing: follows a face with a crop.
struct Zoom {
    enabled: bool,
    framing: Framing,
    tracker: Tracker,
    faces: u64,
}

/// Hand gestures: peace turns the video off and on, a pinch turns face framing off and on.
struct Hands {
    enabled: bool,
    /// The box of the landmark crop asked for, while waiting for its result.
    pending: Option<(HandBox, Instant)>,
    seen: bool,
    last: Option<Gesture>,
    gestures: Gestures,
}

/// A landmark crop not answered in this time is given up on.
const CROP_TIMEOUT: Duration = Duration::from_millis(500);

struct Session {
    link: Arc<Link>,
    control: StreamId,
    mic: StreamId,
    led: StreamId,
    /// Network and crop control streams of each branch, in `Branch::ALL` order; empty in low power mode.
    branches: Vec<(StreamId, StreamId)>,
    crop_in: Option<StreamId>,
    /// The camera streams straight to apps over USB video, without effects.
    low_power: bool,
    /// The camera's own video device, watched for apps in low power mode.
    camera_node: Mutex<Option<PathBuf>>,
    resolution: Resolution,
    output: Option<PathBuf>,
    state: Mutex<State>,
    blur: Mutex<Blur>,
    look: Mutex<Option<Look>>,
    zoom: Mutex<Zoom>,
    hands: Mutex<Hands>,
    /// Microphone on, and its gain in dB.
    mic_state: Mutex<(bool, f32)>,
    /// Set by the peace gesture: black frames instead of video.
    video_off: AtomicBool,
    info: DeviceInfo,
    settings: Mutex<Settings>,
    frames: AtomicU64,
    /// Average chip temperature in degrees C.
    temperature: Mutex<Option<f32>>,
    last_frame: Mutex<Vec<u8>>,
    alive: AtomicBool,
}

impl Session {
    /// Pauses or resumes a branch: its crop and its network.
    fn set_branch(&self, branch: Branch, on: bool) -> Result<()> {
        let i = Branch::ALL.iter().position(|&b| b == branch).unwrap();
        let Some(&(network, crop)) = self.branches.get(i) else {
            return Ok(());
        };
        if on {
            self.link.write(crop, &branch.crop(true).to_packet())?;
            self.link
                .write(network, &NnConfig { process_inputs: true }.to_packet())?;
        } else {
            self.link
                .write(network, &NnConfig { process_inputs: false }.to_packet())?;
            self.link.write(crop, &branch.crop(false).to_packet())?;
        }
        Ok(())
    }

    fn set_streaming(&self, st: &mut State, on: bool) -> Result<()> {
        if st.streaming == on {
            return Ok(());
        }
        if on && st.last_change.elapsed() < RESTART_AFTER {
            return Ok(());
        }
        if !on && !st.first_frame.is_some_and(|t| t.elapsed() >= STOP_AFTER) {
            return Ok(());
        }
        if on {
            st.start_frames = self.frames.load(Ordering::Relaxed);
            st.first_frame = None;
            self.link.write(self.control, &opal_c1::start_control().to_packet())?;
            self.link.write(self.led, &Led::White.packet())?;
            for c in self.settings.lock().unwrap().camera_commands() {
                if let Some(c) = parse_control(c) {
                    self.link.write(self.control, &c.to_packet())?;
                }
            }
        } else {
            self.link.write(self.control, &opal_c1::stop_control().to_packet())?;
            self.link.write(self.led, &Led::Off.packet())?;
        }
        st.streaming = on;
        st.last_change = Instant::now();
        Ok(())
    }

    fn update(&self, consumers: Option<usize>) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        // Low power video does not pass through the host, so frames are assumed once the camera has had time to start.
        let started = if self.low_power {
            st.last_change.elapsed() >= LOW_POWER_START
        } else {
            self.frames.load(Ordering::Relaxed) > st.start_frames
        };
        if st.streaming && st.first_frame.is_none() && started {
            st.first_frame = Some(Instant::now());
        }
        if let Some(n) = consumers {
            st.consumers = n;
            if n > 0 {
                st.last_consumer = Some(Instant::now());
            }
        }
        let want = match st.mode {
            Mode::On => true,
            Mode::Off => false,
            Mode::Auto => {
                (self.output.is_none() && !self.low_power) || st.last_consumer.is_some_and(|t| t.elapsed() < OFF_DELAY)
            }
        };
        self.set_streaming(&mut st, want)
    }

    /// Runs a command and keeps it in the settings if it changes one.
    fn command(&self, line: &str) -> Result<String> {
        let reply = self.run_command(line)?;
        if let Err(e) = self.settings.lock().unwrap().record(line) {
            eprintln!("settings: {e}");
        }
        Ok(reply)
    }

    fn run_command(&self, line: &str) -> Result<String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        if self.low_power && words.first().is_some_and(|w| is_effect(w)) {
            bail!("not available in low power mode");
        }
        match words.as_slice() {
            ["status"] => {
                let st = self.state.lock().unwrap();
                let blur = self.blur.lock().unwrap();
                let person = blur
                    .mask
                    .as_ref()
                    .map_or(0.0, |m| m.data.iter().sum::<f32>() / m.data.len() as f32);
                let look = self.look.lock().unwrap();
                let look = look
                    .as_ref()
                    .map_or("none".into(), |l| format!("{} {:.2}", l.name, l.amount));
                let zoom = self.zoom.lock().unwrap();
                let zoom = if zoom.enabled {
                    format!("{:.2} faces {}", zoom.framing.amount, zoom.faces)
                } else {
                    "off".into()
                };
                Ok(format!(
                    "mode {} power {} resolution {} temperature {} streaming {} consumers {} frames {} blur {} background {} masks {} person {:.2} lut {} zoom {} gestures {} mic {} video {} usb {} serial {} firmware {}",
                    format!("{:?}", st.mode).to_lowercase(),
                    if self.low_power { "low" } else { "normal" },
                    self.resolution.name(),
                    self.temperature
                        .lock()
                        .unwrap()
                        .map_or("-".into(), |t| format!("{t:.1}")),
                    st.streaming,
                    st.consumers,
                    self.frames.load(Ordering::Relaxed),
                    blur.radius,
                    blur.background.as_ref().map_or("none", |b| b.0.as_str()),
                    blur.masks,
                    person,
                    look,
                    zoom,
                    {
                        let h = self.hands.lock().unwrap();
                        match (h.enabled, h.seen) {
                            (false, _) => "off".to_owned(),
                            (true, seen) => format!(
                                "on hand {} last {}",
                                if seen { "yes" } else { "no" },
                                h.last.map_or("-".into(), |g| format!("{g:?}").to_lowercase()),
                            ),
                        }
                    },
                    if self.mic_state.lock().unwrap().0 { "on" } else { "off" },
                    if self.video_off.load(Ordering::Relaxed) {
                        "off"
                    } else {
                        "on"
                    },
                    self.info.usb,
                    self.info.serial,
                    self.info.firmware,
                ))
            }
            ["zoom", amount] => {
                let amount: Option<f32> = match *amount {
                    "off" => None,
                    "on" => Some(0.5),
                    a => Some(a.parse::<f32>()?.clamp(0.0, 1.0)),
                };
                let mut zoom = self.zoom.lock().unwrap();
                if amount.is_some() != zoom.enabled {
                    self.set_branch(Branch::Face, amount.is_some())?;
                }
                zoom.enabled = amount.is_some();
                if let Some(a) = amount {
                    zoom.framing.amount = a;
                }
                Ok(String::new())
            }
            ["gestures", on @ ("on" | "off")] => {
                let on = *on == "on";
                let mut h = self.hands.lock().unwrap();
                if on != h.enabled {
                    self.set_branch(Branch::Palm, on)?;
                }
                (h.enabled, h.seen, h.pending) = (on, false, None);
                if !on {
                    self.video_off.store(false, Ordering::Relaxed);
                }
                Ok(String::new())
            }
            ["video", on @ ("on" | "off")] => {
                self.video_off.store(*on == "off", Ordering::Relaxed);
                Ok(String::new())
            }
            ["tracking", speed] => {
                self.zoom.lock().unwrap().framing.speed = match *speed {
                    "slow" => Speed::Slow,
                    "normal" => Speed::Normal,
                    "fast" => Speed::Fast,
                    _ => bail!("tracking slow, normal or fast"),
                };
                Ok(String::new())
            }
            ["power", power @ ("low" | "normal")] => {
                if (*power == "low") != self.low_power {
                    RESTART.store(true, Ordering::Relaxed);
                }
                Ok(String::new())
            }
            ["resolution", name] => {
                let res = Resolution::parse(name).ok_or_else(|| anyhow!("resolution 720p, 1080p or 1440p"))?;
                if res != self.resolution {
                    RESTART.store(true, Ordering::Relaxed);
                }
                Ok(String::new())
            }
            ["settings"] => Ok(self.settings.lock().unwrap().commands().collect::<Vec<_>>().join("; ")),
            ["lut", "off"] => {
                *self.look.lock().unwrap() = None;
                Ok(String::new())
            }
            ["lut", path, rest @ ..] => {
                let amount = match rest {
                    [] => 1.0,
                    [a] => a.parse()?,
                    _ => bail!("usage: lut <look or file.cube> [amount]"),
                };
                *self.look.lock().unwrap() = Some(load_look(path, amount)?);
                Ok(String::new())
            }
            ["background", "off"] => {
                let mut blur = self.blur.lock().unwrap();
                blur.background = None;
                if blur.radius == 0 {
                    self.set_branch(Branch::Segmentation, false)?;
                    blur.mask = None;
                }
                Ok(String::new())
            }
            ["background", path] => {
                let (w, h) = self.resolution.size();
                let image = load_background(Path::new(path), w, h)?;
                let name = Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let mut blur = self.blur.lock().unwrap();
                if blur.radius == 0 && blur.background.is_none() {
                    self.set_branch(Branch::Segmentation, true)?;
                }
                blur.background = Some((name, Arc::new(image)));
                Ok(String::new())
            }
            ["blur", radius] => {
                let radius: usize = match *radius {
                    "off" => 0,
                    "on" => DEFAULT_BLUR,
                    r => r.parse()?,
                };
                let mut blur = self.blur.lock().unwrap();
                if (radius > 0) != (blur.radius > 0) && blur.background.is_none() {
                    self.set_branch(Branch::Segmentation, radius > 0)?;
                    if radius == 0 {
                        blur.mask = None;
                    }
                }
                blur.radius = radius;
                Ok(String::new())
            }
            [mode @ ("auto" | "on" | "off")] => {
                self.state.lock().unwrap().mode = match *mode {
                    "on" => Mode::On,
                    "off" => Mode::Off,
                    _ => Mode::Auto,
                };
                self.update(None)?;
                Ok(String::new())
            }
            ["snap", path] => self.snap(Path::new(path)).map(|_| String::new()),
            ["gain", db] => {
                let db: f32 = db.parse()?;
                let mut mic = self.mic_state.lock().unwrap();
                if mic.0 {
                    self.link.write(self.mic, &AudioInConfig::gain_db(db).to_packet())?;
                }
                mic.1 = db;
                Ok(String::new())
            }
            ["mic", on @ ("on" | "off")] => {
                let mut mic = self.mic_state.lock().unwrap();
                mic.0 = *on == "on";
                let config = if mic.0 {
                    AudioInConfig::gain_db(mic.1)
                } else {
                    AudioInConfig::muted()
                };
                self.link.write(self.mic, &config.to_packet())?;
                Ok(String::new())
            }
            ["led", color] => {
                let led = match *color {
                    "off" => Led::Off,
                    "white" => Led::White,
                    "blue" => Led::Blue,
                    _ => bail!("unknown colour {color}"),
                };
                self.link.write(self.led, &led.packet())?;
                Ok(String::new())
            }
            _ => {
                let c = parse_control(line).ok_or_else(|| anyhow!("unknown command"))?;
                self.link.write(self.control, &c.to_packet())?;
                Ok(String::new())
            }
        }
    }

    /// Writes the luma plane of the latest frame as a PGM image.
    fn snap(&self, path: &Path) -> Result<()> {
        let (w, h) = self.resolution.size();
        let (w, h) = (w as usize, h as usize);
        let last = self.last_frame.lock().unwrap();
        anyhow::ensure!(last.len() >= w * h, "no frame yet");
        let mut pgm = format!("P5\n{w} {h}\n255\n").into_bytes();
        pgm.extend_from_slice(&last[..w * h]);
        std::fs::write(path, pgm)?;
        Ok(())
    }
}

/// One control command, e.g. `focus 120` or `exposure 10000 400`.
fn parse_control(line: &str) -> Option<CameraControl> {
    let mut words = line.split_whitespace();
    let cmd = words.next()?;
    if cmd == "antibanding" {
        let mode = match (words.next(), words.next()) {
            (Some(m), None) => crate::mains::mode(m)?,
            _ => return None,
        };
        let mut c = CameraControl::default();
        c.anti_banding(mode);
        return Some(c);
    }
    let args: Vec<i64> = words.map(|w| w.parse().ok()).collect::<Option<_>>()?;
    let mut c = CameraControl::default();
    match (cmd, args.as_slice()) {
        ("focus", [p]) => c.auto_focus_mode(af::OFF).manual_focus(*p as u8),
        ("af", []) => c.auto_focus_mode(af::CONTINUOUS_VIDEO).auto_focus_trigger(),
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

/// Device log level for `OPAL_RS_LOG`.
fn log_level(name: &str) -> Option<i32> {
    ["trace", "debug", "info", "warn", "error", "critical", "off"]
        .iter()
        .position(|&l| l == name)
        .map(|i| i as i32)
}

/// Black NV12 frame, written first so apps can open the output before video arrives.
fn black_frame((w, h): (u32, u32)) -> Vec<u8> {
    let y = (w * h) as usize;
    let mut f = vec![0u8; y];
    f.resize(y + y / 2, 128);
    f
}

/// Commands for effects, which low power mode does not have.
fn is_effect(command: &str) -> bool {
    matches!(
        command,
        "blur" | "background" | "lut" | "zoom" | "tracking" | "gestures" | "video" | "snap"
    )
}

/// The camera's own video device, when it is plugged in and booted.
fn find_camera_node() -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .find_map(|e| {
            let dir = e.path();
            let read = |f: &str| std::fs::read_to_string(dir.join(f)).map(|s| s.trim().to_owned());
            // `device` is the USB interface; its parent is the USB device.
            let ours = read("index").is_ok_and(|i| i == "0")
                && read("device/../idVendor").is_ok_and(|v| v == "03e7")
                && read("device/../idProduct").is_ok_and(|p| p == "f63b");
            ours.then(|| PathBuf::from("/dev").join(e.file_name()))
        })
}

/// Number of other processes that have `path` open.
fn count_consumers(path: &Path) -> usize {
    let me = std::process::id().to_string();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return 0;
    };
    procs
        .flatten()
        .filter(|p| {
            let name = p.file_name();
            let name = name.to_string_lossy();
            name.bytes().all(|b| b.is_ascii_digit()) && name != me
        })
        .filter(|p| {
            std::fs::read_dir(p.path().join("fd")).is_ok_and(|fds| {
                fds.flatten()
                    .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == path))
            })
        })
        .count()
}

fn handle(session: &Session, stream: UnixStream) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let line = line.trim();
    let reply = if line == "quit" {
        TERMINATE.store(true, Ordering::Relaxed);
        Ok(String::new())
    } else {
        session.command(line)
    };
    let mut stream = stream;
    match reply {
        Ok(s) if s.is_empty() => writeln!(stream, "ok")?,
        Ok(s) => writeln!(stream, "{s}")?,
        Err(e) => writeln!(stream, "error: {e}")?,
    }
    Ok(())
}

/// Starts the pipeline on a booted camera and serves commands until told to stop.
/// `overrides` are commands applied after the saved settings, without being saved.
#[allow(clippy::too_many_arguments)]
pub fn run(
    dev: Device,
    dir: &Path,
    sensor: &str,
    resolution: Resolution,
    low_power: bool,
    output: Option<PathBuf>,
    settings: Settings,
    overrides: &[String],
    info: DeviceInfo,
) -> Result<()> {
    let (vw, vh) = resolution.size();
    // In low power mode apps use the camera itself, not the virtual camera.
    let output = output
        .filter(|_| !low_power)
        .map(|p| p.canonicalize().with_context(|| p.display().to_string()))
        .transpose()?;
    let mut out = match &output {
        Some(path) => {
            let mut o =
                opal_v4l2::Output::open(path, vw, vh, opal_v4l2::NV12).with_context(|| path.display().to_string())?;
            o.write(&black_frame((vw, vh)))?;
            Some(o)
        }
        None => None,
    };

    if let Some(level) = std::env::var("OPAL_RS_LOG").ok().and_then(|l| log_level(&l)) {
        dev.call("setLogLevel", vec![level.into()])?;
    }
    let banding = crate::mains::mode(settings.antibanding()).unwrap_or(antibanding::HZ_60);
    let streams = dev.start_pipeline(&opal_c1::pipeline(dir, sensor, resolution, low_power, banding)?)?;
    let session = Arc::new(Session {
        link: dev.link().clone(),
        control: streams[opal_c1::CONTROL_STREAM],
        mic: streams[opal_c1::MIC_STREAM],
        led: streams[opal_c1::LED_STREAM],
        branches: Branch::ALL
            .iter()
            .map_while(|b| Some((*streams.get(b.network_control())?, *streams.get(b.crop_control())?)))
            .collect(),
        crop_in: streams.get(hands::CROP_STREAM).copied(),
        low_power,
        camera_node: Mutex::new(None),
        resolution,
        output: output.clone(),
        state: Mutex::new(State {
            mode: Mode::Auto,
            streaming: false,
            consumers: 0,
            last_consumer: None,
            last_change: Instant::now() - RESTART_AFTER,
            start_frames: 0,
            first_frame: None,
        }),
        blur: Mutex::new(Blur::default()),
        look: Mutex::new(None),
        zoom: Mutex::new(Zoom {
            enabled: false,
            framing: Framing::new(vw as usize, vh as usize),
            tracker: Tracker::default(),
            faces: 0,
        }),
        hands: Mutex::new(Hands {
            enabled: false,
            pending: None,
            seen: false,
            last: None,
            gestures: Gestures::default(),
        }),
        mic_state: Mutex::new((true, 0.0)),
        video_off: AtomicBool::new(false),
        info,
        settings: Mutex::new(settings),
        frames: AtomicU64::new(0),
        temperature: Mutex::new(None),
        last_frame: Mutex::new(Vec::new()),
        alive: AtomicBool::new(true),
    });
    session.link.write(session.led, &Led::Off.packet())?;
    for b in Branch::ALL {
        session.set_branch(b, false)?;
    }
    let saved: Vec<String> = session.settings.lock().unwrap().commands().map(str::to_owned).collect();
    for c in saved.iter().chain(overrides) {
        if low_power && c.split_whitespace().next().is_some_and(is_effect) {
            continue;
        }
        if let Err(e) = session.run_command(c) {
            eprintln!("{c}: {e}");
        }
    }

    {
        let session = session.clone();
        std::thread::spawn(move || {
            let (w, h) = (vw as usize, vh as usize);
            let mut effects = BackgroundBlur::new(w, h);
            let mut frame = Vec::new();
            let mut zoomed = Vec::new();
            while let Ok(packet) = session.link.read(opal_c1::VIDEO_STREAM, None) {
                let Some((data, IMG_FRAME, _)) = message::split(&packet) else {
                    continue;
                };
                frame.clear();
                frame.extend_from_slice(data);
                let look = session.look.lock().unwrap().as_ref().map(|l| (l.lut.clone(), l.amount));
                if let (Some((lut, amount)), true) = (look, frame.len() >= w * h * 3 / 2) {
                    lut.apply(&mut frame, w, h, amount);
                }
                let (radius, mask, background) = {
                    let blur = session.blur.lock().unwrap();
                    (
                        blur.radius,
                        blur.mask.clone(),
                        blur.background.as_ref().map(|b| b.1.clone()),
                    )
                };
                if let (Some(bg), Some(mask), true) = (&background, &mask, frame.len() >= w * h * 3 / 2) {
                    effects.replace(&mut frame, mask, bg);
                }
                if let (r @ 1.., Some(mask), None, true) = (radius, &mask, &background, frame.len() >= w * h * 3 / 2) {
                    effects.apply(&mut frame, mask, r);
                }
                let crop = {
                    let mut zoom = session.zoom.lock().unwrap();
                    let crop = zoom.framing.step();
                    (zoom.enabled && !zoom.framing.is_full()).then_some(crop)
                };
                if let (Some(crop), true) = (crop, frame.len() >= w * h * 3 / 2) {
                    zoomed.resize(frame.len(), 0);
                    crop_scale(&frame, &mut zoomed, w, h, crop);
                    std::mem::swap(&mut frame, &mut zoomed);
                }
                if session.video_off.load(Ordering::Relaxed) {
                    frame.fill(0);
                    let y = w * h;
                    frame[y..].fill(128);
                }
                if let Some(out) = &mut out
                    && let Err(e) = out.write(&frame)
                {
                    eprintln!("output: {e}");
                }
                session.frames.fetch_add(1, Ordering::Relaxed);
                std::mem::swap(&mut *session.last_frame.lock().unwrap(), &mut frame);
            }
            session.alive.store(false, Ordering::Relaxed);
        });
    }
    {
        let session = session.clone();
        std::thread::spawn(move || {
            let (w, h) = (
                opal_c1::SEGMENTATION_WIDTH as usize,
                opal_c1::SEGMENTATION_HEIGHT as usize,
            );
            while let Ok(packet) = session.link.read(opal_c1::SEGMENTATION_STREAM, None) {
                let Some((nn, data)) = NnData::parse(&packet) else {
                    continue;
                };
                let mut blur = session.blur.lock().unwrap();
                blur.masks += 1;
                if blur.layout.is_empty() {
                    blur.layout = nn
                        .tensors
                        .iter()
                        .map(|t| format!("{} {:?} order {:#x} type {}", t.name, t.dims, t.order, t.data_type))
                        .collect::<Vec<_>>()
                        .join(", ");
                }
                let Some(next) = nn
                    .tensor_f32(data, "segment")
                    .and_then(|scores| Mask::from_scores(&scores, w, h))
                else {
                    continue;
                };
                match &mut blur.mask {
                    Some(m) => m.follow(&next, MASK_FOLLOW),
                    None => blur.mask = Some(next),
                }
            }
        });
    }

    {
        let session = session.clone();
        std::thread::spawn(move || {
            while let Ok(packet) = session.link.read(opal_c1::FACE_STREAM, None) {
                let Some((nn, data)) = NnData::parse(&packet) else {
                    continue;
                };
                let found = faces::decode(&nn, data);
                let mut zoom = session.zoom.lock().unwrap();
                zoom.faces += 1;
                if let Some(face) = zoom.tracker.update(&found) {
                    let (cx, cy) = face.center();
                    zoom.framing.face(cx, cy, face.height());
                }
            }
        });
    }

    let view = View::new(PREVIEW_SIZE.0, PREVIEW_SIZE.1);
    {
        // Palm detections start following a hand when none is followed.
        let session = session.clone();
        std::thread::spawn(move || {
            let Some(crop_in) = session.crop_in else {
                return;
            };
            while let Ok(packet) = session.link.read(hands::PALM_STREAM, None) {
                let Some((nn, data)) = NnData::parse(&packet) else {
                    continue;
                };
                let mut h = session.hands.lock().unwrap();
                if !h.enabled || h.pending.is_some_and(|(_, t)| t.elapsed() < CROP_TIMEOUT) {
                    continue;
                }
                let scores = nn.tensor_f32(data, "classificators").unwrap_or_default();
                let regressors = nn.tensor_f32(data, "regressors").unwrap_or_default();
                h.pending = None;
                h.seen = false;
                if let Some(b) = view.palm(&scores, &regressors) {
                    if session.link.write(crop_in, &view.crop(&b).to_packet()).is_err() {
                        break;
                    }
                    h.pending = Some((b, Instant::now()));
                }
            }
        });
    }
    {
        // Each landmark result moves the crop on with the hand, and may make a gesture.
        let session = session.clone();
        let aspect = vw as f32 / vh as f32;
        std::thread::spawn(move || {
            let Some(crop_in) = session.crop_in else {
                return;
            };
            while let Ok(packet) = session.link.read(hands::LANDMARK_STREAM, None) {
                let Some((nn, data)) = NnData::parse(&packet) else {
                    continue;
                };
                let fired = {
                    let mut h = session.hands.lock().unwrap();
                    let Some((b, _)) = h.pending.take() else {
                        continue;
                    };
                    let first = |name: &str| {
                        nn.tensor_f32(data, name)
                            .and_then(|v| v.first().copied())
                            .unwrap_or(0.0)
                    };
                    let raw = nn.tensor_f32(data, "Identity_dense/BiasAdd/Add").unwrap_or_default();
                    let hand = view.hand(&b, first("Identity_1"), first("Identity_2"), &raw);
                    h.seen = hand.is_some();
                    if let (Some(hand), true) = (&hand, h.enabled) {
                        let next = hands::follow(hand);
                        if session.link.write(crop_in, &view.crop(&next).to_packet()).is_ok() {
                            h.pending = Some((next, Instant::now()));
                        }
                    }
                    let fired = h.gestures.update(hand.as_ref().and_then(|x| x.gesture(aspect)));
                    if fired.is_some() {
                        h.last = fired;
                    }
                    fired
                };
                let command = match fired {
                    Some(Gesture::Peace) => {
                        let off = !session.video_off.load(Ordering::Relaxed);
                        session.video_off.store(off, Ordering::Relaxed);
                        None
                    }
                    Some(Gesture::Pinch) => {
                        let zoom = session.zoom.lock().unwrap();
                        Some(if zoom.enabled {
                            "zoom off".to_owned()
                        } else {
                            format!("zoom {:.2}", zoom.framing.amount)
                        })
                    }
                    None => None,
                };
                if let Some(c) = command
                    && let Err(e) = session.command(&c)
                {
                    eprintln!("{c}: {e}");
                }
            }
        });
    }

    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
    let socket = socket_path();
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| socket.display().to_string())?;
    listener.set_nonblocking(true)?;
    println!("running");

    let mut result = Ok(());
    let mut last_temperature = Instant::now() - TEMPERATURE_PERIOD;
    while !TERMINATE.load(Ordering::Relaxed) && !RESTART.load(Ordering::Relaxed) {
        if !session.alive.load(Ordering::Relaxed) {
            result = Err(anyhow!("camera disconnected"));
            break;
        }
        if last_temperature.elapsed() >= TEMPERATURE_PERIOD {
            last_temperature = Instant::now();
            let t = dev.call("getChipTemperature", vec![]).ok().and_then(|v| match v {
                opal_dai::msgpack::Value::Map(m) => m.get("average").and_then(|a| match a {
                    opal_dai::msgpack::Value::F32(f) => Some(*f),
                    opal_dai::msgpack::Value::F64(f) => Some(*f as f32),
                    _ => None,
                }),
                _ => None,
            });
            *session.temperature.lock().unwrap() = t;
        }
        let consumers = if low_power {
            let mut node = session.camera_node.lock().unwrap();
            if node.as_ref().is_none_or(|n| !n.exists()) {
                *node = find_camera_node();
            }
            Some(node.as_deref().map_or(0, count_consumers))
        } else {
            output.as_deref().map(count_consumers)
        };
        if let Err(e) = session.update(consumers) {
            result = Err(e);
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(e) = handle(&session, stream) {
                    eprintln!("control: {e}");
                }
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                result = Err(e.into());
                break;
            }
        }
        std::thread::sleep(POLL);
    }
    let _ = std::fs::remove_file(&socket);
    if session.alive.load(Ordering::Relaxed) {
        let mut st = session.state.lock().unwrap();
        let _ = session.set_streaming(&mut st, false);
        if !st.streaming && st.last_change.elapsed() < STOP_SETTLE {
            // A reboot straight after a stop can catch the camera mid-stop and take the next boot down with it.
            std::thread::sleep(STOP_SETTLE - st.last_change.elapsed());
        }
    }
    result
}

/// Sends one command to a running session and prints the reply.
pub fn send(words: &[String]) -> Result<bool> {
    let socket = socket_path();
    let mut stream =
        UnixStream::connect(&socket).with_context(|| format!("no running session at {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut words = words.to_vec();
    // Paths are resolved by the daemon, which runs in another directory.
    if let [cmd, path, ..] = words.as_mut_slice()
        && matches!(cmd.as_str(), "lut" | "snap" | "background")
        && path != "off"
        && !(cmd == "lut" && opal_effects::LOOKS.contains(&path.as_str()))
    {
        *path = std::path::absolute(&*path)?.display().to_string();
    }
    writeln!(stream, "{}", words.join(" "))?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    let reply = reply.trim_end();
    if reply != "ok" {
        println!("{reply}");
    }
    Ok(!reply.starts_with("error:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antibanding_commands() {
        assert!(parse_control("antibanding 50").is_some());
        assert!(parse_control("antibanding auto").is_some());
        assert!(parse_control("antibanding 70").is_none());
        assert!(parse_control("antibanding").is_none());
    }
}
