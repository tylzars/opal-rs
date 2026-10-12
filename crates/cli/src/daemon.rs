//! Long-running camera session controlled over a Unix socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use opal_c1::Led;
use opal_dai::device::Device;
use opal_dai::message::{self, AudioInConfig, CameraControl, IMG_FRAME, af, awb};
use opal_xlink::{Link, StreamId};

/// How long the camera keeps streaming after the last app closes the output.
const OFF_DELAY: Duration = Duration::from_secs(2);
/// Shortest time between starting and stopping the camera. It takes a moment to produce frames after a start,
/// and stopping it before then hangs the firmware.
const MIN_HOLD: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(250);

static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    TERMINATE.store(true, Ordering::Relaxed);
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
}

struct Session {
    link: Arc<Link>,
    control: StreamId,
    mic: StreamId,
    led: StreamId,
    output: Option<PathBuf>,
    state: Mutex<State>,
    frames: AtomicU64,
    last_frame: Mutex<Vec<u8>>,
    alive: AtomicBool,
}

impl Session {
    fn set_streaming(&self, st: &mut State, on: bool) -> Result<()> {
        if st.streaming == on || st.last_change.elapsed() < MIN_HOLD {
            return Ok(());
        }
        if on {
            self.link.write(self.control, &opal_c1::start_control().to_packet())?;
            self.link.write(self.led, &Led::White.packet())?;
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

    fn command(&self, line: &str) -> Result<String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["status"] => {
                let st = self.state.lock().unwrap();
                Ok(format!(
                    "mode {:?} streaming {} consumers {} frames {}",
                    st.mode,
                    st.streaming,
                    st.consumers,
                    self.frames.load(Ordering::Relaxed)
                )
                .to_lowercase())
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
        let (w, h) = (opal_c1::VIDEO_WIDTH as usize, opal_c1::VIDEO_HEIGHT as usize);
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
fn black_frame() -> Vec<u8> {
    let y = (opal_c1::VIDEO_WIDTH * opal_c1::VIDEO_HEIGHT) as usize;
    let mut f = vec![16u8; y];
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
pub fn run(dev: Device, dir: &Path, sensor: &str, output: Option<PathBuf>) -> Result<()> {
    let output = output
        .map(|p| p.canonicalize().with_context(|| p.display().to_string()))
        .transpose()?;
    let mut out = match &output {
        Some(path) => {
            let mut o = opal_v4l2::Output::open(path, opal_c1::VIDEO_WIDTH, opal_c1::VIDEO_HEIGHT, opal_v4l2::NV12)
                .with_context(|| path.display().to_string())?;
            o.write(&black_frame())?;
            Some(o)
        }
        None => None,
    };

    if let Some(level) = std::env::var("OPAL_RS_LOG").ok().and_then(|l| log_level(&l)) {
        dev.call("setLogLevel", vec![level.into()])?;
    }
    let streams = dev.start_pipeline(&opal_c1::pipeline(dir, sensor)?)?;
    let session = Arc::new(Session {
        link: dev.link().clone(),
        control: streams[opal_c1::CONTROL_STREAM],
        mic: streams[opal_c1::MIC_STREAM],
        led: streams[opal_c1::LED_STREAM],
        output: output.clone(),
        state: Mutex::new(State {
            mode: Mode::Auto,
            streaming: true,
            consumers: 0,
            last_consumer: None,
            last_change: Instant::now(),
        }),
        frames: AtomicU64::new(0),
        last_frame: Mutex::new(Vec::new()),
        alive: AtomicBool::new(true),
    });
    session
        .link
        .write(session.control, &opal_c1::start_control().to_packet())?;
    session.link.write(session.led, &Led::White.packet())?;

    {
        let session = session.clone();
        std::thread::spawn(move || {
            while let Ok(packet) = session.link.read(opal_c1::VIDEO_STREAM, None) {
                let Some((data, IMG_FRAME, _)) = message::split(&packet) else {
                    continue;
                };
                if let Some(out) = &mut out
                    && let Err(e) = out.write(data)
                {
                    eprintln!("output: {e}");
                }
                session.frames.fetch_add(1, Ordering::Relaxed);
                *session.last_frame.lock().unwrap() = data.to_vec();
            }
            session.alive.store(false, Ordering::Relaxed);
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
    while !TERMINATE.load(Ordering::Relaxed) {
        if !session.alive.load(Ordering::Relaxed) {
            result = Err(anyhow!("camera disconnected"));
            break;
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
    writeln!(stream, "{}", words.join(" "))?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    let reply = reply.trim_end();
    if reply != "ok" {
        println!("{reply}");
    }
    Ok(!reply.starts_with("error:"))
}
