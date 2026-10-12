//! Control panel for the opal-rs camera service.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod debug;
mod overlay;
mod preview;
mod theme;
mod view;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;

const VIDEO: &str = "/dev/video10";
const RESOLUTIONS: [(&str, usize, usize); 4] = [
    ("720p", 1280, 720),
    ("1080p", 1920, 1080),
    ("1440p", 2560, 1440),
    ("2160p", 3840, 2160),
];

fn video_size(name: &str) -> (usize, usize) {
    RESOLUTIONS
        .iter()
        .find(|r| r.0 == name)
        .map_or((1920, 1080), |r| (r.1, r.2))
}

/// Sends one command to the service and returns its reply.
pub(crate) fn send(command: &str) -> Result<String, String> {
    let reply = opal_ipc::request(command, Duration::from_secs(5)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            "the opal-rs service is not running".to_owned()
        }
        _ => e.to_string(),
    })?;
    match reply.strip_prefix("error: ") {
        Some(e) => Err(e.to_owned()),
        None => Ok(reply),
    }
}

/// Lighting adjustment commands, in the order of `Controls::lighting`.
pub const LIGHTING: [&str; 5] = ["warmth", "tint", "shadows", "highlights", "vibrance"];
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    On,
    Off,
}

/// Values shown in the panel, read from the service's saved settings.
struct Controls {
    auto_focus: bool,
    focus: i32,
    auto_exposure: bool,
    exposure_us: i32,
    iso: i32,
    ev: i32,
    auto_wb: bool,
    wb: i32,
    brightness: i32,
    contrast: i32,
    saturation: i32,
    sharpness: i32,
    gain: f32,
    mic: bool,
    blur: bool,
    blur_radius: i32,
    background: String,
    background_on: bool,
    look: String,
    look_on: bool,
    look_amount: f32,
    zoom: bool,
    zoom_amount: f32,
    tracking: String,
    gestures: bool,
    peace: bool,
    pinch: bool,
    lock_pause: bool,
    low_power: bool,
    resolution: String,
    antibanding: String,
    magnify: f32,
    /// Pixelation, 0 for off to 100.
    pixelate: i32,
    /// Lens (hexagonal) background blur instead of the smooth one.
    lens: bool,
    /// Skin smoothing, even tone (0 to 100) and skin warmth (-100 to 100).
    touchup: i32,
    tone: [i32; 2],
    /// Warmth, tint, shadows, highlights and vibrance, -100 to 100.
    lighting: [i32; 5],
    /// Hue, saturation and lightness change per colour, -100 to 100.
    colours: [[i32; 3]; 8],
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            auto_focus: true,
            focus: 130,
            auto_exposure: true,
            exposure_us: 10000,
            iso: 400,
            ev: 0,
            auto_wb: true,
            wb: 5000,
            brightness: 0,
            contrast: 0,
            saturation: 0,
            sharpness: 1,
            gain: 0.0,
            mic: true,
            blur: false,
            blur_radius: 24,
            background: String::new(),
            background_on: false,
            look: opal_effects::LOOKS[0].to_owned(),
            look_on: false,
            look_amount: 1.0,
            zoom: false,
            zoom_amount: 0.5,
            tracking: "normal".into(),
            gestures: false,
            peace: true,
            pinch: true,
            lock_pause: true,
            low_power: false,
            resolution: "1080p".into(),
            antibanding: "auto".into(),
            magnify: 1.0,
            pixelate: 0,
            lens: false,
            touchup: 0,
            tone: [0, 0],
            lighting: [0; 5],
            colours: [[0; 3]; 8],
        }
    }
}

