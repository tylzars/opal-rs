//! Background blur from a person mask.

use std::thread;

/// Downscale factor of the blurred planes for a frame `height` rows high: the planes stay near 480x270 up to 4K.
fn scale_for(height: usize) -> usize {
    if height >= 2160 { 8 } else { 4 }
}
const PASSES: usize = 3;

/// Person probability per pixel, 0 to 1, at a low resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Mask {
    /// From segmentation scores laid out as two planes: background, then person.
    pub fn from_scores(scores: &[f32], width: usize, height: usize) -> Option<Self> {
        let n = width * height;
        if scores.len() < 2 * n {
            return None;
        }
        let (bg, person) = scores.split_at(n);
        let data = bg
            .iter()
            .zip(&person[..n])
            .map(|(b, p)| 1.0 / (1.0 + (b - p).exp()))
            .collect();
        Some(Self { width, height, data })
    }

    /// Moves this mask towards `next` by `amount`: 0 keeps this one, 1 takes `next`.
    pub fn follow(&mut self, next: &Mask, amount: f32) {
        if (next.width, next.height) != (self.width, self.height) {
            *self = next.clone();
            return;
        }
        for (a, b) in self.data.iter_mut().zip(&next.data) {
            *a += (b - *a) * amount;
        }
    }
}

/// Source indices and weight (0 to 256) for bilinear sampling of `src` samples at `dst` positions.
fn taps(src: usize, dst: usize) -> Vec<(u32, u32, u32)> {
    (0..dst)
        .map(|i| {
            let pos = ((i as f32 + 0.5) * src as f32 / dst as f32 - 0.5).clamp(0.0, (src - 1) as f32);
            let i0 = pos.floor() as usize;
            let i1 = (i0 + 1).min(src - 1);
            let w = ((pos - i0 as f32) * 256.0).round() as u32;
            (i0 as u32, i1 as u32, w)
        })
        .collect()
}

#[inline]
fn bilinear(p00: u8, p01: u8, p10: u8, p11: u8, wx: u32, wy: u32) -> u32 {
    let top = p00 as u32 * (256 - wx) + p01 as u32 * wx;
    let bottom = p10 as u32 * (256 - wx) + p11 as u32 * wx;
    (top * (256 - wy) + bottom * wy + (1 << 15)) >> 16
}

#[inline]
fn mix(sharp: u8, blurred: u32, alpha: u32) -> u8 {
    let (s, b) = (sharp as i32, blurred as i32);
    (b + ((s - b) * alpha as i32 + 127) / 255) as u8
}

fn box_blur_h(src: &[u8], dst: &mut [u8], w: usize, r: usize) {
    let div = (2 * r + 1) as u32;
    for (row, out) in src.chunks_exact(w).zip(dst.chunks_exact_mut(w)) {
        let mut sum = row[0] as u32 * (r as u32 + 1) + (1..=r).map(|i| row[i.min(w - 1)] as u32).sum::<u32>();
        for x in 0..w {
            out[x] = ((sum + div / 2) / div) as u8;
            sum += row[(x + r + 1).min(w - 1)] as u32;
            sum -= row[x.saturating_sub(r)] as u32;
        }
    }
}

fn box_blur_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, sums: &mut Vec<u32>) {
    let div = (2 * r + 1) as u32;
    let row = |y: usize| &src[y.min(h - 1) * w..][..w];
    sums.clear();
    sums.extend(row(0).iter().map(|&v| v as u32 * (r as u32 + 1)));
    for i in 1..=r {
        for (s, &v) in sums.iter_mut().zip(row(i)) {
            *s += v as u32;
        }
    }
    for y in 0..h {
        let out = &mut dst[y * w..][..w];
        for (o, s) in out.iter_mut().zip(sums.iter()) {
            *o = ((s + div / 2) / div) as u8;
        }
        let (add, sub) = (row(y + r + 1), row(y.saturating_sub(r)));
        for ((s, &a), &b) in sums.iter_mut().zip(add).zip(sub) {
            *s = *s + a as u32 - b as u32;
        }
    }
}

