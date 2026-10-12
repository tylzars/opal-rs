//! Skin touch-up: smooths fine detail on skin and evens out or warms its colour. Skin is found by its colour.

use std::thread;

/// How skin-like a chroma pair is, 0 to 1 (full-range BT.601, skin sits around U 110, V 150).
fn skin(u: u8, v: u8) -> f32 {
    let (du, dv) = ((u as f32 - 110.0) / 16.0, (v as f32 - 152.0) / 14.0);
    (1.0 - (du * du + dv * dv).sqrt() / 1.5).clamp(0.0, 1.0)
}

/// Settings, each 0 to 1 (warmth -1 to 1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Skin {
    /// Smoothing of fine detail.
    pub smooth: f32,
    /// How far skin colour is pulled to its average.
    pub even: f32,
    /// Warmer (above 0) or cooler skin.
    pub warmth: f32,
}

impl Skin {
    pub fn is_off(&self) -> bool {
        *self == Self::default()
    }
}

/// Reusable buffers for one frame size.
pub struct TouchUp {
    width: usize,
    height: usize,
    /// Row sums for the box blur, then the blurred luma.
    temp: Vec<u32>,
    blurred: Vec<u8>,
    weights: Vec<u8>,
}

impl TouchUp {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            temp: vec![0; width * height],
            blurred: vec![0; width * height],
            weights: vec![0; width / 2 * (height / 2)],
        }
    }

    pub fn apply(&mut self, frame: &mut [u8], s: Skin) {
        let (w, h) = (self.width, self.height);
        if s.is_off() || frame.len() < w * h * 3 / 2 {
            return;
        }
        let (luma, chroma) = frame[..w * h * 3 / 2].split_at_mut(w * h);
        let threads = thread::available_parallelism().map_or(1, |n| n.get());
        // Chroma rows (w / 2 UV pairs each) per thread.
        let crows = (h / 2).div_ceil(threads);
        // Skin weight per 2x2 block, from the chroma, with the weighted sums of U and V.
        let total = thread::scope(|sc| {
            let parts: Vec<_> = self
                .weights
                .chunks_mut(crows * (w / 2))
                .zip(chroma.chunks(crows * w))
                .map(|(weights, uv)| {
                    sc.spawn(move || {
                        let mut t = [0f64; 3];
                        for (k, uv) in weights.iter_mut().zip(uv.as_chunks::<2>().0) {
                            let s = skin(uv[0], uv[1]);
                            *k = (s * 255.0) as u8;
                            t[0] += s as f64;
                            t[1] += s as f64 * uv[0] as f64;
                            t[2] += s as f64 * uv[1] as f64;
                        }
                        t
                    })
                })
                .collect();
            parts
                .into_iter()
                .map(|p| p.join().unwrap())
                .fold([0f64; 3], |a, t| [a[0] + t[0], a[1] + t[1], a[2] + t[2]])
        });
        if total[0] < 1.0 {
            return;
        }
        if s.smooth > 0.0 {
            let radius = (h / 180).max(2);
            box_blur(luma, &mut self.temp, &mut self.blurred, w, h, radius, threads);
            let amount = (s.smooth.min(1.0) * 256.0) as i32;
            let (weights, blurred) = (&self.weights, &self.blurred);
            // An even number of rows per thread, so each 2x2 weight block stays in one band.
            let rows = h.div_ceil(threads).div_ceil(2) * 2;
            thread::scope(|sc| {
                for (band, l) in luma.chunks_mut(rows * w).enumerate() {
                    sc.spawn(move || {
                        for (r, row) in l.chunks_exact_mut(w).enumerate() {
                            let y = band * rows + r;
                            let (k_row, b_row) = (&weights[(y / 2) * (w / 2)..][..w / 2], &blurred[y * w..][..w]);
                            for (x, p) in row.iter_mut().enumerate() {
                                let k = k_row[x / 2] as i32;
                                if k == 0 {
                                    continue;
                                }
                                let d = b_row[x] as i32 - *p as i32;
                                // Only small differences (pores, blemishes) are smoothed; edges stay.
                                let fine = (24 - d.abs()).clamp(0, 12) * 21;
                                *p = (*p as i32 + d * amount * k / 256 * fine / 256 / 256) as u8;
                            }
                        }
                    });
                }
            });
        }
        if s.even > 0.0 || s.warmth != 0.0 {
            let mean = [total[1] / total[0], total[2] / total[0]];
            let even = s.even.min(1.0);
            // Warmer skin: more red (V), a little less blue (U).
            let shift = [-4.0 * s.warmth, 8.0 * s.warmth];
            let weights = &self.weights;
            thread::scope(|sc| {
                for (uv, k) in chroma.chunks_mut(crows * w).zip(weights.chunks(crows * (w / 2))) {
                    sc.spawn(move || {
                        for (uv, &k) in uv.as_chunks_mut::<2>().0.iter_mut().zip(k) {
                            if k == 0 {
                                continue;
                            }
                            let k = k as f32 / 255.0;
                            for c in 0..2 {
                                let v = uv[c] as f32;
                                let target = v + (mean[c] as f32 - v) * even + shift[c];
                                uv[c] = (v + (target - v) * k).round().clamp(0.0, 255.0) as u8;
                            }
                        }
                    });
                }
            });
        }
    }
}

