//! Stickers drawn over the video: text, a name card and pictures, as RGBA images composited onto NV12 frames.

/// An RGBA image with straight (not premultiplied) alpha.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rgba {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Rgba {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0; width * height * 4],
        }
    }

    /// Draws `alpha` coverage in `colour` at (`x`, `y`), over what is there.
    fn blend(&mut self, x: usize, y: usize, colour: [u8; 4], alpha: f32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let a = alpha * colour[3] as f32 / 255.0;
        let p = &mut self.data[(y * self.width + x) * 4..][..4];
        let under = p[3] as f32 / 255.0;
        let out = a + under * (1.0 - a);
        if out <= 0.0 {
            return;
        }
        for c in 0..3 {
            p[c] = ((colour[c] as f32 * a + p[c] as f32 * under * (1.0 - a)) / out).round() as u8;
        }
        p[3] = (out * 255.0).round() as u8;
    }
}

/// A font loaded from a TrueType or OpenType file.
pub struct Font(fontdue::Font);

impl Font {
    pub fn load(data: &[u8]) -> Option<Self> {
        fontdue::Font::from_bytes(data, fontdue::FontSettings::default())
            .ok()
            .map(Self)
    }

    /// Width of `text` at `px` and the line's ascent and descent.
    fn measure(&self, text: &str, px: f32) -> (f32, f32, f32) {
        let width = text.chars().map(|c| self.0.metrics(c, px).advance_width).sum();
        let line = self.0.horizontal_line_metrics(px);
        line.map_or((width, px * 0.8, px * 0.2), |l| (width, l.ascent, -l.descent))
    }

    /// Draws `text` with its baseline at `baseline`, starting at `x`.
    fn draw(&self, img: &mut Rgba, text: &str, px: f32, x: f32, baseline: f32, colour: [u8; 4]) {
        let mut pen = x;
        for c in text.chars() {
            let (m, coverage) = self.0.rasterize(c, px);
            let left = pen + m.xmin as f32;
            let top = baseline - m.height as f32 - m.ymin as f32;
            for gy in 0..m.height {
                for gx in 0..m.width {
                    let a = coverage[gy * m.width + gx] as f32 / 255.0;
                    let (px_, py) = (left + gx as f32, top + gy as f32);
                    if a > 0.0 && px_ >= 0.0 && py >= 0.0 {
                        img.blend(px_ as usize, py as usize, colour, a);
                    }
                }
            }
            pen += m.advance_width;
        }
    }

    /// `text` without the characters this font has no shape for.
    fn drawable(&self, text: &str) -> String {
        text.chars()
            .filter(|&c| c == ' ' || self.0.lookup_glyph_index(c) != 0)
            .collect()
    }

    /// `text` in `colour` with a soft dark outline so it reads on any background. `height` is the line height.
    pub fn text(&self, text: &str, height: usize, colour: [u8; 3]) -> Rgba {
        let text = &self.drawable(text);
        let px = height as f32;
        let (w, ascent, descent) = self.measure(text, px);
        let pad = (px * 0.08).ceil();
        let mut img = Rgba::new(
            (w + 2.0 * pad).ceil() as usize + 1,
            (ascent + descent + 2.0 * pad).ceil() as usize,
        );
        let shadow = [0, 0, 0, 150];
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            let o = (px * 0.04).max(1.0);
            self.draw(&mut img, text, px, pad + dx * o, pad + ascent + dy * o, shadow);
        }
        self.draw(
            &mut img,
            text,
            px,
            pad,
            pad + ascent,
            [colour[0], colour[1], colour[2], 255],
        );
        img
    }

    /// A lower-third name card: `name` large over `title` small, on a dark rounded panel.
    pub fn card(&self, name: &str, title: &str, height: usize) -> Rgba {
        let (name, title) = (&self.drawable(name), &self.drawable(title));
        let big = height as f32 * 0.5;
        let small = height as f32 * 0.3;
        let (nw, na, _) = self.measure(name, big);
        let (tw, ta, _) = self.measure(title, small);
        let pad = height as f32 * 0.14;
        let width = (nw.max(tw) + 2.5 * pad).ceil() as usize;
        let mut img = Rgba::new(width, height);
        let r = height as f32 * 0.12;
        for y in 0..height {
            for x in 0..width {
                // Rounded rectangle coverage.
                let qx = (x as f32 + 0.5 - width as f32 / 2.0).abs() - (width as f32 / 2.0 - r);
                let qy = (y as f32 + 0.5 - height as f32 / 2.0).abs() - (height as f32 / 2.0 - r);
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() - r;
                let a = (0.5 - d).clamp(0.0, 1.0);
                img.blend(x, y, [20, 22, 28, 215], a);
            }
        }
        // An accent bar on the left.
        for y in (pad as usize)..(height - pad as usize) {
            for x in (pad * 0.5) as usize..(pad * 0.5 + height as f32 * 0.04).ceil() as usize {
                img.blend(x, y, [90, 140, 255, 255], 1.0);
            }
        }
        let left = pad * 1.5;
        let name_base = pad + na;
        self.draw(&mut img, name, big, left, name_base, [255, 255, 255, 255]);
        self.draw(
            &mut img,
            title,
            small,
            left,
            name_base + pad * 0.6 + ta,
            [200, 204, 214, 255],
        );
        img
    }
}

