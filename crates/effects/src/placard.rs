//! The picture shown while the video is off: a crossed-out camera on a dark background, and a fade to it.

use std::thread;

const BACKGROUND: u8 = 28;
const INK: u8 = 150;

/// Distance from `p` to the segment `a`-`b`.
fn segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (ab, ap) = ([b[0] - a[0], b[1] - a[1]], [p[0] - a[0], p[1] - a[1]]);
    let t = ((ap[0] * ab[0] + ap[1] * ab[1]) / (ab[0] * ab[0] + ab[1] * ab[1])).clamp(0.0, 1.0);
    ((ap[0] - ab[0] * t).powi(2) + (ap[1] - ab[1] * t).powi(2)).sqrt()
}

/// Distance from `p` to the outline of a rectangle with rounded corners.
fn rounded_rect(p: [f32; 2], centre: [f32; 2], half: [f32; 2], radius: f32) -> f32 {
    let q = [
        (p[0] - centre[0]).abs() - half[0] + radius,
        (p[1] - centre[1]).abs() - half[1] + radius,
    ];
    let outside = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt();
    (outside + q[0].max(q[1]).min(0.0) - radius).abs()
}

/// An NV12 `width` x `height` frame with the camera-off sign.
pub fn placard(width: usize, height: usize) -> Vec<u8> {
    let (w, h) = (width as f32, height as f32);
    let s = h / 1080.0 * 1.6;
    let c = [w / 2.0, h / 2.0];
    let stroke = 9.0 * s;
    // Camera body, the lens bump on its right, and a slash across both.
    let body = ([c[0] - 30.0 * s, c[1]], [70.0 * s, 48.0 * s]);
    let bump = [
        [c[0] + 48.0 * s, c[1] - 14.0 * s],
        [c[0] + 92.0 * s, c[1] - 38.0 * s],
        [c[0] + 92.0 * s, c[1] + 38.0 * s],
        [c[0] + 48.0 * s, c[1] + 14.0 * s],
    ];
    let slash = ([c[0] - 110.0 * s, c[1] - 90.0 * s], [c[0] + 110.0 * s, c[1] + 90.0 * s]);
    let mut frame = vec![BACKGROUND; width * height];
    frame.resize(width * height * 3 / 2, 128);
    let (x0, x1) = ((c[0] - 130.0 * s) as usize, ((c[0] + 130.0 * s) as usize).min(width));
    let (y0, y1) = ((c[1] - 110.0 * s) as usize, ((c[1] + 110.0 * s) as usize).min(height));
    for y in y0..y1 {
        for x in x0..x1 {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let mut d = rounded_rect(p, body.0, body.1, 14.0 * s);
            for i in 0..3 {
                d = d.min(segment(p, bump[i], bump[i + 1]));
            }
            d = d.min(segment(p, slash.0, slash.1));
            // Antialiased edge one pixel wide.
            let cover = (stroke / 2.0 - d + 0.5).clamp(0.0, 1.0);
            frame[y * width + x] = (BACKGROUND as f32 + (INK - BACKGROUND) as f32 * cover).round() as u8;
        }
    }
    frame
}

/// Moves `frame` `amount` of the way to `target` (0 leaves it, 1 replaces it). Both are NV12 of the same size.
pub fn fade(frame: &mut [u8], target: &[u8], amount: f32) {
    let a = (amount.clamp(0.0, 1.0) * 256.0) as i32;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = frame.len().div_ceil(threads).max(1);
    thread::scope(|s| {
        for (f, t) in frame.chunks_mut(chunk).zip(target.chunks(chunk)) {
            s.spawn(move || {
                for (p, q) in f.iter_mut().zip(t) {
                    *p = (*p as i32 + (((*q as i32 - *p as i32) * a) >> 8)) as u8;
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placard_has_a_sign_on_dark_ground() {
        let (w, h) = (1280, 720);
        let f = placard(w, h);
        assert_eq!(f.len(), w * h * 3 / 2);
        assert_eq!(f[0], BACKGROUND);
        assert!(f[w * h..].iter().all(|&c| c == 128));
        // The slash passes through the centre.
        assert!(f[(h / 2) * w + w / 2] > 100);
    }

    #[test]
    fn fade_ends() {
        let mut f = vec![200u8; 64];
        let t = vec![20u8; 64];
        fade(&mut f, &t, 0.0);
        assert_eq!(f[0], 200);
        fade(&mut f, &t, 1.0);
        assert_eq!(f[0], 20);
        let mut g = vec![200u8; 64];
        fade(&mut g, &t, 0.5);
        assert_eq!(g[0], 110);
    }
}
