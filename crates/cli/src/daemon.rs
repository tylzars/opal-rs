//! Long-running camera session controlled over a Unix socket.

use std::io::{BufRead, BufReader, Write};
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
use opal_effects::{Adjust, BackgroundBlur, Cube, Framing, Mask, Speed, YuvLut, crop_scale};
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
/// How often cameras that appeared or went away (see `cameras`) are picked up.
const REFRESH_PERIOD: Duration = Duration::from_secs(2);
/// How often the control socket is checked while waiting out `POLL`.
const ACCEPT_POLL: Duration = Duration::from_millis(10);
const TEMPERATURE_PERIOD: Duration = Duration::from_secs(5);
const LOCK_PERIOD: Duration = Duration::from_secs(2);
/// A camera that sends nothing for this long (not even keepalive replies) has stopped answering. Building a
/// pipeline can hold replies back for several seconds.
const SILENCE: Duration = Duration::from_secs(20);
/// How long the switch to and from the camera-off picture takes.
const FADE: Duration = Duration::from_millis(300);
/// Blur radius for `blur on`, in frame pixels.
const DEFAULT_BLUR: usize = 24;
/// How far each new mask moves the smoothed one.
const MASK_FOLLOW: f32 = 0.5;

static RESTART: AtomicBool = AtomicBool::new(false);

/// True, once, after a command asked for the session to be rebuilt (a new resolution).
pub fn take_restart() -> bool {
    RESTART.swap(false, Ordering::Relaxed)
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
    /// Programs using the camera.
    apps: Vec<String>,
    last_consumer: Option<Instant>,
    /// When the control panel last asked for a preview frame.
    last_preview: Option<Instant>,
    last_change: Instant,
    /// Frame count when streaming last started, and when the first frame after that arrived.
    start_frames: u64,
    first_frame: Option<Instant>,
}

/// Longest loop that can be recorded, and the most memory it may take.
const LOOP_MAX: Duration = Duration::from_secs(10);
const LOOP_BYTES: usize = 256 << 20;

/// A short recording that can play in place of the live video.
#[derive(Default)]
struct Clip {
    /// Half-size frames.
    frames: Vec<Vec<u8>>,
    recording: Option<Instant>,
    /// Playing, and whether it goes back and forth; the playback step.
    playing: Option<bool>,
    step: usize,
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
    /// Hexagonal lens blur instead of the smooth one.
    lens: bool,
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

/// A colour look and how strongly it is applied.
struct Look {
    name: String,
    cube: Cube,
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
    Ok(Look { name, cube, amount })
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
    /// Which gestures do anything.
    peace: bool,
    pinch: bool,
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
    /// Whether video goes to an output apps open (else it streams all the time).
    has_output: bool,
    state: Mutex<State>,
    blur: Mutex<Blur>,
    look: Mutex<Option<Look>>,
    adjust: Mutex<Adjust>,
    /// The look and adjustments in one table; none while both are off.
    colour_lut: Mutex<Option<Arc<YuvLut>>>,
    zoom: Mutex<Zoom>,
    hands: Mutex<Hands>,
    /// Microphone on, and its gain in dB.
    mic_state: Mutex<(bool, f32)>,
    /// Set by the peace gesture: black frames instead of video.
    video_off: AtomicBool,
    /// Centre zoom factor, 1 for none. Face framing takes over while it is on.
    magnify: Mutex<f32>,
    /// Pixelation strength, 0 to 1.
    pixelate: Mutex<f32>,
    skin: Mutex<opal_effects::Skin>,
    stickers: Mutex<crate::stickers::Stickers>,
    clip: Mutex<Clip>,
    /// Centre of the face being followed, 0 to 1, and when it was seen.
    face: Mutex<Option<((f32, f32), Instant)>>,
    /// Whether the segmentation and face networks are running.
    branch_on: Mutex<[bool; 2]>,
    info: DeviceInfo,
    /// The camera stopped answering. While nothing needs video it is left alone; it is rebooted only once
    /// something wants to stream, so a camera that stops while idle causes no USB churn.
    frozen: AtomicBool,
    /// The screen is locked, and whether that turns the camera off.
    locked: AtomicBool,
    lock_pause: AtomicBool,
    settings: Mutex<Settings>,
    frames: AtomicU64,
    /// Average chip temperature in degrees C.
    temperature: Mutex<Option<f32>>,
    last_frame: Mutex<Vec<u8>>,
    /// Where the video goes; none in low power mode or without an output.
    outputs: Mutex<Option<crate::output::Outputs>>,
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