/// Scales an RGBA picture to `height` pixels tall, keeping its shape.
pub fn scale(src: &Rgba, height: usize) -> Rgba {
    // At most MAX_SIDE either way, whatever the shape.
    const MAX_SIDE: usize = 8192;
    let (sw, sh) = (src.width.max(1), src.height.max(1));
    let height = height.min(MAX_SIDE).min(MAX_SIDE * sh / sw).max(1);
    let width = (sw * height / sh).clamp(1, MAX_SIDE);
    let mut out = Rgba::new(width, height);
    for y in 0..height {
        for x in 0..width {
            // Box filter over the source pixels this one covers.
            let (sx0, sx1) = (
                x * src.width / width,
                ((x + 1) * src.width / width).max(x * src.width / width + 1),
            );
            let (sy0, sy1) = (
                y * src.height / height,
                ((y + 1) * src.height / height).max(y * src.height / height + 1),
            );
            // u64: a big picture shrunk to a small sticker sums many pixels into one.
            let mut acc = [0u64; 4];
            let mut n = 0;
            for sy in sy0..sy1.min(src.height) {
                for sx in sx0..sx1.min(src.width) {
                    let p = &src.data[(sy * src.width + sx) * 4..][..4];
                    let a = p[3] as u64;
                    for c in 0..3 {
                        acc[c] += p[c] as u64 * a;
                    }
                    acc[3] += a;
                    n += 1;
                }
            }
            if n > 0 && acc[3] > 0 {
                let o = &mut out.data[(y * width + x) * 4..][..4];
                for c in 0..3 {
                    o[c] = (acc[c] / acc[3]) as u8;
                }
                o[3] = (acc[3] / n as u64) as u8;
            }
        }
    }
    out
}

