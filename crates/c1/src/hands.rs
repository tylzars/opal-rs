//! Hand landmarks from the camera and the gestures made with them.

use std::time::{Duration, Instant};

use opal_dai::pipeline::ImageManipConfig;

/// NN config messages that pause or resume palm detection.
pub const PALM_CONTROL_STREAM: &str = "ctrlPalm";
/// Palm detector output: `classificators` [896] and `regressors` [896, 18].
pub const PALM_STREAM: &str = "palm";
/// Crop configs for the landmark network, one per request.
pub const CROP_STREAM: &str = "handCrop";
/// Landmark network output for each crop.
pub const LANDMARK_STREAM: &str = "landmarks";

pub const PALM_BLOB: &str = "76fe11746e.tlv";
pub const LANDMARK_BLOB: &str = "3ea9231461.tlv";
pub const PALM_INPUT: i32 = 128;
pub const LANDMARK_INPUT: i32 = 224;

/// Detections and landmarks below these scores are ignored.
const PALM_MIN: f32 = 0.6;
const HAND_MIN: f32 = 0.5;
/// Hand box size relative to the palm box, and how far it moves towards the fingers (palm box sides).
const PALM_SCALE: f32 = 2.9;
const PALM_SHIFT: f32 = 0.5;
/// Hand box size relative to the landmarks' extent, and how far it moves towards the fingers (extent heights).
const LANDMARK_SCALE: f32 = 2.0;
const LANDMARK_SHIFT: f32 = 0.1;

/// Anchor centres of the palm detector, in the order of its outputs.
fn anchors() -> Vec<(f32, f32)> {
    // Feature maps of 16x16 with 2 anchors per cell and 8x8 with 6.
    let mut a = Vec::with_capacity(896);
    for (size, per_cell) in [(16usize, 2usize), (8, 6)] {
        for y in 0..size {
            for x in 0..size {
                let c = ((x as f32 + 0.5) / size as f32, (y as f32 + 0.5) / size as f32);
                a.extend(std::iter::repeat_n(c, per_cell));
            }
        }
    }
    a
}

/// Square region around a hand, in pixels of the frames the hand networks see.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandBox {
    pub cx: f32,
    pub cy: f32,
    pub size: f32,
    /// Radians, clockwise; 0 when the fingers point up.
    pub rotation: f32,
}

fn wrap(a: f32) -> f32 {
    use std::f32::consts::PI;
    a - 2.0 * PI * ((a + PI) / (2.0 * PI)).floor()
}

/// Rotation that turns the direction `from` -> `to` to point up.
fn upright(from: (f32, f32), to: (f32, f32)) -> f32 {
    wrap(std::f32::consts::FRAC_PI_2 - (-(to.1 - from.1)).atan2(to.0 - from.0))
}

/// The frames the hand networks work from. The palm detector sees the whole frame centred in a black square.
#[derive(Clone, Copy, Debug)]
pub struct View {
    width: f32,
    height: f32,
}

