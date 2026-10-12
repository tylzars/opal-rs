//! Keeps a face framed by moving a crop region over the frame.

use std::time::{Duration, Instant};

use crate::zoom::Rect;

/// Largest zoom, as frame height over crop height.
const MAX_ZOOM: f32 = 2.5;
/// Crop height in face heights at amount 0 (loose) and 1 (tight).
const LOOSE: f32 = 7.0;
const TIGHT: f32 = 3.0;
/// How far below the face centre the crop centre sits, in crop heights.
const HEADROOM: f32 = 0.1;
/// Without a face for this long the crop goes back to the whole frame.
const FACE_TIMEOUT: Duration = Duration::from_secs(2);
const OBSERVATION_NOISE: f32 = 1e-3;

/// How quickly the crop follows the face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Speed {
    Slow,
    #[default]
    Normal,
    Fast,
}

impl Speed {
    fn process_noise(self) -> f32 {
        match self {
            Speed::Slow => 1e-5,
            Speed::Normal => 1e-4,
            Speed::Fast => 1e-3,
        }
    }

    /// Share of the remaining distance to the target covered each frame.
    fn rate(self) -> f32 {
        match self {
            Speed::Slow => 0.04,
            Speed::Normal => 0.08,
            Speed::Fast => 0.15,
        }
    }
}

/// One-dimensional Kalman filter for a value that stays put between measurements.
#[derive(Clone, Copy, Debug)]
struct Kalman {
    value: f32,
    error: f32,
}

impl Kalman {
    fn new(value: f32) -> Self {
        Self { value, error: 1.0 }
    }

    fn update(&mut self, measurement: f32, process_noise: f32) -> f32 {
        self.error += process_noise;
        let gain = self.error / (self.error + OBSERVATION_NOISE);
        self.value += gain * (measurement - self.value);
        self.error *= 1.0 - gain;
        self.value
    }
}

pub struct Framing {
    width: f32,
    height: f32,
    crop: Rect,
    target: Rect,
    /// Smoothed face centre x, centre y and height, normalised to the frame.
    face: Option<[Kalman; 3]>,
    last_face: Option<Instant>,
    /// 0 frames loosely, 1 tightly.
    pub amount: f32,
    pub speed: Speed,
}

impl Framing {
    pub fn new(width: usize, height: usize) -> Self {
        let full = Rect::full(width, height);
        Self {
            width: width as f32,
            height: height as f32,
            crop: full,
            target: full,
            face: None,
            last_face: None,
            amount: 0.5,
            speed: Speed::Normal,
        }
    }

    /// Takes a face, normalised to the frame: centre and height.
    pub fn face(&mut self, cx: f32, cy: f32, height: f32) {
        let q = self.speed.process_noise();
        let f = self
            .face
            .get_or_insert([Kalman::new(cx), Kalman::new(cy), Kalman::new(height)]);
        let (cx, cy, fh) = (f[0].update(cx, q), f[1].update(cy, q), f[2].update(height, q));
        self.last_face = Some(Instant::now());

        let amount = self.amount.clamp(0.0, 1.0);
        let crop_h = (fh * self.height * (LOOSE + (TIGHT - LOOSE) * amount)).clamp(self.height / MAX_ZOOM, self.height);
        let crop_w = crop_h * self.width / self.height;
        self.target = Rect {
            x: cx * self.width - crop_w / 2.0,
            y: cy * self.height + HEADROOM * crop_h - crop_h / 2.0,
            width: crop_w,
            height: crop_h,
        }
        .clamped(self.width as usize, self.height as usize);
    }

    /// Moves the crop one frame towards where it should be and returns it.
    pub fn step(&mut self) -> Rect {
        if self.last_face.is_none_or(|t| t.elapsed() > FACE_TIMEOUT) {
            self.target = Rect::full(self.width as usize, self.height as usize);
            self.face = None;
        }
        let k = self.speed.rate();
        let (c, t) = (&mut self.crop, self.target);
        c.x += (t.x - c.x) * k;
        c.y += (t.y - c.y) * k;
        c.width += (t.width - c.width) * k;
        c.height += (t.height - c.height) * k;
        *c = c.clamped(self.width as usize, self.height as usize);
        *c
    }

    /// True while the crop is close enough to the whole frame to skip cropping.
    pub fn is_full(&self) -> bool {
        self.crop.width >= self.width - 1.0 && self.crop.height >= self.height - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooms_towards_the_face_and_keeps_aspect() {
        let mut f = Framing::new(1920, 1080);
        assert!(f.is_full());
        for _ in 0..200 {
            f.face(0.3, 0.4, 0.15);
            f.step();
        }
        let r = f.step();
        assert!((r.width / r.height - 16.0 / 9.0).abs() < 0.01);
        assert!(r.height < 1080.0 && r.height >= 1080.0 / MAX_ZOOM - 1.0, "{r:?}");
        let cx = r.x + r.width / 2.0;
        assert!((cx - 0.3 * 1920.0).abs() < 2.0 || r.x == 0.0, "{r:?}");
        assert!(!f.is_full());
    }

    #[test]
    fn tighter_amount_zooms_in_more() {
        let crop = |amount: f32| {
            let mut f = Framing::new(1920, 1080);
            f.amount = amount;
            for _ in 0..300 {
                f.face(0.5, 0.5, 0.08);
                f.step();
            }
            f.step().height
        };
        assert!(crop(1.0) < crop(0.0));
    }

    #[test]
    fn kalman_smooths_jumps() {
        let mut k = Kalman::new(0.0);
        for _ in 0..50 {
            k.update(0.0, 1e-4);
        }
        let after = k.update(1.0, 1e-4);
        assert!(after > 0.0 && after < 0.5, "{after}");
    }
}
