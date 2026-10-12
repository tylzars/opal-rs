//! Long-running camera session controlled over a Unix socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use opal_c1::Led;
use opal_c1::Resolution;
use opal_c1::faces::{self, Tracker};
use opal_dai::device::Device;
use opal_dai::message::{self, AudioInConfig, CameraControl, IMG_FRAME, NnConfig, NnData, af, awb};
use opal_effects::{BackgroundBlur, Cube, Framing, Mask, Speed, YuvLut, crop_scale};
use opal_xlink::{Link, StreamId};

use crate::settings::Settings;

/// How long the camera keeps streaming after the last app closes the output.
const OFF_DELAY: Duration = Duration::from_secs(2);
/// The camera takes a few seconds to produce frames after a start, and stopping it before then can bring the
/// firmware down. So it is only stopped once frames have been coming for this long.
const STOP_AFTER: Duration = Duration::from_secs(2);
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

fn load_look(path: &Path, amount: f32) -> Result<Look> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let cube = Cube::parse(&text).with_context(|| path.display().to_string())?;
    Ok(Look {
        name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
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

struct Session {
    link: Arc<Link>,
    control: StreamId,
    mic: StreamId,
    led: StreamId,
    seg_ctrl: StreamId,
    face_ctrl: StreamId,
    resolution: Resolution,
    output: Option<PathBuf>,
    state: Mutex<State>,
    blur: Mutex<Blur>,
    look: Mutex<Option<Look>>,
    zoom: Mutex<Zoom>,
    settings: Mutex<Settings>,
    frames: AtomicU64,
    /// Average chip temperature in degrees C.
    temperature: Mutex<Option<f32>>,
    last_frame: Mutex<Vec<u8>>,
    alive: AtomicBool,
}

impl Session {
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
        if st.streaming && st.first_frame.is_none() && self.frames.load(Ordering::Relaxed) > st.start_frames {
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
            Mode::Auto => self.output.is_none() || st.last_consumer.is_some_and(|t| t.elapsed() < OFF_DELAY),
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
                    "mode {} resolution {} temperature {} streaming {} consumers {} frames {} blur {} background {} masks {} person {:.2} lut {} zoom {}",
                    format!("{:?}", st.mode).to_lowercase(),
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
                    let config = NnConfig {
                        process_inputs: amount.is_some(),
                    };
                    self.link.write(self.face_ctrl, &config.to_packet())?;
                }
                zoom.enabled = amount.is_some();
                if let Some(a) = amount {
                    zoom.framing.amount = a;
                }
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
                    _ => bail!("usage: lut <file.cube> [amount]"),
                };
                *self.look.lock().unwrap() = Some(load_look(Path::new(path), amount)?);
                Ok(String::new())
            }
            ["background", "off"] => {
                let mut blur = self.blur.lock().unwrap();
                blur.background = None;
                if blur.radius == 0 {
                    self.link
                        .write(self.seg_ctrl, &NnConfig { process_inputs: false }.to_packet())?;
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
                    self.link
                        .write(self.seg_ctrl, &NnConfig { process_inputs: true }.to_packet())?;
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
                    let config = NnConfig {
                        process_inputs: radius > 0,
                    };
                    self.link.write(self.seg_ctrl, &config.to_packet())?;
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
                self.link.write(self.mic, &AudioInConfig::gain_db(db).to_packet())?;
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
pub fn run(
    dev: Device,
    dir: &Path,
    sensor: &str,
    resolution: Resolution,
    output: Option<PathBuf>,
    settings: Settings,
    overrides: &[String],
) -> Result<()> {
    let (vw, vh) = resolution.size();
    let output = output
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
    let streams = dev.start_pipeline(&opal_c1::pipeline(dir, sensor, resolution)?)?;
    let session = Arc::new(Session {
        link: dev.link().clone(),
        control: streams[opal_c1::CONTROL_STREAM],
        mic: streams[opal_c1::MIC_STREAM],
        led: streams[opal_c1::LED_STREAM],
        seg_ctrl: streams[opal_c1::SEGMENTATION_CONTROL_STREAM],
        face_ctrl: streams[opal_c1::FACE_CONTROL_STREAM],
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
        settings: Mutex::new(settings),
        frames: AtomicU64::new(0),
        temperature: Mutex::new(None),
        last_frame: Mutex::new(Vec::new()),
        alive: AtomicBool::new(true),
    });
    session.link.write(session.led, &Led::Off.packet())?;
    let pause = NnConfig { process_inputs: false };
    session.link.write(session.seg_ctrl, &pause.to_packet())?;
    session.link.write(session.face_ctrl, &pause.to_packet())?;
    let saved: Vec<String> = session.settings.lock().unwrap().commands().map(str::to_owned).collect();
    for c in saved.iter().chain(overrides) {
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
        let consumers = output.as_deref().map(count_consumers);
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
    }
    result
}

/// Sends one command to a running session and prints the reply.
pub fn send(words: &[String]) -> Result<bool> {
    let socket = socket_path();
    let mut stream =
        UnixStream::connect(&socket).with_context(|| format!("no running session at {}", socket.display()))?;
    let mut words = words.to_vec();
    // Paths are resolved by the daemon, which runs in another directory.
    if let [cmd, path, ..] = words.as_mut_slice()
        && matches!(cmd.as_str(), "lut" | "snap" | "background")
        && path != "off"
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