/// Repeated box blur, close to a Gaussian.
fn blur_plane(plane: &mut [u8], tmp: &mut [u8], w: usize, h: usize, r: usize, sums: &mut Vec<u32>) {
    for _ in 0..PASSES {
        box_blur_h(plane, tmp, w, r);
        box_blur_v(tmp, plane, w, h, r, sums);
    }
}

/// Lens blur: a hexagonal kernel (three line blurs at 0, 60 and 120 degrees) in linear light, so bright spots
/// spread into discs as they do through a real lens. `gamma` above 1 makes highlights bloom.
fn lens_plane(plane: &mut [u8], w: usize, h: usize, r: usize, gamma: f32, threads: usize) {
    // Near-white pixels stand for light much brighter than the sensor can show; boosting them makes them bloom.
    let to_linear: Vec<f32> = (0..256)
        .map(|v| {
            let boost = if gamma > 1.0 {
                1.0 + 12.0 * ((v as f32 - 200.0) / 55.0).max(0.0).powi(2)
            } else {
                1.0
            };
            (v as f32 / 255.0).powf(gamma) * boost
        })
        .collect();
    let mut a: Vec<f32> = plane.iter().map(|&v| to_linear[v as usize]).collect();
    let mut b = vec![0f32; a.len()];
    let steps = (0..=2 * r).map(|k| k as f32 - r as f32);
    let n = (2 * r + 1) as f32;
    for degrees in [0f32, 60.0, 120.0] {
        let (dx, dy) = (degrees.to_radians().cos(), degrees.to_radians().sin());
        let offsets: Vec<(isize, isize)> = steps
            .clone()
            .map(|k| ((k * dx).round() as isize, (k * dy).round() as isize))
            .collect();
        let src = &a;
        let band = h.div_ceil(threads).max(1);
        thread::scope(|s| {
            for (bi, out) in b.chunks_mut(band * w).enumerate() {
                let offsets = &offsets;
                s.spawn(move || {
                    for (i, o) in out.iter_mut().enumerate() {
                        let (x, y) = ((i % w) as isize, (bi * band + i / w) as isize);
                        let mut sum = 0.0;
                        for &(ox, oy) in offsets {
                            let sx = (x + ox).clamp(0, w as isize - 1) as usize;
                            let sy = (y + oy).clamp(0, h as isize - 1) as usize;
                            sum += src[sy * w + sx];
                        }
                        *o = sum / n;
                    }
                });
            }
        });
        std::mem::swap(&mut a, &mut b);
    }
    for (p, v) in plane.iter_mut().zip(&a) {
        *p = (v.clamp(0.0, 1.0).powf(1.0 / gamma) * 255.0).round() as u8;
    }
}

/// Runs `f(first_row, rows)` on row bands of `plane` in parallel.
fn par_rows(plane: &mut [u8], row_len: usize, threads: usize, f: impl Fn(usize, &mut [u8]) + Sync) {
    let rows = plane.len() / row_len;
    let band = rows.div_ceil(threads).max(1);
    thread::scope(|s| {
        for (i, chunk) in plane.chunks_mut(band * row_len).enumerate() {
            let f = &f;
            s.spawn(move || f(i * band, chunk));
        }
    });
}

/// Blurs everything outside a person mask in NV12 frames of one size.
pub struct BackgroundBlur {
    width: usize,
    height: usize,
    /// Frame pixels per blurred-plane pixel, each way.
    scale: usize,
    small_w: usize,
    small_h: usize,
    threads: usize,
    luma: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    alpha: Vec<u8>,
    tmp: Vec<u8>,
    sums: Vec<u32>,
    luma_x: Vec<(u32, u32, u32)>,
    luma_y: Vec<(u32, u32, u32)>,
    chroma_x: Vec<(u32, u32, u32)>,
    chroma_y: Vec<(u32, u32, u32)>,
    /// Hexagonal lens blur instead of the smooth one.
    pub lens: bool,
}