    /// The status line. Each value is copied out under its own short lock, so no two are held at once.
    fn status_line(&self) -> String {
        let (mode, streaming, consumers, apps) = {
            let st = self.state.lock().unwrap();
            let apps = if st.apps.is_empty() {
                "-".to_owned()
            } else {
                st.apps.join(",")
            };
            (
                format!("{:?}", st.mode).to_lowercase(),
                st.streaming,
                st.consumers,
                apps,
            )
        };
        let (radius, background, masks, person) = {
            let b = self.blur.lock().unwrap();
            let person = b
                .mask
                .as_ref()
                .map_or(0.0, |m| m.data.iter().sum::<f32>() / m.data.len() as f32);
            (
                b.radius,
                // One word, so the status line keeps one value per key.
                b.background
                    .as_ref()
                    .map_or("none".to_owned(), |b| b.0.replace(char::is_whitespace, "_")),
                b.masks,
                person,
            )
        };
        let look = self.look.lock().unwrap().as_ref().map_or("none".into(), |l| {
            format!("{} {:.2}", l.name.replace(char::is_whitespace, "_"), l.amount)
        });
        let zoom = {
            let z = self.zoom.lock().unwrap();
            if z.enabled {
                format!("{:.2} faces {}", z.framing.amount, z.faces)
            } else {
                "off".into()
            }
        };
        let hands = {
            let h = self.hands.lock().unwrap();
            match (h.enabled, h.seen) {
                (false, _) => "off".to_owned(),
                (true, seen) => format!(
                    "on hand {} last {}",
                    if seen { "yes" } else { "no" },
                    h.last.map_or("-".into(), |g| format!("{g:?}").to_lowercase()),
                ),
            }
        };
        let temperature = self
            .temperature
            .lock()
            .unwrap()
            .map_or("-".into(), |t| format!("{t:.1}"));
        let adjust = if self.adjust.lock().unwrap().is_neutral() {
            "off"
        } else {
            "on"
        };
        let mic = if self.mic_state.lock().unwrap().0 { "on" } else { "off" };
        let video = if self.video_off.load(Ordering::Relaxed) {
            "off"
        } else {
            "on"
        };
        let locked = if self.locked.load(Ordering::Relaxed) {
            "yes"
        } else {
            "no"
        };
        let magnify = *self.magnify.lock().unwrap();
        let pixelate = *self.pixelate.lock().unwrap() * 100.0;
        let touchup = self.skin.lock().unwrap().smooth * 100.0;
        let stickers = self.stickers.lock().unwrap().list.len();
        let clip = {
            let c = self.clip.lock().unwrap();
            let n = c.frames.len();
            match (c.recording, c.playing) {
                (Some(_), _) => format!("recording {n}"),
                (None, Some(true)) => format!("bounce {n}"),
                (None, Some(false)) => format!("playing {n}"),
                _ => format!("off {n}"),
            }
        };
        // `apps` comes last: app names are free text and must not be mistaken for a later key.
        format!(
            "mode {mode} power {} resolution {} temperature {temperature} streaming {streaming} consumers {consumers} \
             frames {} blur {radius} background {background} masks {masks} person {person:.2} lut {look} \
             adjust {adjust} zoom {zoom} gestures {hands} mic {mic} video {video} locked {locked} responding {} \
             magnify {magnify:.2} pixelate {pixelate:.0} touchup {touchup:.0} stickers {stickers} loop {clip} \
             usb {} serial {} firmware {} virtualcam {} apps {apps}",
            if self.low_power { "low" } else { "normal" },
            self.resolution.name(),
            self.frames.load(Ordering::Relaxed),
            if self.frozen.load(Ordering::Relaxed) {
                "no"
            } else {
                "yes"
            },
            self.info.usb,
            self.info.serial,
            self.info.firmware,
            if virtual_camera_available() { "yes" } else { "no" },
        )
    }

    /// Runs the segmentation and face networks only while something uses them.
    fn sync_branches(&self) -> Result<()> {
        // Held throughout, so two callers cannot interleave and leave a network in the wrong state.
        let mut on = self.branch_on.lock().unwrap();
        let (behind, follow) = self.stickers.lock().unwrap().needs();
        let segmentation = {
            let b = self.blur.lock().unwrap();
            b.radius > 0 || b.background.is_some()
        } || behind;
        let face = self.zoom.lock().unwrap().enabled || follow;
        if on[0] != segmentation {
            self.set_branch(Branch::Segmentation, segmentation)?;
            on[0] = segmentation;
            if !segmentation {
                self.blur.lock().unwrap().mask = None;
            }
        }
        if on[1] != face {
            self.set_branch(Branch::Face, face)?;
            on[1] = face;
        }
        Ok(())
    }

    /// Writes the stickers file.
    fn save_stickers(&self) {
        let path = self.settings.lock().unwrap().stickers_path();
        let text = self.stickers.lock().unwrap().text();
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("{}: {e}", path.display());
        }
    }

