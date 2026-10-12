//! Colour lookup tables in the Adobe .cube format, applied to full-range BT.601 NV12 frames.

use std::thread;

/// Grid points per axis of the table built for YUV frames.
const GRID: usize = 33;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("line {0}: {1}")]
    Parse(usize, String),
    #[error("1D tables are not supported")]
    OneDimensional,
    #[error("expected {expected} entries, found {found}")]
    Size { expected: usize, found: usize },
}

/// A 3D table from RGB to RGB, red changing fastest.
#[derive(Clone, Debug, PartialEq)]
pub struct Cube {
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub data: Vec<[f32; 3]>,
}

fn floats(words: &[&str], line: usize) -> Result<[f32; 3], Error> {
    let mut v = [0.0; 3];
    for (o, w) in v.iter_mut().zip(words) {
        *o = w.parse().map_err(|_| Error::Parse(line, format!("bad number {w}")))?;
    }
    if words.len() != 3 {
        return Err(Error::Parse(line, "expected three numbers".into()));
    }
    Ok(v)
}

impl Cube {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut size = 0;
        let (mut domain_min, mut domain_max) = ([0.0; 3], [1.0; 3]);
        let mut data = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let n = i + 1;
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                [] => {}
                [w, ..] if w.starts_with('#') || *w == "TITLE" => {}
                ["LUT_1D_SIZE", ..] => return Err(Error::OneDimensional),
                ["LUT_3D_SIZE", s] => {
                    size = s.parse().map_err(|_| Error::Parse(n, format!("bad size {s}")))?;
                    if !(2..=256).contains(&size) {
                        return Err(Error::Parse(n, format!("bad size {size}")));
                    }
                }
                ["DOMAIN_MIN", rest @ ..] => domain_min = floats(rest, n)?,
                ["DOMAIN_MAX", rest @ ..] => domain_max = floats(rest, n)?,
                [w, ..] if w.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) => {}
                values => {
                    if size == 0 {
                        return Err(Error::Parse(n, "data before LUT_3D_SIZE".into()));
                    }
                    data.push(floats(values, n)?);
                }
            }
        }
        let expected = size * size * size;
        if size == 0 || data.len() != expected {
            return Err(Error::Size {
                expected,
                found: data.len(),
            });
        }
        Ok(Self {
            size,
            domain_min,
            domain_max,
            data,
        })
    }

    /// Looks up an RGB colour with trilinear interpolation.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        let mut idx = [0usize; 3];
        let mut frac = [0f32; 3];
        for c in 0..3 {
            let t = (rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c]);
            let p = t.clamp(0.0, 1.0) * (n - 1) as f32;
            idx[c] = (p.floor() as usize).min(n - 2);
            frac[c] = p - idx[c] as f32;
        }
        let at = |r: usize, g: usize, b: usize| self.data[(b * n + g) * n + r];
        let mut out = [0f32; 3];
        for (dr, dg, db) in corners(2) {
            let w = (if dr == 1 { frac[0] } else { 1.0 - frac[0] })
                * (if dg == 1 { frac[1] } else { 1.0 - frac[1] })
                * (if db == 1 { frac[2] } else { 1.0 - frac[2] });
            let v = at(idx[0] + dr, idx[1] + dg, idx[2] + db);
            for c in 0..3 {
                out[c] += v[c] * w;
            }
        }
        out
    }
}

/// Every (x, y, z) in an n x n x n grid, x changing fastest.
fn corners(n: usize) -> impl Iterator<Item = (usize, usize, usize)> {
    (0..n * n * n).map(move |i| (i % n, i / n % n, i / (n * n)))
}

fn yuv_to_rgb(y: f32, u: f32, v: f32) -> [f32; 3] {
    let (u, v) = (u - 128.0, v - 128.0);
    [
        (y + 1.402 * v) / 255.0,
        (y - 0.344_136 * u - 0.714_136 * v) / 255.0,
        (y + 1.772 * u) / 255.0,
    ]
}

fn rgb_to_yuv(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(|c| c.clamp(0.0, 1.0) * 255.0);
    [
        0.299 * r + 0.587 * g + 0.114 * b,
        128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b,
        128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b,
    ]
}

/// A cube resampled on a YUV grid, ready to apply to NV12 frames.
pub struct YuvLut {
    /// Output Y, U, V per grid point, Y changing fastest.
    grid: Vec<[u8; 3]>,
    /// Grid cell and weight (0 to 256) for each 8-bit value.
    taps: [(u16, u16); 256],
    threads: usize,
}