impl Controls {
    fn from_settings(saved: &str) -> Self {
        let mut c = Self::default();
        for cmd in saved.split(';').map(str::trim) {
            let w: Vec<&str> = cmd.split_whitespace().collect();
            let int = |i: usize| w.get(i).and_then(|v| v.parse::<i32>().ok());
            match w.as_slice() {
                ["focus", _] => (c.auto_focus, c.focus) = (false, int(1).unwrap_or(c.focus)),
                ["af"] => c.auto_focus = true,
                ["exposure", _, _] => {
                    c.auto_exposure = false;
                    c.exposure_us = int(1).unwrap_or(c.exposure_us);
                    c.iso = int(2).unwrap_or(c.iso);
                }
                ["ae"] => c.auto_exposure = true,
                ["wb", _] => (c.auto_wb, c.wb) = (false, int(1).unwrap_or(c.wb)),
                ["awb"] => c.auto_wb = true,
                ["ev", _] => c.ev = int(1).unwrap_or(0),
                ["brightness", _] => c.brightness = int(1).unwrap_or(0),
                ["contrast", _] => c.contrast = int(1).unwrap_or(0),
                ["saturation", _] => c.saturation = int(1).unwrap_or(0),
                ["sharpness", _] => c.sharpness = int(1).unwrap_or(1),
                ["gain", db] => c.gain = db.parse().unwrap_or(0.0),
                ["mic", m] => c.mic = *m != "off",
                ["blur", "off" | "0"] => c.blur = false,
                ["blur", "on"] => c.blur = true,
                ["blur", _] => (c.blur, c.blur_radius) = (true, int(1).unwrap_or(c.blur_radius)),
                ["background", "off"] => c.background_on = false,
                ["background", path] => (c.background, c.background_on) = ((*path).to_owned(), true),
                ["lut", path, rest @ ..] => {
                    c.look = (*path).to_owned();
                    c.look_on = true;
                    c.look_amount = rest.first().and_then(|a| a.parse().ok()).unwrap_or(1.0);
                }
                ["zoom", "off"] => c.zoom = false,
                ["zoom", "on"] => c.zoom = true,
                ["zoom", a] => (c.zoom, c.zoom_amount) = (true, a.parse().unwrap_or(0.5)),
                ["tracking", s] => c.tracking = (*s).to_owned(),
                ["gestures", g] => c.gestures = *g == "on",
                ["gesture", "peace", g] => c.peace = *g == "on",
                ["gesture", "pinch", g] => c.pinch = *g == "on",
                ["lockpause", v] => c.lock_pause = *v == "on",
                ["power", p] => c.low_power = *p == "low",
                ["resolution", r] => c.resolution = (*r).to_owned(),
                ["antibanding", a] => c.antibanding = (*a).to_owned(),
                ["magnify", "off"] => c.magnify = 1.0,
                ["pixelate", "off"] => c.pixelate = 0,
                ["bokeh", style] => c.lens = *style == "lens",
                ["touchup", "off"] => c.touchup = 0,
                ["touchup", t] => c.touchup = t.parse().unwrap_or(0),
                ["tone", "off"] => c.tone = [0, 0],
                ["tone", _, _] => c.tone = [int(1).unwrap_or(0), int(2).unwrap_or(0)],
                ["pixelate", p] => c.pixelate = p.parse().unwrap_or(0),
                ["magnify", m] => c.magnify = m.parse().unwrap_or(1.0),
                [name, v] if LIGHTING.contains(name) => {
                    let i = LIGHTING.iter().position(|n| n == name).unwrap();
                    c.lighting[i] = v.parse().unwrap_or(0);
                }
                ["colour", name, _, _, _] => {
                    if let Some(i) = opal_effects::COLOURS.iter().position(|(n, _)| n == name) {
                        c.colours[i] = [int(2).unwrap_or(0), int(3).unwrap_or(0), int(4).unwrap_or(0)];
                    }
                }
                _ => {}
            }
        }
        c
    }

    fn blur_cmd(&self) -> String {
        format!("blur {}", if self.blur { self.blur_radius } else { 0 })
    }

    fn zoom_cmd(&self) -> String {
        if self.zoom {
            format!("zoom {:.2}", self.zoom_amount)
        } else {
            "zoom off".into()
        }
    }

    fn gestures_cmd(&self) -> String {
        format!("gestures {}", if self.gestures { "on" } else { "off" })
    }

    fn look_cmd(&self) -> String {
        format!("lut {} {:.2}", self.look, self.look_amount)
    }
}

/// Controls from the service's saved settings, and an error if it could not be asked.
pub(crate) fn load_settings() -> (Controls, String) {
    match send("settings") {
        Ok(s) if s != "ok" => (Controls::from_settings(&s), String::new()),
        Ok(_) => (Controls::default(), String::new()),
        Err(e) => (Controls::default(), e),
    }
}

/// Asks the service for its status once a second.
/// Saved presets and stickers as the service lists them, read in the background.
#[derive(Default)]
pub(crate) struct Lists {
    pub presets: String,
    pub stickers: String,
    /// Set by the window to ask for a fresh read; cleared once read.
    pub stale: bool,
    /// Bumped on every read, so the window knows to take the new lists.
    pub serial: u64,
}

/// Asks the service for its status once a second, and for the preset and sticker lists every few seconds or
/// when asked, so a slow service never stalls the window.
fn poll_status_thread(ctx: egui::Context, lists: Arc<Mutex<Lists>>) -> Arc<Mutex<Option<String>>> {
    let slot: Arc<Mutex<Option<String>>> = Arc::default();
    let out = slot.clone();
    std::thread::spawn(move || {
        let mut tick = 0u32;
        loop {
            let status = send("status").unwrap_or_else(|e| e);
            *slot.lock().unwrap() = Some(status);
            if tick.is_multiple_of(3) || lists.lock().unwrap().stale {
                let presets = send("preset list").unwrap_or_default();
                let stickers = send("sticker list").unwrap_or_default();
                let mut l = lists.lock().unwrap();
                (l.presets, l.stickers, l.stale) = (presets, stickers, false);
                l.serial += 1;
            }
            ctx.request_repaint();
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(100));
                if lists.lock().unwrap().stale {
                    break;
                }
            }
            tick += 1;
        }
    });
    out
}