    /// Rebuilds the colour table after the look or an adjustment changed.
    fn rebuild_colour(&self) {
        let look = self.look.lock().unwrap();
        let adjust = self.adjust.lock().unwrap();
        *self.colour_lut.lock().unwrap() = (look.is_some() || !adjust.is_neutral())
            .then(|| Arc::new(YuvLut::new(&adjust.cube(look.as_ref().map(|l| (&l.cube, l.amount))))));
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
            self.link.write(self.led, &self.streaming_led().packet())?;
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

    /// White while video goes out; blue while the video is off and the camera only watches for gestures.
    fn streaming_led(&self) -> Led {
        if self.video_off.load(Ordering::Relaxed) {
            Led::Blue
        } else {
            Led::White
        }
    }

    fn set_video_off(&self, off: bool) -> Result<()> {
        self.video_off.store(off, Ordering::Relaxed);
        if self.state.lock().unwrap().streaming {
            self.link.write(self.led, &self.streaming_led().packet())?;
        }
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
                (!self.has_output && !self.low_power) || st.last_consumer.is_some_and(|t| t.elapsed() < OFF_DELAY)
            }
        } && !(self.locked.load(Ordering::Relaxed) && self.lock_pause.load(Ordering::Relaxed));
        if want && self.frozen.load(Ordering::Relaxed) {
            bail!("camera stopped responding");
        }
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
            ["status"] => Ok(self.status_line()),
            ["zoom", amount] => {
                let amount: Option<f32> = match *amount {
                    "off" => None,
                    "on" => Some(0.5),
                    a => Some(a.parse::<f32>()?.clamp(0.0, 1.0)),
                };
                {
                    let mut zoom = self.zoom.lock().unwrap();
                    zoom.enabled = amount.is_some();
                    if let Some(a) = amount {
                        zoom.framing.amount = a;
                    }
                }
                self.sync_branches()?;
                Ok(String::new())
            }
            ["gestures", on @ ("on" | "off")] => {
                let on = *on == "on";
                let was = {
                    let mut h = self.hands.lock().unwrap();
                    let was = h.enabled;
                    (h.enabled, h.seen, h.pending) = (on, false, None);
                    was
                };
                // Written with no lock held: a write can wait on the camera.
                if on != was {
                    self.set_branch(Branch::Palm, on)?;
                }
                if !on {
                    self.set_video_off(false)?;
                }
                Ok(String::new())
            }
            ["lockpause", on @ ("on" | "off")] => {
                self.lock_pause.store(*on == "on", Ordering::Relaxed);
                self.update(None)?;
                Ok(String::new())
            }
            ["gesture", which @ ("peace" | "pinch"), on @ ("on" | "off")] => {
                let mut h = self.hands.lock().unwrap();
                let on = *on == "on";
                if *which == "peace" {
                    h.peace = on;
                    drop(h);
                    if !on {
                        self.set_video_off(false)?;
                    }
                } else {
                    h.pinch = on;
                }
                Ok(String::new())
            }
            ["video", on @ ("on" | "off")] => {
                self.set_video_off(*on == "off")?;
                Ok(String::new())
            }
            ["touchup", amount] => {
                self.skin.lock().unwrap().smooth = match *amount {
                    "off" => 0.0,
                    a => percent(a)?,
                };
                Ok(String::new())
            }
            ["tone", "off"] => {
                let mut s = self.skin.lock().unwrap();
                (s.even, s.warmth) = (0.0, 0.0);
                Ok(String::new())
            }
            ["tone", even, warmth] => {
                let (even, warmth) = (percent(even)?, warmth.parse::<f32>()?);
                anyhow::ensure!((-100.0..=100.0).contains(&warmth), "warmth -100 to 100");
                let mut s = self.skin.lock().unwrap();
                (s.even, s.warmth) = (even, warmth / 100.0);
                Ok(String::new())
            }
            ["sticker", "add", rest @ ..] => {
                let id = self.stickers.lock().unwrap().add(rest)?;
                self.sync_branches()?;
                self.save_stickers();
                Ok(id.to_string())
            }
            ["sticker", "move", id, x, y, size] => {
                {
                    let mut st = self.stickers.lock().unwrap();
                    let s = st.get(id)?;
                    let (x, y, size): (f32, f32, f32) = (x.parse()?, y.parse()?, size.parse()?);
                    anyhow::ensure!(x.is_finite() && y.is_finite(), "bad position");
                    anyhow::ensure!((0.01..=1.0).contains(&size), "size 0.01 to 1");
                    (s.x, s.y, s.size) = (x.clamp(-1.0, 2.0), y.clamp(-1.0, 2.0), size);
                }
                self.save_stickers();
                Ok(String::new())
            }
            ["sticker", "flags", id, flags] => {
                {
                    let mut st = self.stickers.lock().unwrap();
                    let s = st.get(id)?;
                    (s.behind, s.follow) = (flags.contains('b'), flags.contains('f'));
                }
                self.sync_branches()?;
                self.save_stickers();
                Ok(String::new())
            }
            ["sticker", "remove", id] => {
                {
                    let mut st = self.stickers.lock().unwrap();
                    let id = st.get(id)?.id;
                    st.list.retain(|s| s.id != id);
                }
                self.sync_branches()?;
                self.save_stickers();
                Ok(String::new())
            }
            ["sticker", "clear"] => {
                self.stickers.lock().unwrap().list.clear();
                self.sync_branches()?;
                self.save_stickers();
                Ok(String::new())
            }
            ["sticker", "list"] => Ok(self.stickers.lock().unwrap().describe()),
            ["loop", action @ ("record" | "stop" | "play" | "bounce" | "off")] => {
                let mut c = self.clip.lock().unwrap();
                match *action {
                    "record" => {
                        c.frames.clear();
                        (c.recording, c.playing) = (Some(Instant::now()), None);
                    }
                    "stop" => c.recording = None,
                    "off" => (c.recording, c.playing) = (None, None),
                    play => {
                        anyhow::ensure!(!c.frames.is_empty(), "nothing recorded");
                        (c.recording, c.playing, c.step) = (None, Some(play == "bounce"), 0);
                    }
                }
                Ok(String::new())
            }
            ["pixelate", amount] => {
                *self.pixelate.lock().unwrap() = match *amount {
                    "off" => 0.0,
                    a => a.parse::<f32>()?.clamp(0.0, 100.0) / 100.0,
                };
                Ok(String::new())
            }
            ["magnify", factor] => {
                *self.magnify.lock().unwrap() = match *factor {
                    "off" => 1.0,
                    f => f.parse::<f32>()?.clamp(1.0, 4.0),
                };
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
                let res = Resolution::parse(name).ok_or_else(|| anyhow!("resolution 720p, 1080p, 1440p or 2160p"))?;
                if res != self.resolution {
                    RESTART.store(true, Ordering::Relaxed);
                }
                Ok(String::new())
            }
            ["cameras"] => Ok(crate::output::describe(self.outputs.lock().unwrap().as_ref())),
            ["cameras", "release"] => {
                if let Some(o) = self.outputs.lock().unwrap().as_mut() {
                    o.release();
                }
                Ok(String::new())
            }
            ["cameras", "open"] => {
                if let Some(o) = self.outputs.lock().unwrap().as_mut() {
                    o.reopen();
                }
                Ok(String::new())
            }
            ["settings"] => Ok(self.settings.lock().unwrap().commands().collect::<Vec<_>>().join("; ")),
            ["preset", "list"] => {
                let dir = self.settings.lock().unwrap().preset_dir();
                let mut names: Vec<String> = std::fs::read_dir(&dir)
                    .map(|d| {
                        d.filter_map(|e| e.ok()?.file_name().into_string().ok())
                            .filter(|n| crate::settings::preset_name(n).is_some())
                            .collect()
                    })
                    .unwrap_or_default();
                names.sort();
                Ok(names.join(" "))
            }
            ["preset", action @ ("save" | "load" | "delete"), name] => {
                let name = crate::settings::preset_name(name)
                    .ok_or_else(|| anyhow!("preset names use letters, digits, - and _"))?;
                let (dir, text) = {
                    let s = self.settings.lock().unwrap();
                    (s.preset_dir(), s.preset_text())
                };
                let path = dir.join(name);
                let stickers = dir.join(format!("{name}.stickers"));
                match *action {
                    "save" => {
                        std::fs::create_dir_all(&dir)?;
                        std::fs::write(&path, text)?;
                        std::fs::write(stickers, self.stickers.lock().unwrap().text())?;
                    }
                    "delete" => {
                        std::fs::remove_file(&path).with_context(|| format!("no preset {name}"))?;
                        let _ = std::fs::remove_file(stickers);
                    }
                    _ => {
                        let text = std::fs::read_to_string(&path).with_context(|| format!("no preset {name}"))?;
                        for c in crate::settings::preset_commands(&text) {
                            if let Err(e) = self.command(&c) {
                                eprintln!("preset {name}: {c}: {e}");
                            }
                        }
                        let saved = std::fs::read_to_string(stickers).unwrap_or_default();
                        self.stickers.lock().unwrap().load(&saved);
                        self.sync_branches()?;
                        self.save_stickers();
                    }
                }
                Ok(String::new())
            }
            ["lut", "off"] => {
                *self.look.lock().unwrap() = None;
                self.rebuild_colour();
                Ok(String::new())
            }
            ["lut", path, rest @ ..] => {
                let amount = match rest {
                    [] => 1.0,
                    [a] => a.parse()?,
                    _ => bail!("usage: lut <look or file.cube> [amount]"),
                };
                *self.look.lock().unwrap() = Some(load_look(path, amount)?);
                self.rebuild_colour();
                Ok(String::new())
            }
            ["adjust", "reset"] => {
                *self.adjust.lock().unwrap() = Adjust::default();
                self.rebuild_colour();
                Ok(String::new())
            }
            [
                name @ ("warmth" | "tint" | "shadows" | "highlights" | "vibrance"),
                value,
            ] => {
                let name = if *name == "warmth" { "temperature" } else { name };
                self.adjust
                    .lock()
                    .unwrap()
                    .set(name, &[value.parse()?])
                    .ok_or_else(|| anyhow!("usage: {name} -100 to 100"))?;
                self.rebuild_colour();
                Ok(String::new())
            }
            ["colour", name, hue, sat, light] => {
                let values = [hue.parse()?, sat.parse()?, light.parse()?];
                self.adjust
                    .lock()
                    .unwrap()
                    .set(name, &values)
                    .ok_or_else(|| anyhow!("usage: colour <red, orange, yellow, green, aqua, blue, purple or magenta> <hue> <saturation> <lightness>"))?;
                self.rebuild_colour();
                Ok(String::new())
            }
            ["background", "off"] => {
                self.blur.lock().unwrap().background = None;
                self.sync_branches()?;
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
                self.blur.lock().unwrap().background = Some((name, Arc::new(image)));
                self.sync_branches()?;
                Ok(String::new())
            }
            ["bokeh", style @ ("soft" | "lens")] => {
                self.blur.lock().unwrap().lens = *style == "lens";
                Ok(String::new())
            }
            ["blur", radius] => {
                let radius: usize = match *radius {
                    "off" => 0,
                    "on" => DEFAULT_BLUR,
                    r => r.parse()?,
                };
                self.blur.lock().unwrap().radius = radius;
                self.sync_branches()?;
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
                let on = {
                    let mut mic = self.mic_state.lock().unwrap();
                    mic.1 = db;
                    mic.0
                };
                if on {
                    self.link.write(self.mic, &AudioInConfig::gain_db(db).to_packet())?;
                }
                Ok(String::new())
            }
            ["mic", on @ ("on" | "off")] => {
                let config = {
                    let mut mic = self.mic_state.lock().unwrap();
                    mic.0 = *on == "on";
                    if mic.0 {
                        AudioInConfig::gain_db(mic.1)
                    } else {
                        AudioInConfig::muted()
                    }
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

/// A percentage from 0 to 100 as 0 to 1.
fn percent(v: &str) -> Result<f32> {
    let v: f32 = v.parse()?;
    anyhow::ensure!((0.0..=100.0).contains(&v), "0 to 100");
    Ok(v / 100.0)
}

/// The middle 1/`factor` of a `w` x `h` frame.
fn centre_crop(w: usize, h: usize, factor: f32) -> opal_effects::Rect {
    let (cw, ch) = (w as f32 / factor, h as f32 / factor);
    opal_effects::Rect {
        x: (w as f32 - cw) / 2.0,
        y: (h as f32 - ch) / 2.0,
        width: cw,
        height: ch,
    }
}

/// Device log level for `OPAL_RS_LOG`.
fn log_level(name: &str) -> Option<i32> {
    ["trace", "debug", "info", "warn", "error", "critical", "off"]
        .iter()
        .position(|&l| l == name)
        .map(|i| i as i32)
}

/// Commands for effects, which low power mode does not have.
fn is_effect(command: &str) -> bool {
    matches!(
        command,
        "blur"
            | "background"
            | "lut"
            | "adjust"
            | "warmth"
            | "tint"
            | "shadows"
            | "highlights"
            | "vibrance"
            | "colour"
            | "zoom"
            | "magnify"
            | "pixelate"
            | "touchup"
            | "sticker"
            | "bokeh"
            | "loop"
            | "tone"
            | "tracking"
            | "gestures"
            | "gesture"
            | "video"
            | "snap"
    )
}

/// False on Windows 10, which has no virtual camera.
fn virtual_camera_available() -> bool {
    #[cfg(windows)]
    return opal_vcam::host::available();
    #[cfg(unix)]
    true
}

/// Largest preview width; the latest frame is averaged down to about this.
const PREVIEW_WIDTH: usize = 640;

/// Replies with the latest frame shrunk for the control panel: `bytes <n> <width> <height>`, then NV12. The panel
/// counts as an app using the camera while it asks.
fn send_preview(session: &Session, mut stream: opal_ipc::Stream) -> Result<()> {
    let streaming = {
        let mut st = session.state.lock().unwrap();
        st.last_preview = Some(Instant::now());
        st.streaming
    };
    let (w, h) = session.resolution.size();
    let (w, h) = (w as usize, h as usize);
    let small = {
        let last = session.last_frame.lock().unwrap();
        (streaming && last.len() >= w * h * 3 / 2).then(|| shrink_nv12(&last, w, h, w.div_ceil(PREVIEW_WIDTH)))
    };
    match small {
        Some((data, sw, sh)) => {
            writeln!(stream, "bytes {} {sw} {sh}", data.len())?;
            stream.write_all(&data)?;
        }
        None => writeln!(stream, "error: no frame yet")?,
    }
    Ok(())
}

/// Averages `step` x `step` blocks of a `w` x `h` NV12 frame. Returns the frame and its even width and height.
fn shrink_nv12(frame: &[u8], w: usize, h: usize, step: usize) -> (Vec<u8>, usize, usize) {
    let (sw, sh) = ((w / step) & !1, (h / step) & !1);
    let mut out = vec![0u8; sw * sh * 3 / 2];
    let n = (step * step) as u32;
    for y in 0..sh {
        for x in 0..sw {
            let sum: u32 = (0..step)
                .flat_map(|dy| &frame[(y * step + dy) * w + x * step..][..step])
                .map(|&p| p as u32)
                .sum();
            out[y * sw + x] = ((sum + n / 2) / n) as u8;
        }
    }
    // Chroma: each small UV pair takes the source pair at the centre of its block.
    let chroma = &frame[w * h..];
    for y in 0..sh / 2 {
        let sy = (y * step + step / 2).min(h / 2 - 1);
        for x in 0..sw / 2 {
            let sx = (x * step + step / 2).min(w / 2 - 1);
            out[sw * sh + y * sw + 2 * x] = chroma[sy * w + 2 * sx];
            out[sw * sh + y * sw + 2 * x + 1] = chroma[sy * w + 2 * sx + 1];
        }
    }
    (out, sw, sh)
}

fn handle(session: &Session, stream: opal_ipc::Stream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let line = line.trim();
    if line == "preview" {
        return send_preview(session, stream);
    }
    let reply = if line == "quit" {
        crate::request_stop();
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
    let opened = if low_power {
        None
    } else {
        crate::output::open(output, vw as usize, vh as usize)?
    };
    let (outputs, _registration) = match opened {
        Some((o, r)) => (Some(o), Some(r)),
        None => (None, None),
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
        has_output: outputs.is_some(),
        state: Mutex::new(State {
            mode: Mode::Auto,
            streaming: false,
            consumers: 0,
            apps: Vec::new(),
            last_consumer: None,
            last_preview: None,
            last_change: Instant::now() - RESTART_AFTER,
            start_frames: 0,
            first_frame: None,
        }),
        blur: Mutex::new(Blur::default()),
        look: Mutex::new(None),
        adjust: Mutex::new(Adjust::default()),
        colour_lut: Mutex::new(None),
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
            peace: true,
            pinch: true,
        }),
        mic_state: Mutex::new((true, 0.0)),
        video_off: AtomicBool::new(false),
        magnify: Mutex::new(1.0),
        pixelate: Mutex::new(0.0),
        skin: Mutex::new(opal_effects::Skin::default()),
        stickers: Mutex::new(crate::stickers::Stickers::new()),
        clip: Mutex::new(Clip::default()),
        face: Mutex::new(None),
        branch_on: Mutex::new([false; 2]),
        info,
        frozen: AtomicBool::new(false),
        locked: AtomicBool::new(false),
        lock_pause: AtomicBool::new(true),
        settings: Mutex::new(settings),
        frames: AtomicU64::new(0),
        temperature: Mutex::new(None),
        last_frame: Mutex::new(Vec::new()),
        outputs: Mutex::new(outputs),
        alive: AtomicBool::new(true),
    });
    session.link.write(session.led, &Led::Off.packet())?;
    for b in Branch::ALL {
        session.set_branch(b, false)?;
    }
    let saved: Vec<String> = session.settings.lock().unwrap().commands().map(str::to_owned).collect();
    for c in saved.iter().chain(overrides) {
        let first = c.split_whitespace().next();
        if low_power && first.is_some_and(is_effect) {
            continue;
        }
        // Already applied when the camera was booted; replaying them could ask for a restart that never settles.
        if matches!(first, Some("resolution" | "power")) {
            continue;
        }
        if let Err(e) = session.run_command(c) {
            eprintln!("{c}: {e}");
        }
    }
    if !low_power {
        let path = session.settings.lock().unwrap().stickers_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            session.stickers.lock().unwrap().load(&text);
            session.sync_branches()?;
        }
    }