impl YuvLut {
    pub fn new(cube: &Cube) -> Self {
        let step = 255.0 / (GRID - 1) as f32;
        let mut grid = Vec::with_capacity(GRID * GRID * GRID);
        for (y, u, v) in corners(GRID) {
            let rgb = yuv_to_rgb(y as f32 * step, u as f32 * step, v as f32 * step);
            let out = rgb_to_yuv(cube.sample(rgb));
            grid.push(out.map(|c| c.round().clamp(0.0, 255.0) as u8));
        }
        let mut taps = [(0, 0); 256];
        for (i, t) in taps.iter_mut().enumerate() {
            let p = i as f32 / step;
            let cell = (p.floor() as usize).min(GRID - 2);
            *t = (cell as u16, ((p - cell as f32) * 256.0).round() as u16);
        }
        Self {
            grid,
            taps,
            threads: thread::available_parallelism().map_or(1, |n| n.get()),
        }
    }

    #[inline]
    fn lookup(&self, y: u8, u: u8, v: u8) -> [u32; 3] {
        let (ty, tu, tv) = (self.taps[y as usize], self.taps[u as usize], self.taps[v as usize]);
        let mut out = [0u32; 3];
        for (dy, du, dv) in corners(2) {
            let w = (if dy == 1 { ty.1 } else { 256 - ty.1 }) as u32
                * (if du == 1 { tu.1 } else { 256 - tu.1 }) as u32
                * (if dv == 1 { tv.1 } else { 256 - tv.1 }) as u32;
            let i = ((tv.0 as usize + dv) * GRID + tu.0 as usize + du) * GRID + ty.0 as usize + dy;
            let g = self.grid[i];
            for c in 0..3 {
                out[c] += g[c] as u32 * w;
            }
        }
        out.map(|c| (c + (1 << 23)) >> 24)
    }

    /// Grid row offsets and weights of the four U, V corners around (u, v), shared by a 2x2 block of pixels.
    fn uv_weights(&self, u: u8, v: u8) -> [(usize, u32); 4] {
        let (tu, tv) = (self.taps[u as usize], self.taps[v as usize]);
        let mut out = [(0, 0); 4];
        for (k, (du, dv)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
            let w = (if du == 1 { tu.1 } else { 256 - tu.1 }) as u32 * (if dv == 1 { tv.1 } else { 256 - tv.1 }) as u32;
            out[k] = (((tv.0 as usize + dv) * GRID + tu.0 as usize + du) * GRID, w);
        }
        out
    }

    /// The Y output of [`Self::lookup`] for one pixel, given its block's [`Self::uv_weights`].
    fn lookup_y(&self, y: u8, block: &[(usize, u32); 4]) -> u32 {
        let ty = self.taps[y as usize];
        let (w0, w1) = ((256 - ty.1) as u32, ty.1 as u32);
        let mut out = 0u32;
        for &(row, w) in block {
            let i = row + ty.0 as usize;
            out += (self.grid[i][0] as u32 * w0 + self.grid[i + 1][0] as u32 * w1) * w;
        }
        (out + (1 << 23)) >> 24
    }

