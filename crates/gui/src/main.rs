//! Control panel for the opal-rs camera service.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

const VIDEO: &str = "/dev/video10";
/// Preview width in pixels; frames are sampled down to about this.
const PREVIEW_WIDTH: usize = 480;
const RESOLUTIONS: [(&str, usize, usize); 3] = [("720p", 1280, 720), ("1080p", 1920, 1080), ("1440p", 2560, 1440)];

fn video_size(name: &str) -> (usize, usize) {
    RESOLUTIONS
        .iter()
        .find(|r| r.0 == name)
        .map_or((1920, 1080), |r| (r.1, r.2))
}

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("opal-rs.sock")
}

/// Sends one command to the service and returns its reply.
fn send(command: &str) -> Result<String, String> {
    let mut s = UnixStream::connect(socket_path()).map_err(|_| "the opal-rs service is not running".to_owned())?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    writeln!(s, "{command}").map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(&s).read_line(&mut reply).map_err(|e| e.to_string())?;
    let reply = reply.trim_end().to_owned();
    match reply.strip_prefix("error: ") {
        Some(e) => Err(e.to_owned()),
        None => Ok(reply),
    }
}

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
    resolution: String,
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
            resolution: "1080p".into(),
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
                ["resolution", r] => c.resolution = (*r).to_owned(),
                _ => {}
            }
        }
        c
    }
}

/// Latest preview frame as RGB, filled by a capture thread.
#[derive(Default)]
struct Preview {
    rgb: Option<Vec<u8>>,
    size: [usize; 2],
    error: Option<String>,
}

/// Every `step`-th pixel of a `width` x `height` NV12 frame as RGB.
fn nv12_to_rgb_small(frame: &[u8], width: usize, height: usize, step: usize) -> Vec<u8> {
    let (w, h) = (width / step, height / step);
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (x * step, y * step);
            let luma = frame[sy * width + sx] as f32;
            let c = width * height + (sy / 2) * width + (sx / 2) * 2;
            let (u, v) = (frame[c] as f32 - 128.0, frame[c + 1] as f32 - 128.0);
            let px = [luma + 1.402 * v, luma - 0.344_136 * u - 0.714_136 * v, luma + 1.772 * u];
            out.extend(px.map(|p| p.clamp(0.0, 255.0) as u8));
        }
    }
    out
}