/// Draws `img` onto an NV12 frame with its top left at (`x0`, `y0`), which may be partly outside.
/// `hide` gives, for a frame pixel, how much of the sticker to hide there (for stickers behind the person).
pub fn composite(
    frame: &mut [u8],
    width: usize,
    height: usize,
    img: &Rgba,
    x0: i64,
    y0: i64,
    hide: Option<&dyn Fn(usize, usize) -> f32>,
) {
    let (luma, chroma) = frame[..width * height * 3 / 2].split_at_mut(width * height);
    let yuv = |p: &[u8]| {
        let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
        [
            0.299 * r + 0.587 * g + 0.114 * b,
            128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b,
            128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b,
        ]
    };
    let alpha = |sx: usize, sy: usize, fx: usize, fy: usize| {
        let a = img.data[(sy * img.width + sx) * 4 + 3] as f32 / 255.0;
        a * (1.0 - hide.map_or(0.0, |h| h(fx, fy)))
    };
    // Luma per pixel.
    for sy in 0..img.height {
        let fy = y0 + sy as i64;
        if fy < 0 || fy >= height as i64 {
            continue;
        }
        for sx in 0..img.width {
            let fx = x0 + sx as i64;
            if fx < 0 || fx >= width as i64 {
                continue;
            }
            let (fx, fy) = (fx as usize, fy as usize);
            let a = alpha(sx, sy, fx, fy);
            if a <= 0.0 {
                continue;
            }
            let y = yuv(&img.data[(sy * img.width + sx) * 4..][..3])[0];
            let p = &mut luma[fy * width + fx];
            *p = (*p as f32 + (y - *p as f32) * a).round() as u8;
        }
    }
    // Chroma per 2x2 block, from the sticker pixel at its top left.
    let (cx0, cy0) = (x0.max(0) as usize / 2, y0.max(0) as usize / 2);
    let cx1 = ((x0 + img.width as i64 + 1).clamp(0, width as i64) as usize) / 2;
    let cy1 = ((y0 + img.height as i64 + 1).clamp(0, height as i64) as usize) / 2;
    for cy in cy0..cy1 {
        for cx in cx0..cx1 {
            let (fx, fy) = (2 * cx, 2 * cy);
            let (sx, sy) = (fx as i64 - x0, fy as i64 - y0);
            if sx < 0 || sy < 0 || sx >= img.width as i64 || sy >= img.height as i64 {
                continue;
            }
            let (sx, sy) = (sx as usize, sy as usize);
            let a = alpha(sx, sy, fx, fy);
            if a <= 0.0 {
                continue;
            }
            let c = yuv(&img.data[(sy * img.width + sx) * 4..][..3]);
            let uv = &mut chroma[cy * width + 2 * cx..][..2];
            for k in 0..2 {
                uv[k] = (uv[k] as f32 + (c[k + 1] - uv[k] as f32) * a).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> Option<Font> {
        [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/ubuntu/Ubuntu-M.ttf",
        ]
        .iter()
        .find_map(|p| Font::load(&std::fs::read(p).ok()?))
    }

    #[test]
    fn wide_picture_stays_within_limits() {
        let img = Rgba::new(20000, 2);
        let s = scale(&img, 1080);
        assert!(s.width <= 8192 && s.height >= 1, "{}x{}", s.width, s.height);
    }

    #[test]
    fn big_picture_shrinks_to_one_pixel() {
        let mut img = Rgba::new(300, 300);
        img.data.fill(255);
        assert_eq!(scale(&img, 1).data, [255, 255, 255, 255]);
    }

    #[test]
    fn composite_blends_and_clips() {
        let (w, h) = (8, 4);
        let mut frame = vec![0u8; w * h];
        frame.extend(vec![128u8; w * h / 2]);
        let mut img = Rgba::new(4, 2);
        for p in img.data.chunks_mut(4) {
            p.copy_from_slice(&[255, 255, 255, 255]);
        }
        composite(&mut frame, w, h, &img, 6, 3, None);
        assert_eq!(frame[3 * w + 6], 255);
        assert_eq!(frame[3 * w + 5], 0);
        assert_eq!(frame[0], 0);
        // Hidden where `hide` says so.
        let mut f2 = vec![0u8; w * h];
        f2.extend(vec![128u8; w * h / 2]);
        composite(&mut f2, w, h, &img, 0, 0, Some(&|x, _| if x < 2 { 1.0 } else { 0.0 }));
        assert_eq!(f2[0], 0);
        assert_eq!(f2[2], 255);
    }

    #[test]
    fn text_and_card_have_ink() {
        let Some(f) = font() else { return };
        let t = f.text("Hello", 40, [255, 255, 255]);
        assert!(t.width > 60 && t.height >= 40);
        assert!(t.data.chunks(4).any(|p| p[3] == 255 && p[0] == 255));
        let c = f.card("Ada Lovelace", "Engineer", 120);
        assert_eq!(c.height, 120);
        assert!(c.width > 200);
    }

    #[test]
    fn scale_keeps_shape() {
        let mut src = Rgba::new(40, 20);
        src.data.fill(255);
        let s = scale(&src, 10);
        assert_eq!((s.width, s.height), (20, 10));
        assert!(s.data.iter().all(|&v| v == 255));
    }
}