impl BackgroundBlur {
    /// `width` and `height` must be multiples of 8 (16 from 2160 rows up).
    pub fn new(width: usize, height: usize) -> Self {
        let scale = scale_for(height);
        assert!(width.is_multiple_of(2 * scale) && height.is_multiple_of(2 * scale));
        let (small_w, small_h) = (width / scale, height / scale);
        let n = small_w * small_h;
        Self {
            width,
            height,
            scale,
            small_w,
            small_h,
            threads: thread::available_parallelism().map_or(1, |n| n.get()),
            luma: vec![0; n],
            u: vec![0; n],
            v: vec![0; n],
            alpha: vec![0; n],
            tmp: vec![0; n],
            sums: Vec::with_capacity(small_w),
            luma_x: taps(small_w, width),
            luma_y: taps(small_h, height),
            chroma_x: taps(small_w, width / 2),
            chroma_y: taps(small_h, height / 2),
            lens: false,
        }
    }

    /// Blurs `frame` where `mask` is low. `radius` is in frame pixels.
    pub fn apply(&mut self, frame: &mut [u8], mask: &Mask, radius: usize) {
        let (w, h, sw, sh) = (self.width, self.height, self.small_w, self.small_h);
        assert!(frame.len() >= w * h * 3 / 2);
        let (luma, chroma) = frame[..w * h * 3 / 2].split_at_mut(w * h);
        self.downscale(luma, chroma);

        let r = (radius / self.scale).max(1);
        if self.lens {
            // Brightness blurred in linear light, so highlights bloom; colour blurred plainly.
            lens_plane(&mut self.luma, sw, sh, r, 2.6, self.threads);
            lens_plane(&mut self.u, sw, sh, r, 1.0, self.threads);
            lens_plane(&mut self.v, sw, sh, r, 1.0, self.threads);
        } else {
            blur_plane(&mut self.luma, &mut self.tmp, sw, sh, r, &mut self.sums);
            blur_plane(&mut self.u, &mut self.tmp, sw, sh, r, &mut self.sums);
            blur_plane(&mut self.v, &mut self.tmp, sw, sh, r, &mut self.sums);
        }
        self.resample_mask(mask);

        let (alpha, blurred) = (&self.alpha, &self.luma);
        let (tx, ty) = (&self.luma_x, &self.luma_y);
        par_rows(luma, w, self.threads, |first, rows| {
            for (i, row) in rows.chunks_exact_mut(w).enumerate() {
                let (y0, y1, wy) = ty[first + i];
                let (a0, a1) = (&alpha[y0 as usize * sw..][..sw], &alpha[y1 as usize * sw..][..sw]);
                let (b0, b1) = (&blurred[y0 as usize * sw..][..sw], &blurred[y1 as usize * sw..][..sw]);
                for (px, &(x0, x1, wx)) in row.iter_mut().zip(tx) {
                    let (x0, x1) = (x0 as usize, x1 as usize);
                    let a = bilinear(a0[x0], a0[x1], a1[x0], a1[x1], wx, wy);
                    if a < 255 {
                        *px = mix(*px, bilinear(b0[x0], b0[x1], b1[x0], b1[x1], wx, wy), a);
                    }
                }
            }
        });

        let (u, v) = (&self.u, &self.v);
        let (tx, ty) = (&self.chroma_x, &self.chroma_y);
        par_rows(chroma, w, self.threads, |first, rows| {
            for (i, row) in rows.chunks_exact_mut(w).enumerate() {
                let (y0, y1, wy) = ty[first + i];
                let at = |p: &[u8], y: u32| -> usize { (y as usize * sw).min(p.len() - sw) };
                let (a0, a1) = (&alpha[at(alpha, y0)..][..sw], &alpha[at(alpha, y1)..][..sw]);
                let (u0, u1) = (&u[at(u, y0)..][..sw], &u[at(u, y1)..][..sw]);
                let (v0, v1) = (&v[at(v, y0)..][..sw], &v[at(v, y1)..][..sw]);
                for (uv, &(x0, x1, wx)) in row.as_chunks_mut::<2>().0.iter_mut().zip(tx) {
                    let (x0, x1) = (x0 as usize, x1 as usize);
                    let a = bilinear(a0[x0], a0[x1], a1[x0], a1[x1], wx, wy);
                    if a < 255 {
                        uv[0] = mix(uv[0], bilinear(u0[x0], u0[x1], u1[x0], u1[x1], wx, wy), a);
                        uv[1] = mix(uv[1], bilinear(v0[x0], v0[x1], v1[x0], v1[x1], wx, wy), a);
                    }
                }
            }
        });
    }