struct App {
    c: Controls,
    mode: Mode,
    status: String,
    message: String,
    /// Newest status line, filled by a polling thread so a slow service never stalls the window.
    latest: Arc<Mutex<Option<String>>>,
    lists: Arc<Mutex<Lists>>,
    preview: preview::Handle,
    /// Keep the preview running, restarting it after errors.
    want_preview: bool,
    texture: Option<egui::TextureHandle>,
    debug: bool,
    view: view::State,
    /// The service answered the last status request.
    connected: bool,
    /// Preview aids, saved by the panel itself.
    zebra: bool,
    histogram: bool,
    bins: Option<[u32; overlay::BINS]>,
    /// The newest preview frame as shown, before zebra stripes, for the white picker.
    frame: Option<(Vec<u8>, [usize; 2])>,
}

impl App {
    fn new(cc: &eframe::CreationContext, debug: bool) -> Self {
        theme::apply(&cc.egui_ctx);
        let (c, message) = load_settings();
        let connected = message.is_empty();
        let view = view::State::new(&c);
        let (zebra, histogram) = overlay::load();
        let lists: Arc<Mutex<Lists>> = Arc::default();
        Self {
            c,
            mode: Mode::Auto,
            status: String::new(),
            message,
            latest: poll_status_thread(cc.egui_ctx.clone(), lists.clone()),
            lists,
            preview: preview::Handle::default(),
            want_preview: !debug,
            texture: None,
            debug,
            view,
            connected,
            zebra,
            histogram,
            bins: None,
            frame: None,
        }
    }

    fn apply(&mut self, command: String) {
        self.message = match send(&command) {
            Ok(_) => String::new(),
            Err(e) => format!("{command}: {e}"),
        };
    }

    /// Word following `key` in the status line.
    fn status_value(&self, key: &str) -> Option<&str> {
        let mut words = self.status.split_whitespace();
        words.find(|w| *w == key)?;
        words.next()
    }

    /// Opens the device apps see, or the camera itself in low power mode.
    fn start_preview(&mut self, ctx: &egui::Context) {
        let source = if self.c.low_power {
            (preview::camera_node(), video_size("1080p"))
        } else {
            (PathBuf::from(VIDEO), video_size(&self.c.resolution))
        };
        self.preview.start(ctx.clone(), source);
    }

    /// Moves the newest frame into the preview texture.
    fn update_texture(&mut self, ctx: &egui::Context) {
        if let Some((mut rgb, size)) = self.preview.take_frame() {
            self.bins = self.histogram.then(|| overlay::histogram(&rgb));
            if self.view.picking_white {
                self.frame = Some((rgb.clone(), size));
            }
            if self.zebra {
                overlay::zebra(&mut rgb, size);
            }
            let image = egui::ColorImage::from_rgb(size, &rgb);
            match &mut self.texture {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR)),
            }
        }
    }

    fn poll_status(&mut self) {
        let Some(status) = self.latest.lock().unwrap().take() else {
            return;
        };
        self.status = status;
        let connected = self.status.starts_with("mode ");
        if connected && !self.connected {
            // The service came back: show what it has saved.
            let (c, message) = load_settings();
            self.c = c;
            self.message = message;
            self.view = view::State::new(&self.c);
            self.preview.stop();
        }
        self.connected = connected;
        // A pinch can turn face following on or off from the camera.
        let mut words = self.status.split_whitespace();
        if words.any(|w| w == "zoom")
            && let Some(z) = words.next()
        {
            self.c.zoom = z != "off";
        }
        // The microphone can be switched elsewhere too.
        if let Some(m) = self.status_value("mic") {
            self.c.mic = m != "off";
        }
        self.mode = match self.status_value("mode") {
            Some("on") => Mode::On,
            Some("off") => Mode::Off,
            Some("auto") => Mode::Auto,
            _ => self.mode,
        };
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_status();
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        if self.debug {
            debug::show(self, ui);
        } else {
            view::show(self, ui);
        }
    }
}

fn main() -> eframe::Result {
    let debug = std::env::args().any(|a| a == "--debug");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Opal C1")
            .with_inner_size([600.0, 900.0])
            .with_min_inner_size([480.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "opal-rs-gui",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, debug)))),
    )
}