    {
        let session = session.clone();
        std::thread::spawn(move || {
            let (w, h) = (vw as usize, vh as usize);
            let mut effects = BackgroundBlur::new(w, h);
            let placard = opal_effects::placard(w, h);
            let mut touch_up = opal_effects::TouchUp::new(w, h);
            // 0 shows the video, 1 the camera-off picture.
            let (mut off_level, mut last) = (0f32, Instant::now());
            let mut frame = Vec::new();
            let mut zoomed = Vec::new();
            while let Ok(packet) = session.link.read(opal_c1::VIDEO_STREAM, None) {
                let Some((data, IMG_FRAME, _)) = message::split(&packet) else {
                    continue;
                };
                frame.clear();
                frame.extend_from_slice(data);
                let skin = *session.skin.lock().unwrap();
                touch_up.apply(&mut frame, skin);
                let lut = session.colour_lut.lock().unwrap().clone();
                if let (Some(lut), true) = (lut, frame.len() >= w * h * 3 / 2) {
                    lut.apply(&mut frame, w, h, 1.0);
                }
                let (radius, mask, background) = {
                    let blur = session.blur.lock().unwrap();
                    effects.lens = blur.lens;
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
                    // The radius is in pixels of a 1080p picture, so it looks the same at every size.
                    effects.apply(&mut frame, mask, (r * h / 1080).max(1));
                }
                let crop = {
                    let mut zoom = session.zoom.lock().unwrap();
                    let crop = zoom.framing.step();
                    let m = *session.magnify.lock().unwrap();
                    if zoom.enabled {
                        (!zoom.framing.is_full()).then_some(crop)
                    } else {
                        (m > 1.0).then(|| centre_crop(w, h, m))
                    }
                };
                let shown = crop.unwrap_or(opal_effects::Rect::full(w, h));
                if let (Some(crop), true) = (crop, frame.len() >= w * h * 3 / 2) {
                    zoomed.resize(frame.len(), 0);
                    crop_scale(&frame, &mut zoomed, w, h, crop);
                    std::mem::swap(&mut frame, &mut zoomed);
                }
                let block = opal_effects::pixel_block(*session.pixelate.lock().unwrap(), h);
                if block > 0 && frame.len() >= w * h * 3 / 2 {
                    opal_effects::pixelate(&mut frame, w, h, block);
                }
                {
                    let mut st = session.stickers.lock().unwrap();
                    if !st.list.is_empty() && frame.len() >= w * h * 3 / 2 {
                        let face = session
                            .face
                            .lock()
                            .unwrap()
                            .filter(|(_, t)| t.elapsed() < Duration::from_secs(1))
                            .map(|(c, _)| c);
                        // The mask copied above; locking the blur again here, inside the stickers lock, could deadlock.
                        let scene = crate::stickers::Scene {
                            crop: shown,
                            face,
                            mask: if st.needs().0 { mask.as_ref() } else { None },
                        };
                        st.draw(&mut frame, w, h, &scene);
                    }
                }
                {
                    let mut c = session.clip.lock().unwrap();
                    if frame.len() >= w * h * 3 / 2 {
                        if let Some(started) = c.recording {
                            if started.elapsed() < LOOP_MAX && (c.frames.len() + 1) * (w * h * 3 / 8) <= LOOP_BYTES {
                                c.frames.push(opal_effects::halve(&frame, w, h));
                            } else {
                                c.recording = None;
                            }
                        } else if let Some(bounce) = c.playing
                            && !c.frames.is_empty()
                        {
                            let i = opal_effects::clip_frame(c.step, c.frames.len(), bounce);
                            c.step += 1;
                            opal_effects::double(&c.frames[i], &mut frame, w, h);
                        }
                    }
                }
                let step = last.elapsed().as_secs_f32() / FADE.as_secs_f32();
                last = Instant::now();
                off_level = if session.video_off.load(Ordering::Relaxed) {
                    (off_level + step).min(1.0)
                } else {
                    (off_level - step).max(0.0)
                };
                if frame.len() == placard.len() {
                    if off_level >= 1.0 {
                        frame.copy_from_slice(&placard);
                    } else if off_level > 0.0 {
                        opal_effects::fade(&mut frame, &placard, off_level);
                    }
                }
                if let Some(out) = session.outputs.lock().unwrap().as_mut()
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
                    *session.face.lock().unwrap() = Some(((cx, cy), Instant::now()));
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
                let found = {
                    let mut h = session.hands.lock().unwrap();
                    if !h.enabled || h.pending.is_some_and(|(_, t)| t.elapsed() < CROP_TIMEOUT) {
                        continue;
                    }
                    let scores = nn.tensor_f32(data, "classificators").unwrap_or_default();
                    let regressors = nn.tensor_f32(data, "regressors").unwrap_or_default();
                    h.seen = false;
                    let found = view.palm(&scores, &regressors);
                    h.pending = found.map(|b| (b, Instant::now()));
                    found
                };
                // Written with no lock held: a write can wait on the camera.
                if let Some(b) = found
                    && session.link.write(crop_in, &view.crop(&b).to_packet()).is_err()
                {
                    break;
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
                    let next = hand.as_ref().filter(|_| h.enabled).map(hands::follow);
                    h.pending = next.map(|n| (n, Instant::now()));
                    let fired = h.gestures.update(hand.as_ref().and_then(|x| x.gesture(aspect)));
                    if fired.is_some() {
                        h.last = fired;
                    }
                    (fired, next)
                };
                let (fired, next) = fired;
                // Written with no lock held: a write can wait on the camera.
                if let Some(next) = next
                    && session.link.write(crop_in, &view.crop(&next).to_packet()).is_err()
                {
                    session.hands.lock().unwrap().pending = None;
                }
                let (peace, pinch) = {
                    let h = session.hands.lock().unwrap();
                    (h.peace, h.pinch)
                };
                let command = match fired {
                    Some(Gesture::Peace) if peace => {
                        let off = !session.video_off.load(Ordering::Relaxed);
                        if let Err(e) = session.set_video_off(off) {
                            eprintln!("video: {e}");
                        }
                        None
                    }
                    Some(Gesture::Pinch) if pinch => {
                        let zoom = session.zoom.lock().unwrap();
                        Some(if zoom.enabled {
                            "zoom off".to_owned()
                        } else {
                            format!("zoom {:.2}", zoom.framing.amount)
                        })
                    }
                    _ => None,
                };
                if let Some(c) = command
                    && let Err(e) = session.command(&c)
                {
                    eprintln!("{c}: {e}");
                }
            }
        });
    }