    /// Shows `background`, an NV12 frame of the same size, where `mask` is low.
    pub fn replace(&mut self, frame: &mut [u8], mask: &Mask, background: &[u8]) {
        let (w, h, sw) = (self.width, self.height, self.small_w);
        let n = w * h * 3 / 2;
        assert!(frame.len() >= n && background.len() >= n);
        self.resample_mask(mask);
        let (luma, chroma) = frame[..n].split_at_mut(w * h);
        let (bg_luma, bg_chroma) = background[..n].split_at(w * h);
        let alpha = &self.alpha;
        for (plane, bg, tx, ty, channels) in [
            (luma, bg_luma, &self.luma_x, &self.luma_y, 1),
            (chroma, bg_chroma, &self.chroma_x, &self.chroma_y, 2),
        ] {
            par_rows(plane, w, self.threads, |first, rows| {
                for (i, row) in rows.chunks_exact_mut(w).enumerate() {
                    let (y0, y1, wy) = ty[first + i];
                    let (a0, a1) = (&alpha[y0 as usize * sw..][..sw], &alpha[y1 as usize * sw..][..sw]);
                    let bg_row = &bg[(first + i) * w..][..w];
                    for (x, (px, &b)) in row.iter_mut().zip(bg_row).enumerate() {
                        let (x0, x1, wx) = tx[x / channels];
                        let (x0, x1) = (x0 as usize, x1 as usize);
                        let a = bilinear(a0[x0], a0[x1], a1[x0], a1[x1], wx, wy);
                        if a < 255 {
                            *px = mix(*px, b as u32, a);
                        }
                    }
                }
            });
        }
    }

    /// Averages luma over scale x scale blocks, and chroma over the matching blocks of its half-size plane.
    fn downscale(&mut self, luma: &[u8], chroma: &[u8]) {
        let (w, sw, s) = (self.width, self.small_w, self.scale);
        let n = (s * s) as u32;
        par_rows(&mut self.luma, sw, self.threads, |first, rows| {
            for (i, out) in rows.chunks_exact_mut(sw).enumerate() {
                let src = &luma[(first + i) * s * w..][..s * w];
                for (x, o) in out.iter_mut().enumerate() {
                    let sum: u32 = (0..s)
                        .flat_map(|dy| src[dy * w + x * s..][..s].iter())
                        .map(|&p| p as u32)
                        .sum();
                    *o = ((sum + n / 2) / n) as u8;
                }
            }
        });
        // Chroma rows hold w / 2 UV pairs; each small pixel covers c x c pairs.
        let c = s / 2;
        let m = (c * c) as u32;
        for sy in 0..self.small_h {
            for sx in 0..sw {
                let (mut u, mut v) = (0u32, 0u32);
                for dy in 0..c {
                    let row = &chroma[(c * sy + dy) * w..][..w];
                    for dx in 0..c {
                        let i = 2 * (c * sx + dx);
                        u += row[i] as u32;
                        v += row[i + 1] as u32;
                    }
                }
                self.u[sy * sw + sx] = ((u + m / 2) / m) as u8;
                self.v[sy * sw + sx] = ((v + m / 2) / m) as u8;
            }
        }
    }

