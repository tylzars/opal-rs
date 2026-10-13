//! Cropping a region of an NV12 frame and scaling it back up to the full frame.

use std::thread;

/// A region of the frame in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn full(width: usize, height: usize) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
        }
    }

    /// Moves and shrinks the rect so it lies within a `width` x `height` frame.
    pub fn clamped(self, width: usize, height: usize) -> Self {
        let (fw, fh) = (width.max(1) as f32, height.max(1) as f32);
        if ![self.x, self.y, self.width, self.height].iter().all(|v| v.is_finite()) {
            return Self {
                x: 0.0,
                y: 0.0,
                width: fw,
                height: fh,
            };
        }
        let w = self.width.clamp(1.0, fw);
        let h = self.height.clamp(1.0, fh);
        Self {
            x: self.x.clamp(0.0, fw - w),
            y: self.y.clamp(0.0, fh - h),
            width: w,
            height: h,
        }
    }
}

/// Source index and weight (0 to 256) for each output sample when `count` samples cover `start..start + span`.
fn taps(start: f32, span: f32, count: usize, limit: usize) -> Vec<(u32, u32)> {
    (0..count)
        .map(|i| {
            let pos = (start + (i as f32 + 0.5) * span / count as f32 - 0.5).clamp(0.0, (limit - 1) as f32);
            let i0 = (pos.floor() as usize).min(limit.saturating_sub(2));
            (i0 as u32, ((pos - i0 as f32) * 256.0).round().min(256.0) as u32)
        })
        .collect()
}

/// Writes `region` of `src` scaled to the whole of `dst`. Both are `width` x `height` NV12 frames.
pub fn crop_scale(src: &[u8], dst: &mut [u8], width: usize, height: usize, region: Rect) {
    let (w, h) = (width, height);
    let r = region.clamped(w, h);
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let (src_y, src_uv) = src[..w * h * 3 / 2].split_at(w * h);
    let (dst_y, dst_uv) = dst[..w * h * 3 / 2].split_at_mut(w * h);

    let tx = taps(r.x, r.width, w, w);
    let ty = taps(r.y, r.height, h, h);
    scale_plane(src_y, dst_y, w, 1, &tx, &ty, threads);

    let tx = taps(r.x / 2.0, r.width / 2.0, w / 2, w / 2);
    let ty = taps(r.y / 2.0, r.height / 2.0, h / 2, h / 2);
    scale_plane(src_uv, dst_uv, w, 2, &tx, &ty, threads);
}

/// Bilinear resampling of a plane with `channels` interleaved samples per pixel and `row` bytes per row.
fn scale_plane(
    src: &[u8],
    dst: &mut [u8],
    row: usize,
    channels: usize,
    tx: &[(u32, u32)],
    ty: &[(u32, u32)],
    threads: usize,
) {
    let rows = dst.len() / row;
    let band = rows.div_ceil(threads).max(1);
    thread::scope(|s| {
        for (b, chunk) in dst.chunks_mut(band * row).enumerate() {
            s.spawn(move || {
                for (i, out) in chunk.chunks_exact_mut(row).enumerate() {
                    let (y0, wy) = ty[b * band + i];
                    let r0 = &src[y0 as usize * row..][..row];
                    let r1 = &src[((y0 as usize + 1).min(rows - 1)) * row..][..row];
                    for (px, &(x0, wx)) in out.chunks_exact_mut(channels).zip(tx) {
                        let x0 = x0 as usize * channels;
                        let x1 = (x0 + channels).min(row - channels);
                        for c in 0..channels {
                            let top = r0[x0 + c] as u32 * (256 - wx) + r0[x1 + c] as u32 * wx;
                            let bottom = r1[x0 + c] as u32 * (256 - wx) + r1[x1 + c] as u32 * wx;
                            px[c] = ((top * (256 - wy) + bottom * wy + (1 << 15)) >> 16) as u8;
                        }
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 32;
    const H: usize = 16;

    fn frame() -> Vec<u8> {
        let mut f: Vec<u8> = (0..W * H).map(|i| ((i % W) * 8) as u8).collect();
        f.extend((0..W * H / 2).map(|i| if i % 2 == 0 { 90 } else { 160 }));
        f
    }

    #[test]
    fn full_region_is_a_copy() {
        let src = frame();
        let mut dst = vec![0; src.len()];
        crop_scale(&src, &mut dst, W, H, Rect::full(W, H));
        assert_eq!(dst, src);
    }

    #[test]
    fn left_half_is_stretched() {
        let src = frame();
        let mut dst = vec![0; src.len()];
        let half = Rect {
            x: 0.0,
            y: 0.0,
            width: W as f32 / 2.0,
            height: H as f32 / 2.0,
        };
        crop_scale(&src, &mut dst, W, H, half);
        // Luma ramps 0..248 across the frame; the left half spans 0..120 after scaling.
        assert!(dst[W - 1] <= 124 && dst[W - 1] >= 116, "{}", dst[W - 1]);
        assert_eq!(dst[0], 0);
        assert_eq!(&dst[W * H..W * H + 2], &[90, 160]);
    }

    #[test]
    fn region_is_kept_inside_the_frame() {
        let r = Rect {
            x: 30.0,
            y: -5.0,
            width: 10.0,
            height: 40.0,
        }
        .clamped(W, H);
        assert_eq!(
            r,
            Rect {
                x: 22.0,
                y: 0.0,
                width: 10.0,
                height: 16.0
            }
        );
    }

    #[test]
    #[ignore = "timing"]
    fn timing_1080p() {
        let (w, h) = (1920, 1080);
        let src: Vec<u8> = (0..w * h * 3 / 2).map(|i| (i % 251) as u8).collect();
        let mut dst = vec![0; src.len()];
        let r = Rect {
            x: 300.0,
            y: 100.0,
            width: 1280.0,
            height: 720.0,
        };
        let start = std::time::Instant::now();
        for _ in 0..30 {
            crop_scale(&src, &mut dst, w, h, r);
        }
        println!("{:.2} ms per frame", start.elapsed().as_secs_f64() * 1000.0 / 30.0);
    }
}
