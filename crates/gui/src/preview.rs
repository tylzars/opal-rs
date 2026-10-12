//! Live preview: a capture thread that samples frames down to a small RGB image.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

/// Preview width in pixels; frames are sampled down to about this.
const PREVIEW_WIDTH: usize = 640;

#[derive(Default)]
struct Shared {
    /// Bumped on every start and stop; a thread runs while it matches its own.
    generation: u64,
    running: bool,
    frame: Option<(Vec<u8>, [usize; 2])>,
    /// When the newest frame arrived.
    last_frame: Option<Instant>,
    error: Option<String>,
}

/// Owner side of the capture thread.
#[derive(Default)]
pub struct Handle {
    shared: Arc<Mutex<Shared>>,
    started: Option<Instant>,
}

impl Handle {
    pub fn running(&self) -> bool {
        self.shared.lock().unwrap().running
    }

    /// Time since the last start, if any.
    pub fn since_start(&self) -> Option<Duration> {
        self.started.map(|t| t.elapsed())
    }

    /// True while frames keep arriving.
    pub fn live(&self) -> bool {
        let s = self.shared.lock().unwrap();
        s.running && s.last_frame.is_some_and(|t| t.elapsed() < Duration::from_secs(2))
    }

    pub fn take_frame(&self) -> Option<(Vec<u8>, [usize; 2])> {
        self.shared.lock().unwrap().frame.take()
    }

    pub fn take_error(&self) -> Option<String> {
        self.shared.lock().unwrap().error.take()
    }

    /// Last error without clearing it.
    pub fn error(&self) -> Option<String> {
        self.shared.lock().unwrap().error.clone()
    }

    pub fn stop(&self) {
        let mut s = self.shared.lock().unwrap();
        s.generation += 1;
        s.running = false;
        s.last_frame = None;
    }

    pub fn start(&mut self, ctx: egui::Context, (device, (width, height)): (PathBuf, (usize, usize))) {
        let generation = {
            let mut s = self.shared.lock().unwrap();
            s.generation += 1;
            s.running = true;
            s.error = None;
            s.last_frame = None;
            s.generation
        };
        self.started = Some(Instant::now());
        let shared = self.shared.clone();
        let step = (width / PREVIEW_WIDTH).max(1);
        let size = [width / step, height / step];
        std::thread::spawn(move || {
            let current = || shared.lock().unwrap().generation == generation;
            let result = (|| -> std::io::Result<()> {
                let mut cap = opal_v4l2::Capture::open(&device, width as u32, height as u32, opal_v4l2::NV12)?;
                let mut last = Instant::now() - Duration::from_secs(1);
                while current() {
                    let rgb = cap.next(Duration::from_secs(5), |f| {
                        (last.elapsed() > Duration::from_millis(40) && f.data.len() >= width * height * 3 / 2)
                            .then(|| nv12_to_rgb_small(f.data, width, height, step))
                    })?;
                    if let Some(rgb) = rgb {
                        last = Instant::now();
                        let mut s = shared.lock().unwrap();
                        if s.generation != generation {
                            break;
                        }
                        s.frame = Some((rgb, size));
                        s.last_frame = Some(last);
                        drop(s);
                        ctx.request_repaint();
                    }
                }
                Ok(())
            })();
            let mut s = shared.lock().unwrap();
            if s.generation == generation {
                if let Err(e) = result {
                    s.error = Some(format!("{}: {e}", device.display()));
                }
                s.running = false;
                drop(s);
                ctx.request_repaint();
            }
        });
    }
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

/// The camera's own video device, used in low power mode.
pub fn camera_node() -> PathBuf {
    std::fs::read_dir("/sys/class/video4linux")
        .into_iter()
        .flatten()
        .flatten()
        .find(|e| {
            let read = |f: &str| std::fs::read_to_string(e.path().join(f)).map(|s| s.trim().to_owned());
            read("index").is_ok_and(|i| i == "0") && read("device/../idVendor").is_ok_and(|v| v == "03e7")
        })
        .map_or_else(
            || PathBuf::from("/dev/video0"),
            |e| PathBuf::from("/dev").join(e.file_name()),
        )
}