    let listener = opal_ipc::Listener::bind().with_context(|| opal_ipc::location().display().to_string())?;
    println!("running");

    let mut result = Ok(());
    // Asked off the main loop: a camera that does not answer would hold up the control socket.
    let dev = Arc::new(dev);
    let done = Arc::new(AtomicBool::new(false));
    let temperature = {
        let (dev, session, done) = (dev.clone(), session.clone(), done.clone());
        std::thread::spawn(move || {
            let mut last = Instant::now() - TEMPERATURE_PERIOD;
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(POLL);
                if last.elapsed() >= TEMPERATURE_PERIOD && !session.frozen.load(Ordering::Relaxed) {
                    last = Instant::now();
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
            }
        })
    };
    let screen_lock = crate::platform::ScreenLock::new();
    let mut last_lock = Instant::now() - LOCK_PERIOD;
    let mut last_refresh = Instant::now();
    while !crate::stopped() && !RESTART.load(Ordering::Relaxed) {
        // A camera that stopped answering is kept until video is wanted (see `update`).
        if !session.alive.load(Ordering::Relaxed) && !session.frozen.load(Ordering::Relaxed) {
            result = Err(anyhow!("camera disconnected"));
            break;
        }
        if session.link.quiet_for() > SILENCE && !session.frozen.swap(true, Ordering::Relaxed) {
            eprintln!("camera stopped responding; it is rebooted when video is next wanted");
            // Closing the link makes every write to the camera fail at once instead of waiting forever.
            session.link.close("camera stopped responding");
        }
        if last_lock.elapsed() >= LOCK_PERIOD {
            last_lock = Instant::now();
            if let Some(locked) = screen_lock.locked() {
                session.locked.store(locked, Ordering::Relaxed);
            }
        }
        let users = if low_power {
            Some(crate::output::low_power_users(&mut session.camera_node.lock().unwrap()))
        } else {
            if last_refresh.elapsed() >= REFRESH_PERIOD {
                last_refresh = Instant::now();
                if let Some(o) = session.outputs.lock().unwrap().as_mut() {
                    o.refresh();
                }
            }
            // Scanned without holding the outputs, which the frame thread needs.
            let paths = session.outputs.lock().unwrap().as_ref().map(|o| o.watch_paths());
            paths.and_then(|paths| {
                let found = crate::output::scan_users(&paths);
                let mut outputs = session.outputs.lock().unwrap();
                let o = outputs.as_mut()?;
                o.set_users(&paths, found);
                let names = o.users();
                Some((names.len(), names))
            })
        };
        // The control panel takes its preview from here rather than from the output, which v4l2loopback lets
        // only one app read.
        let previewing = !low_power
            && session
                .state
                .lock()
                .unwrap()
                .last_preview
                .is_some_and(|t| t.elapsed() < OFF_DELAY);
        let users = match users {
            Some((n, mut names)) if previewing => {
                names.push("opal-rs-gui".into());
                Some((n + 1, names))
            }
            None if previewing => Some((1, vec!["opal-rs-gui".into()])),
            u => u,
        };
        let count = users.as_ref().map(|u| u.0);
        if let Some((_, mut apps)) = users {
            apps.sort();
            apps.dedup();
            session.state.lock().unwrap().apps = apps;
        }
        if let Err(e) = session.update(count) {
            result = Err(e);
            break;
        }
        // Waits out the poll period, answering requests as they come so the preview keeps up.
        let until = Instant::now() + POLL;
        while result.is_ok() && Instant::now() < until {
            match listener.accept() {
                Ok(Some(stream)) => {
                    if let Err(e) = handle(&session, stream) {
                        eprintln!("control: {e}");
                    }
                }
                Ok(None) => std::thread::sleep(ACCEPT_POLL),
                Err(e) => result = Err(e.into()),
            }
        }
        if result.is_err() {
            break;
        }
    }
    done.store(true, Ordering::Relaxed);
    let _ = temperature.join();
    drop(listener);
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
    let mut stream =
        opal_ipc::connect().with_context(|| format!("no running session at {}", opal_ipc::location().display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut words = words.to_vec();
    // Paths are resolved by the daemon, which runs in another directory.
    if let [cmd, add, kind, _, _, _, _, path] = words.as_mut_slice()
        && (cmd.as_str(), add.as_str(), kind.as_str()) == ("sticker", "add", "picture")
    {
        *path = std::path::absolute(&*path)?.display().to_string();
    }
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
    fn centre_crop_is_centred() {
        let r = centre_crop(1920, 1080, 2.0);
        assert_eq!((r.x, r.y, r.width, r.height), (480.0, 270.0, 960.0, 540.0));
        let r = centre_crop(1280, 720, 1.0);
        assert_eq!((r.x, r.y, r.width, r.height), (0.0, 0.0, 1280.0, 720.0));
    }

    #[test]
    fn preview_shrink_keeps_flat_frames() {
        let (w, h) = (24, 12);
        let mut f = vec![80u8; w * h];
        f.extend([100u8, 150].repeat(w * h / 4));
        let (small, sw, sh) = shrink_nv12(&f, w, h, 3);
        assert_eq!((sw, sh), (8, 4));
        assert!(small[..sw * sh].iter().all(|&p| p == 80));
        assert_eq!(&small[sw * sh..sw * sh + 2], [100, 150]);
    }

    #[test]
    fn antibanding_commands() {
        assert!(parse_control("antibanding 50").is_some());
        assert!(parse_control("antibanding auto").is_some());
        assert!(parse_control("antibanding 70").is_none());
        assert!(parse_control("antibanding").is_none());
    }
}