impl View {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width as f32,
            height: height as f32,
        }
    }

    /// The most likely palm, as a box around the whole hand.
    pub fn palm(&self, scores: &[f32], regressors: &[f32]) -> Option<HandBox> {
        let anchors = anchors();
        if scores.len() < anchors.len() || regressors.len() < anchors.len() * 18 {
            return None;
        }
        let (best, &logit) = scores[..anchors.len()]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))?;
        if 1.0 / (1.0 + (-logit.clamp(-100.0, 100.0)).exp()) < PALM_MIN {
            return None;
        }
        let r = &regressors[best * 18..best * 18 + 18];
        let (ax, ay) = anchors[best];
        let n = PALM_INPUT as f32;
        let side = self.width.max(self.height);
        let (left, top) = ((side - self.width) / 2.0, (side - self.height) / 2.0);
        let point = |i: usize| ((r[i] / n + ax) * side - left, (r[i + 1] / n + ay) * side - top);
        let (cx, cy) = point(0);
        let (w, h) = (r[2] / n * side, r[3] / n * side);
        let rotation = upright(point(4), point(8));
        let palm = w.max(h);
        Some(HandBox {
            cx: cx + PALM_SHIFT * palm * rotation.sin(),
            cy: cy - PALM_SHIFT * palm * rotation.cos(),
            size: PALM_SCALE * palm,
            rotation,
        })
    }

    /// Crop of `b` for the landmark network.
    pub fn crop(&self, b: &HandBox) -> ImageManipConfig {
        ImageManipConfig::rotated_crop(
            (b.cx / self.width, b.cy / self.height),
            (b.size / self.width, b.size / self.height),
            b.rotation.to_degrees(),
            LANDMARK_INPUT,
            LANDMARK_INPUT,
        )
    }

    /// The hand in landmark output for the crop of `b`: score, handedness and 63 values (x, y, z in crop pixels).
    pub fn hand(&self, b: &HandBox, score: f32, handedness: f32, raw: &[f32]) -> Option<Hand> {
        if score < HAND_MIN || raw.len() < 63 {
            return None;
        }
        let (c, s) = (b.rotation.cos(), b.rotation.sin());
        let n = LANDMARK_INPUT as f32;
        let mut pixels = [(0.0, 0.0); 21];
        for (i, p) in pixels.iter_mut().enumerate() {
            let u = (raw[3 * i] / n - 0.5) * b.size;
            let v = (raw[3 * i + 1] / n - 0.5) * b.size;
            *p = (b.cx + u * c - v * s, b.cy + u * s + v * c);
        }
        Some(Hand {
            score,
            handedness,
            points: pixels.map(|(x, y)| (x / self.width, y / self.height)),
            pixels,
        })
    }
}

/// Wrist, thumb base and the first two joints of each finger: the landmarks the next crop is sized from.
const PALM_POINTS: [usize; 12] = [0, 1, 2, 3, 5, 6, 9, 10, 13, 14, 17, 18];

/// Box for the next landmark crop, following the hand from its landmarks.
pub fn follow(hand: &Hand) -> HandBox {
    let pts: Vec<(f32, f32)> = PALM_POINTS.iter().map(|&i| hand.pixels[i]).collect();
    let pts = &pts;
    let p = &hand.pixels;
    let knuckles = (
        0.25 * (p[INDEX_BASE].0 + p[RING_BASE].0) + 0.5 * p[MIDDLE_BASE].0,
        0.25 * (p[INDEX_BASE].1 + p[RING_BASE].1) + 0.5 * p[MIDDLE_BASE].1,
    );
    let rotation = upright(p[WRIST], knuckles);
    let (c, s) = (rotation.cos(), rotation.sin());
    let n = pts.len() as f32;
    let (mx, my) = (
        pts.iter().map(|p| p.0).sum::<f32>() / n,
        pts.iter().map(|p| p.1).sum::<f32>() / n,
    );
    let range = |it: &mut dyn Iterator<Item = f32>| it.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let (u0, u1) = range(&mut pts.iter().map(|p| (p.0 - mx) * c + (p.1 - my) * s));
    let (v0, v1) = range(&mut pts.iter().map(|p| -(p.0 - mx) * s + (p.1 - my) * c));
    let size = LANDMARK_SCALE * (u1 - u0).max(v1 - v0);
    let (uc, vc) = ((u0 + u1) / 2.0, (v0 + v1) / 2.0 - LANDMARK_SHIFT * (v1 - v0));
    HandBox {
        cx: mx + uc * c - vc * s,
        cy: my + uc * s + vc * c,
        size,
        rotation,
    }
}

/// A hand: landmark positions normalised to the frame (0 to 1), wrist first, in the usual 21-point order.
#[derive(Clone, Debug, PartialEq)]
pub struct Hand {
    pub score: f32,
    pub handedness: f32,
    pub points: [(f32, f32); 21],
    /// The same points in pixels.
    pixels: [(f32, f32); 21],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// Index and middle finger up, the others folded.
    Peace,
    /// Thumb and index tips together.
    Pinch,
}