fn start_preview(
    shared: Arc<Mutex<Preview>>,
    running: Arc<Mutex<bool>>,
    ctx: egui::Context,
    (width, height): (usize, usize),
) {
    let step = (width / PREVIEW_WIDTH).max(1);
    shared.lock().unwrap().size = [width / step, height / step];
    std::thread::spawn(move || {
        let result = (|| -> std::io::Result<()> {
            let mut cap = opal_v4l2::Capture::open(Path::new(VIDEO), width as u32, height as u32, opal_v4l2::NV12)?;
            let mut last = Instant::now() - Duration::from_secs(1);
            while *running.lock().unwrap() {
                let rgb = cap.next(Duration::from_secs(5), |f| {
                    (last.elapsed() > Duration::from_millis(66) && f.data.len() >= width * height * 3 / 2)
                        .then(|| nv12_to_rgb_small(f.data, width, height, step))
                })?;
                if let Some(rgb) = rgb {
                    last = Instant::now();
                    shared.lock().unwrap().rgb = Some(rgb);
                    ctx.request_repaint();
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            shared.lock().unwrap().error = Some(format!("{VIDEO}: {e}"));
            ctx.request_repaint();
        }
        *running.lock().unwrap() = false;
    });
}

struct App {
    c: Controls,
    mode: Mode,
    status: String,
    message: String,
    last_status: Instant,
    preview: Arc<Mutex<Preview>>,
    preview_running: Arc<Mutex<bool>>,
    texture: Option<egui::TextureHandle>,
}

impl App {
    fn new() -> Self {
        let (c, message) = match send("settings") {
            Ok(s) if s != "ok" => (Controls::from_settings(&s), String::new()),
            Ok(_) => (Controls::default(), String::new()),
            Err(e) => (Controls::default(), e),
        };
        Self {
            c,
            mode: Mode::Auto,
            status: String::new(),
            message,
            last_status: Instant::now() - Duration::from_secs(10),
            preview: Arc::default(),
            preview_running: Arc::new(Mutex::new(false)),
            texture: None,
        }
    }

    fn apply(&mut self, command: String) {
        self.message = match send(&command) {
            Ok(_) => String::new(),
            Err(e) => format!("{command}: {e}"),
        };
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let mut cmds: Vec<String> = Vec::new();
        let c = &mut self.c;

        ui.heading("Camera");
        ui.horizontal(|ui| {
            ui.label("Resolution");
            egui::ComboBox::from_id_salt("resolution")
                .selected_text(&c.resolution)
                .show_ui(ui, |ui| {
                    for (name, _, _) in RESOLUTIONS {
                        if ui.selectable_value(&mut c.resolution, name.to_owned(), name).changed() {
                            cmds.push(format!("resolution {name}"));
                        }
                    }
                });
            ui.weak("restarts the camera");
        });
        ui.horizontal(|ui| {
            ui.label("Streaming");
            for (m, name) in [(Mode::Auto, "Auto"), (Mode::On, "On"), (Mode::Off, "Off")] {
                if ui.radio_value(&mut self.mode, m, name).changed() {
                    cmds.push(name.to_lowercase());
                }
            }
        });

        if ui.checkbox(&mut c.auto_focus, "Autofocus").changed() {
            cmds.push(if c.auto_focus {
                "af".into()
            } else {
                format!("focus {}", c.focus)
            });
        }
        if ui
            .add_enabled(!c.auto_focus, egui::Slider::new(&mut c.focus, 0..=255).text("Focus"))
            .changed()
        {
            cmds.push(format!("focus {}", c.focus));
        }

        if ui.checkbox(&mut c.auto_exposure, "Auto exposure").changed() {
            cmds.push(if c.auto_exposure {
                "ae".into()
            } else {
                format!("exposure {} {}", c.exposure_us, c.iso)
            });
        }
        ui.add_enabled_ui(!c.auto_exposure, |ui| {
            let a = ui.add(
                egui::Slider::new(&mut c.exposure_us, 100..=33000)
                    .logarithmic(true)
                    .text("Exposure (us)"),
            );
            let b = ui.add(egui::Slider::new(&mut c.iso, 100..=1600).text("ISO"));
            if a.changed() || b.changed() {
                cmds.push(format!("exposure {} {}", c.exposure_us, c.iso));
            }
        });
        if ui
            .add_enabled(
                c.auto_exposure,
                egui::Slider::new(&mut c.ev, -9..=9).text("Exposure compensation"),
            )
            .changed()
        {
            cmds.push(format!("ev {}", c.ev));
        }

        if ui.checkbox(&mut c.auto_wb, "Auto white balance").changed() {
            cmds.push(if c.auto_wb {
                "awb".into()
            } else {
                format!("wb {}", c.wb)
            });
        }
        if ui
            .add_enabled(
                !c.auto_wb,
                egui::Slider::new(&mut c.wb, 1000..=12000).text("White balance (K)"),
            )
            .changed()
        {
            cmds.push(format!("wb {}", c.wb));
        }

        for (name, value, range) in [
            ("brightness", &mut c.brightness, -10..=10),
            ("contrast", &mut c.contrast, -10..=10),
            ("saturation", &mut c.saturation, -10..=10),
            ("sharpness", &mut c.sharpness, 0..=4),
        ] {
            let label = format!("{}{}", name[..1].to_uppercase(), &name[1..]);
            if ui.add(egui::Slider::new(value, range).text(label)).changed() {
                cmds.push(format!("{name} {value}"));
            }
        }

        ui.separator();
        ui.heading("Microphone");
        if ui
            .add(egui::Slider::new(&mut c.gain, 0.0..=30.0).text("Gain (dB)"))
            .changed()
        {
            cmds.push(format!("gain {:.1}", c.gain));
        }

        ui.separator();
        ui.heading("Effects");
        if ui.checkbox(&mut c.blur, "Blur background").changed() {
            cmds.push(format!("blur {}", if c.blur { c.blur_radius } else { 0 }));
        }
        if ui
            .add_enabled(c.blur, egui::Slider::new(&mut c.blur_radius, 4..=64).text("Blur"))
            .changed()
        {
            cmds.push(format!("blur {}", c.blur_radius));
        }

        ui.horizontal(|ui| {
            ui.label("Background picture");
            ui.text_edit_singleline(&mut c.background);
        });
        if ui.checkbox(&mut c.background_on, "Replace background").changed() {
            cmds.push(if c.background_on {
                format!("background {}", c.background)
            } else {
                "background off".into()
            });
        }

        ui.horizontal(|ui| {
            ui.label("Look");
            let builtin = opal_effects::LOOKS.contains(&c.look.as_str());
            egui::ComboBox::from_id_salt("look")
                .selected_text(if builtin { c.look.as_str() } else { "from file" })
                .show_ui(ui, |ui| {
                    for name in opal_effects::LOOKS {
                        if ui.selectable_label(c.look == name, name).clicked() {
                            c.look = name.to_owned();
                            if c.look_on {
                                cmds.push(format!("lut {} {:.2}", c.look, c.look_amount));
                            }
                        }
                    }
                    if ui.selectable_label(!builtin, "from file").clicked() && builtin {
                        c.look.clear();
                    }
                });
        });
        if !opal_effects::LOOKS.contains(&c.look.as_str()) {
            ui.horizontal(|ui| {
                ui.label(".cube file");
                ui.text_edit_singleline(&mut c.look);
            });
        }
        ui.horizontal(|ui| {
            if ui.checkbox(&mut c.look_on, "Use look").changed() {
                cmds.push(if c.look_on {
                    format!("lut {} {:.2}", c.look, c.look_amount)
                } else {
                    "lut off".into()
                });
            }
            let s = ui.add_enabled(
                c.look_on,
                egui::Slider::new(&mut c.look_amount, 0.0..=1.0).text("Amount"),
            );
            if s.drag_stopped() || (s.changed() && !s.dragged()) {
                cmds.push(format!("lut {} {:.2}", c.look, c.look_amount));
            }
        });

        if ui.checkbox(&mut c.zoom, "Follow my face").changed() {
            cmds.push(if c.zoom {
                format!("zoom {:.2}", c.zoom_amount)
            } else {
                "zoom off".into()
            });
        }
        if ui
            .add_enabled(c.zoom, egui::Slider::new(&mut c.zoom_amount, 0.0..=1.0).text("Framing"))
            .changed()
        {
            cmds.push(format!("zoom {:.2}", c.zoom_amount));
        }
        ui.add_enabled_ui(c.zoom, |ui| {
            ui.horizontal(|ui| {
                ui.label("Tracking");
                for speed in ["slow", "normal", "fast"] {
                    if ui.radio_value(&mut c.tracking, speed.to_owned(), speed).changed() {
                        cmds.push(format!("tracking {speed}"));
                    }
                }
            });
        });

        if ui.checkbox(&mut c.gestures, "Hand gestures").changed() {
            cmds.push(format!("gestures {}", if c.gestures { "on" } else { "off" }));
        }
        ui.add_enabled_ui(c.gestures, |ui| {
            ui.label("Peace sign: video off and on");
            ui.label("Pinch: follow my face on and off");
        });

        for cmd in cmds {
            self.apply(cmd);
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let mut running = *self.preview_running.lock().unwrap();
        if ui.checkbox(&mut running, "Preview").changed() {
            *self.preview_running.lock().unwrap() = running;
            if running {
                self.preview.lock().unwrap().error = None;
                start_preview(
                    self.preview.clone(),
                    self.preview_running.clone(),
                    ui.ctx().clone(),
                    video_size(&self.c.resolution),
                );
            }
        }
        let mut p = self.preview.lock().unwrap();
        if let Some(e) = p.error.take() {
            self.message = e;
        }
        if let Some(rgb) = p.rgb.take() {
            let image = egui::ColorImage::from_rgb(p.size, &rgb);
            match &mut self.texture {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => self.texture = Some(ui.ctx().load_texture("preview", image, egui::TextureOptions::LINEAR)),
            }
        }
        drop(p);
        if let (Some(t), true) = (&self.texture, running) {
            let w = ui.available_width();
            ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(w, w * 9.0 / 16.0)));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.last_status.elapsed() > Duration::from_secs(1) {
            self.last_status = Instant::now();
            self.status = send("status").unwrap_or_else(|e| e);
            // A pinch can turn face following on or off from the camera.
            let mut words = self.status.split_whitespace();
            if words.any(|w| w == "zoom")
                && let Some(z) = words.next()
            {
                self.c.zoom = z != "off";
            }
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));

        egui::Panel::left("controls")
            .default_size(380.0)
            .min_size(320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui));
            });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label(&self.status);
            if !self.message.is_empty() {
                ui.colored_label(egui::Color32::LIGHT_RED, &self.message);
            }
            if self.status.ends_with("video off") {
                ui.horizontal(|ui| {
                    ui.label("Video is off. Make a peace sign to turn it back on, or");
                    if ui.button("Turn video back on").clicked() {
                        self.apply("video on".into());
                    }
                });
            }
            ui.separator();
            self.preview(ui);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Opal C1")
            .with_inner_size([1000.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native("opal-rs-gui", options, Box::new(|_| Ok(Box::new(App::new()))))
}
