//! Preview aids: zebra stripes over clipped areas and a brightness histogram. They only change the preview.

use std::path::PathBuf;

/// Bars in the histogram.
pub const BINS: usize = 64;
/// Luma at or above which a pixel counts as clipped.
const CLIPPED: f32 = 245.0;

fn luma(p: &[u8; 3]) -> f32 {
    0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
}

/// Paints diagonal stripes over clipped pixels of an RGB image.
pub fn zebra(rgb: &mut [u8], [width, _]: [usize; 2]) {
    for (i, p) in rgb.as_chunks_mut::<3>().0.iter_mut().enumerate() {
        if luma(p) >= CLIPPED {
            let (x, y) = (i % width, i / width);
            let dark = (x + y) / 4 % 2 == 0;
            *p = if dark { [40, 40, 40] } else { [255, 255, 255] };
        }
    }
}

/// Pixel counts per brightness band.
pub fn histogram(rgb: &[u8]) -> [u32; BINS] {
    let mut bins = [0u32; BINS];
    for p in rgb.as_chunks::<3>().0 {
        bins[(luma(p) as usize * BINS / 256).min(BINS - 1)] += 1;
    }
    bins
}

/// Where the panel keeps its own choices: `~/.config/opal-rs/panel`.
fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("opal-rs").join("panel"))
}

/// The saved (zebra, histogram) switches.
pub fn load() -> (bool, bool) {
    let text = path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let on = |name: &str| text.lines().any(|l| l.trim() == format!("{name} on"));
    (on("zebra"), on("histogram"))
}

pub fn save(zebra: bool, histogram: bool) {
    let Some(p) = path() else { return };
    let word = |b: bool| if b { "on" } else { "off" };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, format!("zebra {}\nhistogram {}\n", word(zebra), word(histogram)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripes_only_on_clipped_pixels() {
        let mut rgb = vec![255u8; 8 * 8 * 3];
        rgb[..3].copy_from_slice(&[10, 20, 30]);
        zebra(&mut rgb, [8, 8]);
        assert_eq!(&rgb[..3], &[10, 20, 30]);
        assert_eq!(&rgb[3..6], &[40, 40, 40]);
        assert_eq!(&rgb[4 * 3..5 * 3], &[255, 255, 255]);
    }

    #[test]
    fn histogram_counts_every_pixel() {
        let mut rgb = vec![0u8; 10 * 3];
        rgb[3..6].copy_from_slice(&[255, 255, 255]);
        let h = histogram(&rgb);
        assert_eq!(h.iter().sum::<u32>(), 10);
        assert_eq!(h[0], 9);
        assert_eq!(h[BINS - 1], 1);
    }
}