/// Wrist, then finger tip and middle joint of the index, middle, ring and little fingers.
const WRIST: usize = 0;
const THUMB_TIP: usize = 4;
const FINGERS: [(usize, usize); 4] = [(8, 6), (12, 10), (16, 14), (20, 18)];
const INDEX_BASE: usize = 5;
const MIDDLE_BASE: usize = 9;
const RING_BASE: usize = 13;

impl Hand {
    /// Distance between two landmarks, with x scaled so both axes use the same unit.
    fn dist(&self, a: usize, b: usize, aspect: f32) -> f32 {
        let (pa, pb) = (self.points[a], self.points[b]);
        (((pa.0 - pb.0) * aspect).powi(2) + (pa.1 - pb.1).powi(2)).sqrt()
    }

    fn extended(&self, (tip, joint): (usize, usize), aspect: f32) -> bool {
        self.dist(WRIST, tip, aspect) > 1.15 * self.dist(WRIST, joint, aspect)
    }

    /// Distance between thumb and index tips, in lengths of the palm.
    fn pinch_gap(&self, aspect: f32) -> f32 {
        self.dist(THUMB_TIP, FINGERS[0].0, aspect) / self.dist(WRIST, MIDDLE_BASE, aspect)
    }

    /// Which of the index, middle, ring and little fingers are straight.
    fn fingers_up(&self, aspect: f32) -> [bool; 4] {
        FINGERS.map(|f| self.extended(f, aspect))
    }

    /// The gesture being made, for a frame `aspect` (width over height) wide.
    pub fn gesture(&self, aspect: f32) -> Option<Gesture> {
        let up = self.fingers_up(aspect);
        if self.pinch_gap(aspect) < 0.3 {
            return Some(Gesture::Pinch);
        }
        (up == [true, true, false, false]).then_some(Gesture::Peace)
    }
}

/// Holding a gesture this long counts as making it.
const HOLD: Duration = Duration::from_millis(400);
/// A gesture has ended once it has not been seen for this long; shorter gaps are tracking dropouts.
const RELEASE: Duration = Duration::from_secs(1);
/// After a gesture, others are ignored for this long.
const COOLDOWN: Duration = Duration::from_millis(1500);

/// Turns per-frame gestures into single events.
pub struct Gestures {
    /// The gesture being held: when it started and when it was last seen.
    current: Option<(Gesture, Instant, Instant)>,
    fired: bool,
    quiet_until: Instant,
}

impl Default for Gestures {
    fn default() -> Self {
        Self {
            current: None,
            fired: false,
            quiet_until: Instant::now(),
        }
    }
}

impl Gestures {
    /// Takes this frame's gesture and returns one once it has been held, once per hold.
    pub fn update(&mut self, seen: Option<Gesture>) -> Option<Gesture> {
        self.update_at(seen, Instant::now())
    }

