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
