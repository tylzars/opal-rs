//! Built-in colour looks, made as 3D tables from simple colour adjustments.

use crate::lut::Cube;

/// Grid points per axis of a built-in table.
const SIZE: usize = 33;

/// Names of the built-in looks, in menu order.
pub const LOOKS: [&str; 10] = [
    "mono", "noir", "tonal", "fade", "chrome", "instant", "process", "transfer", "warm", "cool",
];

fn luma([r, g, b]: [f32; 3]) -> f32 {
    0.299 * r + 0.587 * g + 0.114 * b
}

/// Moves each channel `amount` of the way from grey towards the colour (0 grey, 1 unchanged, above 1 more vivid).
fn saturate(c: [f32; 3], amount: f32) -> [f32; 3] {
    let y = luma(c);
    c.map(|v| y + (v - y) * amount)
}

/// S-shaped contrast around mid grey: 0 unchanged, 1 strong.
fn contrast(v: f32, amount: f32) -> f32 {
    let s = v * v * (3.0 - 2.0 * v);
    v + (s - v) * amount
}

/// Maps 0..1 onto `black`..`white`.
fn range(v: f32, black: f32, white: f32) -> f32 {
    black + v * (white - black)
}

/// Adds the `shadows` colour to dark tones and the `highlights` colour to light ones.
fn split_tone(c: [f32; 3], shadows: [f32; 3], highlights: [f32; 3]) -> [f32; 3] {
    let y = luma(c);
    let (low, high) = ((1.0 - y).powi(2), y * y);
    [0, 1, 2].map(|i| c[i] + shadows[i] * low + highlights[i] * high)
}

fn grade(name: &str, c: [f32; 3]) -> Option<[f32; 3]> {
    Some(match name {
        "mono" => [luma(c); 3],
        "noir" => [contrast(luma(c), 0.9); 3],
        "tonal" => {
            let y = range(contrast(luma(c), 0.3), 0.04, 0.96);
            [y; 3]
        }
        "fade" => saturate(c, 0.7).map(|v| range(v, 0.1, 0.92)),
        "chrome" => saturate(c, 1.35).map(|v| contrast(v, 0.35)),
        "instant" => {
            let c = saturate(c, 0.85);
            split_tone(c, [0.05, 0.03, 0.0], [0.03, 0.01, -0.04]).map(|v| range(v, 0.06, 0.95))
        }
        "process" => {
            let c = split_tone(saturate(c, 1.1), [-0.03, 0.02, 0.08], [0.06, 0.03, -0.06]);
            c.map(|v| contrast(v, 0.25))
        }
        "transfer" => {
            let c = split_tone(saturate(c, 0.9), [0.04, 0.02, -0.02], [0.05, 0.02, -0.08]);
            c.map(|v| range(contrast(v, 0.15), 0.03, 0.97))
        }
        "warm" => [c[0] * 1.06 + 0.01, c[1] * 1.01, c[2] * 0.9],
        "cool" => [c[0] * 0.92, c[1] * 0.99, c[2] * 1.06 + 0.01],
        _ => return None,
    })
}

/// The table for a built-in look, or `None` if there is none called `name`.
pub fn builtin(name: &str) -> Option<Cube> {
    grade(name, [0.0; 3])?;
    let step = 1.0 / (SIZE - 1) as f32;
    let data = (0..SIZE * SIZE * SIZE)
        .map(|i| {
            let rgb = [i % SIZE, i / SIZE % SIZE, i / (SIZE * SIZE)].map(|v| v as f32 * step);
            grade(name, rgb).unwrap().map(|v| v.clamp(0.0, 1.0))
        })
        .collect();
    Some(Cube {
        size: SIZE,
        domain_min: [0.0; 3],
        domain_max: [1.0; 3],
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_look_builds() {
        for name in LOOKS {
            let cube = builtin(name).unwrap();
            assert_eq!(cube.data.len(), SIZE * SIZE * SIZE, "{name}");
        }
        assert!(builtin("nope").is_none());
    }

    #[test]
    fn mono_is_grey_and_keeps_black_and_white() {
        let cube = builtin("mono").unwrap();
        let [r, g, b] = cube.sample([0.8, 0.2, 0.1]);
        assert!((r - g).abs() < 1e-4 && (g - b).abs() < 1e-4);
        assert!(cube.sample([0.0; 3])[0] < 1e-4);
        assert!(cube.sample([1.0; 3])[0] > 0.999);
    }

    #[test]
    fn warm_and_cool_shift_white_balance() {
        let grey = [0.5; 3];
        let w = builtin("warm").unwrap().sample(grey);
        let c = builtin("cool").unwrap().sample(grey);
        assert!(w[0] > w[2] && c[2] > c[0]);
    }

    #[test]
    fn fade_lifts_black() {
        assert!(builtin("fade").unwrap().sample([0.0; 3])[0] > 0.05);
    }
}