    /// Applies the table to an NV12 frame. `amount` 1 is the full look, 0 leaves the frame unchanged.
    pub fn apply(&self, frame: &mut [u8], width: usize, height: usize, amount: f32) {
        let amount = (amount.clamp(0.0, 1.0) * 256.0) as i32;
        let mix = |old: u8, new: u32| (old as i32 + (((new as i32 - old as i32) * amount) >> 8)) as u8;
        let (luma, chroma) = frame[..width * height * 3 / 2].split_at_mut(width * height);
        let rows = height / 2;
        let band = rows.div_ceil(self.threads).max(1);
        thread::scope(|s| {
            for (l, c) in luma.chunks_mut(2 * width * band).zip(chroma.chunks_mut(width * band)) {
                s.spawn(move || {
                    for (pair, uv_row) in l.chunks_exact_mut(2 * width).zip(c.chunks_exact_mut(width)) {
                        let (top, bottom) = pair.split_at_mut(width);
                        for (x, uv) in uv_row.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                            let (u, v) = (uv[0], uv[1]);
                            let block = self.uv_weights(u, v);
                            let px = [&mut top[2 * x..2 * x + 2], &mut bottom[2 * x..2 * x + 2]];
                            let mut sum = 0u32;
                            for p in px.into_iter().flatten() {
                                sum += *p as u32;
                                *p = mix(*p, self.lookup_y(*p, &block));
                            }
                            let out = self.lookup(((sum + 2) / 4) as u8, u, v);
                            uv[0] = mix(u, out[1]);
                            uv[1] = mix(v, out[2]);
                        }
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_lookup_matches_the_full_one() {
        let mut cube = Cube::parse(&identity(9)).unwrap();
        for (i, c) in cube.data.iter_mut().enumerate() {
            *c = [
                (c[0] * 0.8 + 0.1).min(1.0),
                c[1] * c[1],
                (c[2] + (i % 7) as f32 * 0.01).min(1.0),
            ];
        }
        let lut = YuvLut::new(&cube);
        for y in (0..=255).step_by(3) {
            for u in (0..=255).step_by(17) {
                for v in (0..=255).step_by(13) {
                    let block = lut.uv_weights(u, v);
                    assert_eq!(lut.lookup_y(y, &block), lut.lookup(y, u, v)[0], "{y} {u} {v}");
                }
            }
        }
    }

    fn identity(n: usize) -> String {
        let mut s = format!("TITLE \"id\"\nLUT_3D_SIZE {n}\n");
        for (r, g, b) in corners(n) {
            let f = |i: usize| i as f32 / (n - 1) as f32;
            s += &format!("{} {} {}\n", f(r), f(g), f(b));
        }
        s
    }

    fn frame() -> Vec<u8> {
        let (w, h) = (16, 8);
        let mut f: Vec<u8> = (0..w * h).map(|i| (60 + i) as u8).collect();
        f.extend((0..w * h / 2).map(|i| (120 + i % 16) as u8));
        f
    }

    #[test]
    fn parses_cube() {
        let c = Cube::parse(&identity(3)).unwrap();
        assert_eq!(c.size, 3);
        assert_eq!(c.data[1], [0.5, 0.0, 0.0]);
        assert_eq!(c.sample([0.25, 0.5, 1.0]), [0.25, 0.5, 1.0]);
    }

    #[test]
    fn rejects_bad_cubes() {
        assert_eq!(Cube::parse("LUT_1D_SIZE 4\n"), Err(Error::OneDimensional));
        assert!(matches!(Cube::parse("0 0 0\n"), Err(Error::Parse(1, _))));
        assert!(matches!(Cube::parse("LUT_3D_SIZE 2\n0 0 0\n"), Err(Error::Size { .. })));
    }

    #[test]
    fn identity_keeps_frame() {
        let lut = YuvLut::new(&Cube::parse(&identity(17)).unwrap());
        let original = frame();
        let mut f = original.clone();
        lut.apply(&mut f, 16, 8, 1.0);
        for (a, b) in f.iter().zip(&original) {
            assert!((*a as i32 - *b as i32).abs() <= 2, "{a} vs {b}");
        }
    }

    #[test]
    fn inverts_and_mixes() {
        let mut text = "LUT_3D_SIZE 2\n".to_owned();
        for (r, g, b) in corners(2) {
            text += &format!("{} {} {}\n", 1 - r, 1 - g, 1 - b);
        }
        let lut = YuvLut::new(&Cube::parse(&text).unwrap());
        let mut f = vec![200u8; 16 * 8];
        f.extend(vec![128u8; 16 * 4]);
        lut.apply(&mut f, 16, 8, 1.0);
        assert!((f[0] as i32 - 55).abs() <= 2, "{}", f[0]);
        let mut g = vec![200u8; 16 * 8];
        g.extend(vec![128u8; 16 * 4]);
        lut.apply(&mut g, 16, 8, 0.0);
        assert_eq!(g[0], 200);
    }

    #[test]
    #[ignore = "timing"]
    fn timing_1080p() {
        let lut = YuvLut::new(&Cube::parse(&identity(33)).unwrap());
        let mut f: Vec<u8> = (0..1920 * 1080 * 3 / 2).map(|i| (i % 251) as u8).collect();
        let start = std::time::Instant::now();
        for _ in 0..30 {
            lut.apply(&mut f, 1920, 1080, 1.0);
        }
        println!("{:.2} ms per frame", start.elapsed().as_secs_f64() * 1000.0 / 30.0);
    }
}