/// Box blur of `src` into `dst` with half-width `r`, edges repeated, using `temp` for the horizontal pass.
/// Both passes run in bands of rows on `threads` threads.
fn box_blur(src: &[u8], temp: &mut [u32], dst: &mut [u8], w: usize, h: usize, r: usize, threads: usize) {
    let rows = h.div_ceil(threads.max(1));
    thread::scope(|sc| {
        for (t, s) in temp.chunks_mut(rows * w).zip(src.chunks(rows * w)) {
            sc.spawn(move || {
                for (out, row) in t.chunks_exact_mut(w).zip(s.chunks_exact(w)) {
                    let mut sum: u32 =
                        row[..=r.min(w - 1)].iter().map(|&v| v as u32).sum::<u32>() + row[0] as u32 * r as u32;
                    for (x, o) in out.iter_mut().enumerate() {
                        *o = sum;
                        let add = row[(x + r + 1).min(w - 1)] as u32;
                        let sub = row[x.saturating_sub(r)] as u32;
                        sum = sum + add - sub;
                    }
                }
            });
        }
    });
    let n = ((2 * r + 1) * (2 * r + 1)) as u32;
    let temp = &*temp;
    let row = |y: isize| &temp[(y.clamp(0, h as isize - 1) as usize) * w..][..w];
    thread::scope(|sc| {
        for (band, d) in dst.chunks_mut(rows * w).enumerate() {
            sc.spawn(move || {
                let y0 = (band * rows) as isize;
                // Column sums over rows y0 - r ..= y0 + r, then slid down one row at a time.
                let mut sums = vec![0u32; w];
                for k in y0 - r as isize..=y0 + r as isize {
                    for (s, &v) in sums.iter_mut().zip(row(k)) {
                        *s += v;
                    }
                }
                for (i, out) in d.chunks_exact_mut(w).enumerate() {
                    let y = y0 + i as isize;
                    for (o, &s) in out.iter_mut().zip(&sums) {
                        *o = ((s + n / 2) / n) as u8;
                    }
                    let (add, sub) = (row(y + r as isize + 1), row(y - r as isize));
                    for ((s, &a), &b) in sums.iter_mut().zip(add).zip(sub) {
                        *s = *s + a - b;
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skin_weight() {
        assert!(skin(110, 152) > 0.99);
        assert_eq!(skin(128, 128), 0.0);
        assert_eq!(skin(180, 90), 0.0);
    }

    #[test]
    fn box_blur_keeps_flat_and_spreads_a_dot() {
        let (w, h) = (9, 9);
        let mut src = vec![100u8; w * h];
        let (mut t, mut d) = (vec![0; w * h], vec![0; w * h]);
        box_blur(&src, &mut t, &mut d, w, h, 1, 3);
        assert!(d.iter().all(|&v| v == 100));
        src[4 * w + 4] = 190;
        box_blur(&src, &mut t, &mut d, w, h, 1, 3);
        assert_eq!(d[4 * w + 4], 110);
        assert_eq!(d[3 * w + 3], 110);
        assert_eq!(d[0], 100);
    }

    #[test]
    fn box_blur_matches_a_plain_average_on_any_thread_count() {
        let (w, h, r) = (37, 23, 3);
        let src: Vec<u8> = (0..w * h).map(|i| ((i * 7919) % 251) as u8).collect();
        let at =
            |x: isize, y: isize| src[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
        let n = ((2 * r + 1) * (2 * r + 1)) as u32;
        let want: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y, r) = ((i % w) as isize, (i / w) as isize, r as isize);
                let sum: u32 = (-r..=r)
                    .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
                    .map(|(dx, dy)| at(x + dx, y + dy) as u32)
                    .sum();
                ((sum + n / 2) / n) as u8
            })
            .collect();
        for threads in [1, 2, 5, 8] {
            let (mut t, mut d) = (vec![0; w * h], vec![0; w * h]);
            box_blur(&src, &mut t, &mut d, w, h, r, threads);
            assert_eq!(d, want, "{threads} threads");
        }
    }

    #[test]
    fn only_skin_changes() {
        let (w, h) = (16, 8);
        let mut f: Vec<u8> = (0..w * h).map(|i| (100 + (i * 37) % 20) as u8).collect();
        // Left half skin coloured, right half grey.
        for _ in 0..h / 2 {
            for x in 0..w / 2 {
                let (u, v) = if x < w / 4 { (110, 152) } else { (128, 128) };
                f.push(u);
                f.push(v);
            }
        }
        let original = f.clone();
        let mut t = TouchUp::new(w, h);
        t.apply(
            &mut f,
            Skin {
                smooth: 1.0,
                even: 0.0,
                warmth: 1.0,
            },
        );
        let uv = |f: &[u8], x: usize| (f[w * h + 2 * x], f[w * h + 2 * x + 1]);
        assert_ne!(uv(&f, 0), uv(&original, 0), "skin warmed");
        assert_eq!(uv(&f, 6), uv(&original, 6), "grey untouched");
        assert_eq!(f[w - 1], original[w - 1], "grey luma untouched");
    }
}
