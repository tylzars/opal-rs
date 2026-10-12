//! A short recorded clip played back in place of the live video, kept at half size to save memory.

/// Half-size copy of an NV12 frame (each 2x2 block averaged; chroma likewise).
pub fn halve(src: &[u8], width: usize, height: usize) -> Vec<u8> {
    let (w2, h2) = (width / 2, height / 2);
    let mut out = vec![0u8; w2 * h2 * 3 / 2];
    let avg4 = |a: u8, b: u8, c: u8, d: u8| ((a as u16 + b as u16 + c as u16 + d as u16 + 2) / 4) as u8;
    for y in 0..h2 {
        for x in 0..w2 {
            let i = 2 * y * width + 2 * x;
            out[y * w2 + x] = avg4(src[i], src[i + 1], src[i + width], src[i + width + 1]);
        }
    }
    // Chroma: the source plane is (w/2 x h/2) UV pairs; the half-size one is (w/4 x h/4) pairs.
    let (cw, ch) = (w2 / 2, h2 / 2);
    let (src_uv, out_uv) = (&src[width * height..], &mut out[w2 * h2..]);
    for y in 0..ch {
        for x in 0..cw {
            for k in 0..2 {
                let at = |yy: usize, xx: usize| src_uv[yy * width + 2 * xx + k];
                out_uv[y * w2 + 2 * x + k] = avg4(
                    at(2 * y, 2 * x),
                    at(2 * y, 2 * x + 1),
                    at(2 * y + 1, 2 * x),
                    at(2 * y + 1, 2 * x + 1),
                );
            }
        }
    }
    out
}

/// Writes a half-size frame from [`halve`] back at full size into `dst` (`width` x `height` NV12).
pub fn double(src: &[u8], dst: &mut [u8], width: usize, height: usize) {
    let (w2, h2) = (width / 2, height / 2);
    if w2 < 2 || h2 < 2 || src.len() < w2 * h2 * 3 / 2 || dst.len() < width * height * 3 / 2 {
        return;
    }
    for y in 0..height {
        let sy = (y / 2).min(h2 - 1);
        for x in 0..width {
            dst[y * width + x] = src[sy * w2 + (x / 2).min(w2 - 1)];
        }
    }
    let (cw, ch) = (w2 / 2, h2 / 2);
    let (src_uv, dst_uv) = (&src[w2 * h2..], &mut dst[width * height..]);
    for y in 0..height / 2 {
        let sy = (y / 2).min(ch.saturating_sub(1));
        for x in 0..width / 2 {
            let sx = (x / 2).min(cw.saturating_sub(1));
            for k in 0..2 {
                dst_uv[y * width + 2 * x + k] = src_uv[sy * w2 + 2 * sx + k];
            }
        }
    }
}

/// Which frame of an `n`-frame clip shows at playback step `step`: round and round, or forth and back.
pub fn clip_frame(step: usize, n: usize, bounce: bool) -> usize {
    if n <= 1 {
        return 0;
    }
    if !bounce {
        return step % n;
    }
    let period = 2 * (n - 1);
    let s = step % period;
    if s < n { s } else { period - s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halve_and_double_keep_flat_frames() {
        let (w, h) = (8, 4);
        let mut f = vec![90u8; w * h];
        f.extend(vec![140u8; w * h / 2]);
        let small = halve(&f, w, h);
        assert_eq!(small.len(), w * h * 3 / 8);
        assert!(small[..w * h / 4].iter().all(|&v| v == 90));
        assert!(small[w * h / 4..].iter().all(|&v| v == 140));
        let mut back = vec![0u8; f.len()];
        double(&small, &mut back, w, h);
        assert_eq!(back, f);
    }

    #[test]
    fn halve_averages_blocks() {
        let (w, h) = (4, 4);
        let mut f: Vec<u8> = vec![0, 100, 0, 0, 100, 200, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        f.extend(vec![128u8; 8]);
        assert_eq!(halve(&f, w, h)[0], 100);
    }

    #[test]
    fn clip_order() {
        let fwd: Vec<usize> = (0..7).map(|s| clip_frame(s, 3, false)).collect();
        assert_eq!(fwd, [0, 1, 2, 0, 1, 2, 0]);
        let bounce: Vec<usize> = (0..7).map(|s| clip_frame(s, 3, true)).collect();
        assert_eq!(bounce, [0, 1, 2, 1, 0, 1, 2]);
        assert_eq!(clip_frame(5, 1, true), 0);
    }
}
