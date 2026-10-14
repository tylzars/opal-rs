//! Live preview: a thread that fetches frames and shrinks them to a small RGB image.

use crate::LockExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

/// Shortest time between preview frames.
const FETCH_PERIOD: Duration = Duration::from_millis(50);

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

/// Where preview frames come from.
pub enum Source {
    /// Small frames asked of the service. Apps keep the virtual camera to themselves: v4l2loopback lets only one
    /// program read it at a time.
    Service,
    /// A video device of `width` x `height` NV12 frames (the camera's own video in low power mode on Linux, which does
    /// not pass through the service).
    Device(Option<PathBuf>, (usize, usize)),
}

/// Owner side of the preview thread.
#[derive(Default)]
pub struct Handle {
    shared: Arc<Mutex<Shared>>,
    started: Option<Instant>,
}

impl Handle {
    pub fn running(&self) -> bool {
        self.shared.locked().running
    }

    /// Time since the last start, if any.
    pub fn since_start(&self) -> Option<Duration> {
        self.started.map(|t| t.elapsed())
    }

    /// True while frames keep arriving.
    pub fn live(&self) -> bool {
        let s = self.shared.locked();
        s.running && s.last_frame.is_some_and(|t| t.elapsed() < Duration::from_secs(2))
    }

    pub fn take_frame(&self) -> Option<(Vec<u8>, [usize; 2])> {
        self.shared.locked().frame.take()
    }

    pub fn take_error(&self) -> Option<String> {
        self.shared.locked().error.take()
    }

    /// Last error without clearing it.
    pub fn error(&self) -> Option<String> {
        self.shared.locked().error.clone()
    }

    pub fn stop(&self) {
        let mut s = self.shared.locked();
        s.generation += 1;
        s.running = false;
        s.last_frame = None;
    }

    pub fn start(&mut self, ctx: egui::Context, source: Source) {
        let generation = {
            let mut s = self.shared.locked();
            s.generation += 1;
            s.running = true;
            s.error = None;
            s.last_frame = None;
            s.generation
        };
        self.started = Some(Instant::now());
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let current = || shared.locked().generation == generation;
            // Hands a frame to the window; false once this thread is no longer wanted.
            let store = |rgb: Vec<u8>, size: [usize; 2]| {
                let mut s = shared.locked();
                if s.generation != generation {
                    return false;
                }
                s.frame = Some((rgb, size));
                s.last_frame = Some(Instant::now());
                drop(s);
                ctx.request_repaint();
                true
            };
            let (name, result) = match source {
                Source::Service => ("the camera service".to_owned(), from_service(&current, &store)),
                Source::Device(Some(device), (width, height)) => (
                    device.display().to_string(),
                    from_device(&device, width, height, &current, &store),
                ),
                Source::Device(None, _) => (
                    "the camera".to_owned(),
                    Err(std::io::Error::other("not found; is it plugged in?")),
                ),
            };
            let mut s = shared.locked();
            if s.generation == generation {
                if let Err(e) = result {
                    s.error = Some(format!("{name}: {e}"));
                }
                s.running = false;
                drop(s);
                ctx.request_repaint();
            }
        });
    }
}

/// Asks the service for frames until `current` turns false or `store` refuses one.
fn from_service(current: &dyn Fn() -> bool, store: &dyn Fn(Vec<u8>, [usize; 2]) -> bool) -> std::io::Result<()> {
    while current() {
        let asked = Instant::now();
        match opal_ipc::request_data("preview", Duration::from_secs(5)) {
            Ok((words, data)) => {
                let mut n = words.split_whitespace().filter_map(|w| w.parse::<usize>().ok());
                if let (Some(w), Some(h)) = (n.next(), n.next())
                    && (2..=4096).contains(&w)
                    && (2..=4096).contains(&h)
                    && w % 2 == 0
                    && h % 2 == 0
                    && data.len() >= w * h * 3 / 2
                    && !store(nv12_to_rgb_small(&data, w, h, 1), [w, h])
                {
                    break;
                }
            }
            // The service answers, just without a frame yet.
            Err(e) if e.kind() == std::io::ErrorKind::Other => {}
            Err(e) => return Err(e),
        }
        std::thread::sleep(FETCH_PERIOD.saturating_sub(asked.elapsed()));
    }
    Ok(())
}