    fn update_at(&mut self, seen: Option<Gesture>, now: Instant) -> Option<Gesture> {
        if let Some((_, _, last)) = self.current
            && now - last >= RELEASE
        {
            self.current = None;
            self.fired = false;
        }
        let g = seen?;
        match &mut self.current {
            Some((c, _, last)) if *c == g => *last = now,
            _ => {
                self.current = Some((g, now, now));
                self.fired = false;
            }
        }
        let (_, since, _) = self.current?;
        if self.fired || now < self.quiet_until || now - since < HOLD {
            return None;
        }
        self.fired = true;
        self.quiet_until = now + COOLDOWN;
        Some(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand pointing up with the given fingers (index, middle, ring, little) raised.
    fn hand(up: [bool; 4], pinch: bool) -> Hand {
        let mut points = [(0.5, 0.9); 21];
        points[MIDDLE_BASE] = (0.5, 0.7);
        for (i, &(tip, joint)) in FINGERS.iter().enumerate() {
            let x = 0.44 + 0.04 * i as f32;
            points[joint] = (x, 0.6);
            points[tip] = (x, if up[i] { 0.45 } else { 0.66 });
        }
        points[THUMB_TIP] = if pinch { points[FINGERS[0].0] } else { (0.3, 0.7) };
        Hand {
            score: 0.9,
            handedness: 0.9,
            points,
            pixels: points,
        }
    }

    #[test]
    fn has_896_anchors() {
        let a = anchors();
        assert_eq!(a.len(), 896);
        assert_eq!(a[0], (1.0 / 32.0, 1.0 / 32.0));
        assert_eq!(a[512], (1.0 / 16.0, 1.0 / 16.0));
        assert_eq!(a[895], (15.0 / 16.0, 15.0 / 16.0));
    }

    #[test]
    fn decodes_an_upright_palm() {
        let view = View::new(640, 360);
        let mut scores = vec![-10.0; 896];
        let mut reg = vec![0.0; 896 * 18];
        // Anchor 300: cell (6, 9) of the 16x16 map, centre (0.40625, 0.59375).
        scores[300] = 5.0;
        let r = &mut reg[300 * 18..301 * 18];
        r[2] = 25.6;
        r[3] = 25.6;
        r[5] = 12.8;
        r[9] = -12.8;
        let b = view.palm(&scores, &reg).unwrap();
        assert!(b.rotation.abs() < 1e-5, "{b:?}");
        // Box 0.2 of the 640 pixel square each way, 140 pixels of it above the frame; the hand box is 2.9 times
        // its side, moved half of it up.
        assert!((b.size - 2.9 * 128.0).abs() < 1e-3, "{b:?}");
        assert!(
            (b.cx - 0.40625 * 640.0).abs() < 1e-3 && (b.cy - (0.59375 * 640.0 - 140.0 - 64.0)).abs() < 1e-3,
            "{b:?}"
        );
        scores[300] = -1.0;
        assert_eq!(view.palm(&scores, &reg), None);
    }

    #[test]
    fn landmarks_round_trip_through_the_box() {
        let view = View::new(640, 360);
        let b = HandBox {
            cx: 320.0,
            cy: 180.0,
            size: 100.0,
            rotation: 0.3,
        };
        let mut raw = vec![112.0; 63];
        raw[1] = 200.0;
        raw[28] = 40.0;
        let h = view.hand(&b, 0.9, 0.5, &raw).unwrap();
        assert!((h.pixels[5].0 - 320.0).abs() < 1e-3 && (h.pixels[5].1 - 180.0).abs() < 1e-3);
        assert!((h.points[5].0 - 0.5).abs() < 1e-5);
        assert!((follow(&h).rotation - 0.3).abs() < 1e-4);
        assert!(view.hand(&b, 0.1, 0.5, &raw).is_none());
        let crop = view.crop(&b);
        assert_eq!(
            crop,
            ImageManipConfig::rotated_crop(
                (0.5, 0.5),
                (100.0 / 640.0, 100.0 / 360.0),
                0.3f32.to_degrees(),
                224,
                224
            )
        );
    }

    #[test]
    fn recognises_gestures() {
        let a = 1.0;
        assert_eq!(hand([true, true, false, false], false).gesture(a), Some(Gesture::Peace));
        assert_eq!(hand([true, true, true, true], false).gesture(a), None);
        assert_eq!(hand([false, false, false, false], false).gesture(a), None);
        assert_eq!(hand([true, true, true, true], true).gesture(a), Some(Gesture::Pinch));
    }

    #[test]
    fn fires_once_per_hold() {
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        let peace = Some(Gesture::Peace);
        let mut g = Gestures::default();
        assert_eq!(g.update_at(peace, ms(0)), None);
        assert_eq!(g.update_at(peace, ms(450)), peace);
        assert_eq!(g.update_at(peace, ms(500)), None);
        // Dropouts shorter than a release do not end the hold, even past the cooldown.
        assert_eq!(g.update_at(None, ms(1200)), None);
        assert_eq!(g.update_at(peace, ms(1400)), None);
        assert_eq!(g.update_at(peace, ms(2300)), None);
        assert_eq!(g.update_at(peace, ms(3000)), None);
        // Gone for a release: the next hold fires again.
        assert_eq!(g.update_at(None, ms(4100)), None);
        assert_eq!(g.update_at(peace, ms(4200)), None);
        assert_eq!(g.update_at(peace, ms(4700)), peace);
    }
}