    /// Resamples the person mask to the blurred planes' size as 0-255 alpha.
    fn resample_mask(&mut self, mask: &Mask) {
        let (sw, sh) = (self.small_w, self.small_h);
        let (tx, ty) = (taps(mask.width, sw), taps(mask.height, sh));
        for (y, &(y0, y1, wy)) in ty.iter().enumerate() {
            let (wy, r0, r1) = (wy as f32 / 256.0, y0 as usize * mask.width, y1 as usize * mask.width);
            for (x, &(x0, x1, wx)) in tx.iter().enumerate() {
                let (wx, x0, x1) = (wx as f32 / 256.0, x0 as usize, x1 as usize);
                let d = &mask.data;
                let top = d[r0 + x0] + (d[r0 + x1] - d[r0 + x0]) * wx;
                let bottom = d[r1 + x0] + (d[r1 + x1] - d[r1 + x0]) * wx;
                let p = top + (bottom - top) * wy;
                self.alpha[y * sw + x] = (p.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lens_spreads_a_light_into_a_hexagon() {
        let (w, h) = (41, 41);
        let mut p = vec![0u8; w * h];
        p[20 * w + 20] = 255;
        lens_plane(&mut p, w, h, 6, 2.6, 2);
        let lit = |x: usize, y: usize| p[y * w + x] > 0;
        // Corners of a hexagon lie along 0, 60 and 120 degrees: far to the right, but not as far straight up.
        assert!(lit(20 + 11, 20) && lit(20 - 11, 20));
        assert!(!lit(20, 20 - 12));
        assert!(!lit(20 + 15, 20));
        // Flat areas stay flat.
        let mut flat = vec![120u8; w * h];
        lens_plane(&mut flat, w, h, 6, 2.6, 2);
        assert!(flat.iter().all(|&v| (v as i32 - 120).abs() <= 1));
    }

    const W: usize = 64;
    const H: usize = 32;

    fn frame(f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        let mut v: Vec<u8> = (0..W * H).map(|i| f(i % W, i / W)).collect();
        v.extend((0..W * H / 2).map(|i| if i % 2 == 0 { 100 } else { 150 }));
        v
    }

    fn mask(p: f32) -> Mask {
        Mask {
            width: 16,
            height: 9,
            data: vec![p; 16 * 9],
        }
    }

    #[test]
    fn scores_to_probability() {
        let m = Mask::from_scores(&[4.0, -4.0, -4.0, 4.0], 2, 1).unwrap();
        assert!(m.data[0] < 0.01 && m.data[1] > 0.99);
        assert!(Mask::from_scores(&[0.0; 3], 2, 1).is_none());
    }

    #[test]
    fn follow_moves_towards_next() {
        let mut m = mask(0.0);
        m.follow(&mask(1.0), 0.25);
        assert!(m.data.iter().all(|&p| (p - 0.25).abs() < 1e-6));
    }

    #[test]
    fn person_is_kept() {
        let original = frame(|x, y| ((x * 7 + y * 13) % 256) as u8);
        let mut f = original.clone();
        BackgroundBlur::new(W, H).apply(&mut f, &mask(1.0), 8);
        assert_eq!(f, original);
    }

    #[test]
    fn background_is_blurred() {
        let original = frame(|x, _| if x < W / 2 { 0 } else { 200 });
        let mut f = original.clone();
        BackgroundBlur::new(W, H).apply(&mut f, &mask(0.0), 8);
        // The hard edge in the middle becomes a ramp; flat areas far from it stay flat.
        let row = &f[H / 2 * W..][..W];
        assert_eq!(row[0], 0);
        assert_eq!(row[W - 1], 200);
        assert!(row[W / 2 - 1] > 20 && row[W / 2] < 180, "{row:?}");
        // Uniform chroma is unchanged.
        assert_eq!(&f[W * H..], &original[W * H..]);
    }

    #[test]
    fn background_replaces_outside_the_person() {
        let original = frame(|_, _| 50);
        let mut bg = vec![200u8; W * H];
        bg.extend(vec![128u8; W * H / 2]);
        let mut f = original.clone();
        BackgroundBlur::new(W, H).replace(&mut f, &mask(0.0), &bg);
        assert!(f[..W * H].iter().all(|&p| p == 200));
        let mut f = original.clone();
        BackgroundBlur::new(W, H).replace(&mut f, &mask(1.0), &bg);
        assert_eq!(f, original);
    }

    #[test]
    #[ignore = "timing"]
    fn timing_1080p() {
        let (w, h) = (1920, 1080);
        let mut f: Vec<u8> = (0..w * h * 3 / 2).map(|i| (i % 251) as u8).collect();
        let mut blur = BackgroundBlur::new(w, h);
        let m = Mask {
            width: 256,
            height: 144,
            data: (0..256 * 144).map(|i| if i % 256 < 128 { 0.0 } else { 1.0 }).collect(),
        };
        let start = std::time::Instant::now();
        for _ in 0..30 {
            blur.apply(&mut f, &m, 24);
        }
        println!("{:.2} ms per frame", start.elapsed().as_secs_f64() * 1000.0 / 30.0);
    }
}