/// Reads a video device until `current` turns false or `store` refuses a frame.
#[cfg(unix)]
fn from_device(
    device: &std::path::Path,
    width: usize,
    height: usize,
    current: &dyn Fn() -> bool,
    store: &dyn Fn(Vec<u8>, [usize; 2]) -> bool,
) -> std::io::Result<()> {
    // Preview width in pixels; frames are averaged down to about this (the service does the same).
    const PREVIEW_WIDTH: usize = 640;
    let step = (width / PREVIEW_WIDTH).max(1);
    let size = [width / step, height / step];
    let mut cap = opal_v4l2::Capture::open(device, width as u32, height as u32, opal_v4l2::NV12)?;
    let mut last = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    while current() {
        let rgb = cap.next(Duration::from_secs(5), |f| {
            (last.elapsed() > FETCH_PERIOD && f.data.len() >= width * height * 3 / 2)
                .then(|| nv12_to_rgb_small(f.data, width, height, step))
        })?;
        if let Some(rgb) = rgb {
            last = Instant::now();
            if !store(rgb, size) {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn from_device(
    _: &std::path::Path,
    _: usize,
    _: usize,
    _: &dyn Fn() -> bool,
    _: &dyn Fn(Vec<u8>, [usize; 2]) -> bool,
) -> std::io::Result<()> {
    Err(std::io::Error::other("not available on Windows"))
}

/// A `width` x `height` NV12 frame shrunk `step` times as RGB, each pixel the average of its block (sampling single
/// pixels would make fine detail look like grain).
fn nv12_to_rgb_small(frame: &[u8], width: usize, height: usize, step: usize) -> Vec<u8> {
    let (w, h) = (width / step, height / step);
    let chroma = &frame[width * height..];
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let (y0, y1) = (y * step, (y + 1) * step);
        // Chroma rows and pair columns covering the block.
        let (cy0, cy1) = (y0 / 2, (y1 - 1) / 2);
        for x in 0..w {
            let (x0, x1) = (x * step, (x + 1) * step);
            let luma: u32 = (y0..y1)
                .flat_map(|r| &frame[r * width + x0..r * width + x1])
                .map(|&p| p as u32)
                .sum();
            let luma = luma as f32 / (step * step) as f32;
            let (cx0, cx1) = (x0 / 2, (x1 - 1) / 2);
            let (mut su, mut sv) = (0u32, 0u32);
            for r in cy0..=cy1 {
                for c in cx0..=cx1 {
                    su += chroma[r * width + 2 * c] as u32;
                    sv += chroma[r * width + 2 * c + 1] as u32;
                }
            }
            let n = ((cy1 - cy0 + 1) * (cx1 - cx0 + 1)) as f32;
            let (u, v) = (su as f32 / n - 128.0, sv as f32 / n - 128.0);
            let px = [luma + 1.402 * v, luma - 0.344_136 * u - 0.714_136 * v, luma + 1.772 * u];
            out.extend(px.map(|p| p.clamp(0.0, 255.0) as u8));
        }
    }
    out
}

/// The camera's own video device, used in low power mode: the booted C1 (03e7:f63b).
pub fn camera_node() -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .find(|e| {
            let read = |f: &str| std::fs::read_to_string(e.path().join(f)).map(|s| s.trim().to_owned());
            read("index").is_ok_and(|i| i == "0")
                && read("device/../idVendor").is_ok_and(|v| v == "03e7")
                && read("device/../idProduct").is_ok_and(|p| p == "f63b")
        })
        .map(|e| PathBuf::from("/dev").join(e.file_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_preview_averages_blocks() {
        let (w, h) = (12, 6);
        // Luma alternates 0 and 200 per pixel; chroma is neutral.
        let mut f: Vec<u8> = (0..w * h)
            .map(|i| if (i % w + i / w) % 2 == 0 { 0 } else { 200 })
            .collect();
        f.extend(std::iter::repeat_n(128, w * h / 2));
        for step in [2, 3, 6] {
            let rgb = nv12_to_rgb_small(&f, w, h, step);
            assert_eq!(rgb.len(), (w / step) * (h / step) * 3);
            // Every block is near the average of 100 (odd blocks hold 4 or 5 bright pixels of 9).
            assert!(
                rgb.iter().all(|&p| p.abs_diff(100) <= 12),
                "step {step}: {:?}",
                &rgb[..3]
            );
        }
    }
}
