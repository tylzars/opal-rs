//! Pixelation: the frame drawn as big square blocks.

use std::thread;

/// Block size in pixels for `amount` 0 to 1 on a frame `height` tall; 0 means off.
pub fn block_size(amount: f32, height: usize) -> usize {
    if amount <= 0.0 {
        return 0;
    }
    // From 1/90 of the height up to 1/12, rounded to an even size so the half-size colour plane lines up.
    let lo = height as f32 / 90.0;
    let hi = height as f32 / 12.0;
    let s = (lo + (hi - lo) * amount.min(1.0)).round() as usize;
    (s.max(2) + 1) & !1
}

/// Replaces each `block` x `block` square of an NV12 frame with its average.
pub fn pixelate(frame: &mut [u8], width: usize, height: usize, block: usize) {
    if block < 2 {
        return;
    }
    let (luma, chroma) = frame[..width * height * 3 / 2].split_at_mut(width * height);
    let rows = height.div_ceil(block);
    let threads = thread::available_parallelism().map_or(1, |n| n.get()).min(rows);
    let per = rows.div_ceil(threads);
    let half = block / 2;
    thread::scope(|s| {
        for (l, c) in luma
            .chunks_mut(width * block * per)
            .zip(chroma.chunks_mut(width * half * per))
        {
            s.spawn(move || {
                average_blocks(l, width, block, 1);
                average_blocks(c, width, half, 2);
            });
        }
    });
}

/// Averages `size` x `size` blocks of a plane with `step` bytes per sample (2 for interleaved UV).
fn average_blocks(plane: &mut [u8], width: usize, size: usize, step: usize) {
    let rows = plane.len() / width;
    let cols = width / step;
    for by in (0..rows).step_by(size) {
        let bh = size.min(rows - by);
        for bx in (0..cols).step_by(size) {
            let bw = size.min(cols - bx);
            for k in 0..step {
                let mut sum = 0u32;
                for y in by..by + bh {
                    for x in bx..bx + bw {
                        sum += plane[y * width + x * step + k] as u32;
                    }
                }
                let avg = ((sum + (bw * bh) as u32 / 2) / (bw * bh) as u32) as u8;
                for y in by..by + bh {
                    for x in bx..bx + bw {
                        plane[y * width + x * step + k] = avg;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(block_size(0.0, 1080), 0);
        assert_eq!(block_size(1.0, 1080), 90);
        assert_eq!(block_size(0.0001, 1080), 12);
        assert_eq!(block_size(0.5, 720) % 2, 0);
    }

    #[test]
    fn blocks_are_flat_and_keep_the_mean() {
        let (w, h) = (16, 8);
        let mut f: Vec<u8> = (0..w * h).map(|i| (i * 7 % 256) as u8).collect();
        f.extend((0..w * h / 2).map(|i| (100 + i % 40) as u8));
        let mean = |p: &[u8]| p.iter().map(|&v| v as u32).sum::<u32>() as f32 / p.len() as f32;
        let before = mean(&f[..w * h]);
        pixelate(&mut f, w, h, 4);
        assert!((mean(&f[..w * h]) - before).abs() < 1.0);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(f[y * w + x], f[0]);
            }
        }
        // U and V stay separate: one 2x2 chroma block holds a U average and a V average.
        let uv = &f[w * h..];
        assert_eq!(uv[0], uv[2]);
        assert_eq!(uv[1], uv[3]);
    }
}
