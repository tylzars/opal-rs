//! Colour adjustments, baked into a 3D table together with a look.

use crate::looks::{luma, saturate};
use crate::lut::Cube;

const SIZE: usize = 33;
/// Red and blue gain at full temperature, and green cut at full tint.
const TEMPERATURE: f32 = 0.25;
const TINT: f32 = 0.15;

/// Colours that can be adjusted on their own, with their hue in degrees.
pub const COLOURS: [(&str, f32); 8] = [
    ("red", 0.0),
    ("orange", 30.0),
    ("yellow", 60.0),
    ("green", 120.0),
    ("aqua", 180.0),
    ("blue", 240.0),
    ("purple", 270.0),
    ("magenta", 300.0),
];

/// Hue, saturation and lightness change for one colour, each -1 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hsl {
    pub hue: f32,
    pub saturation: f32,
    pub lightness: f32,
}

/// All adjustments, each -1 to 1 with 0 leaving the picture alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Adjust {
    /// Warmer (more orange) above 0, cooler (more blue) below.
    pub temperature: f32,
    /// More magenta above 0, more green below.
    pub tint: f32,
    pub shadows: f32,
    pub highlights: f32,
    /// Saturation that favours dull colours and spares skin.
    pub vibrance: f32,
    pub colours: [Hsl; 8],
}

fn hue_of([r, g, b]: [f32; 3]) -> Option<f32> {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    if d < 1e-5 {
        return None;
    }
    let h = if max == r {
        (g - b) / d
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    Some((h * 60.0).rem_euclid(360.0))
}

/// Turns `c` around the grey axis by `degrees`, keeping its luma.
fn rotate_hue(c: [f32; 3], degrees: f32) -> [f32; 3] {
    let y = luma(c);
    let (cos, sin) = (degrees.to_radians().cos(), degrees.to_radians().sin());
    // Rotation about (1, 1, 1) in RGB.
    let k = (1.0 - cos) / 3.0;
    let s = sin / 3f32.sqrt();
    let m = [
        [cos + k, k - s, k + s],
        [k + s, cos + k, k - s],
        [k - s, k + s, cos + k],
    ];
    let out = [0, 1, 2].map(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2]);
    let shift = y - luma(out);
    out.map(|v| v + shift)
}

/// How much a hue belongs to the colour centred on `centre`: 1 at the centre, 0 at the neighbouring colours.
fn weight(hue: f32, i: usize) -> f32 {
    let n = COLOURS.len();
    let centre = COLOURS[i].1;
    let next = COLOURS[(i + 1) % n].1 + if i + 1 == n { 360.0 } else { 0.0 };
    let prev = COLOURS[(i + n - 1) % n].1 - if i == 0 { 360.0 } else { 0.0 };
    let mut h = hue;
    if h - centre > 180.0 {
        h -= 360.0;
    } else if centre - h > 180.0 {
        h += 360.0;
    }
    if h >= centre {
        (1.0 - (h - centre) / (next - centre)).max(0.0)
    } else {
        (1.0 - (centre - h) / (centre - prev)).max(0.0)
    }
}

impl Adjust {
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    /// Applies every adjustment to a linear-ish RGB colour in 0..1.
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let mut c = c;
        if self.temperature != 0.0 || self.tint != 0.0 {
            let (t, g) = (self.temperature * TEMPERATURE, self.tint * TINT);
            c = [
                c[0] * (1.0 + t) * (1.0 + g / 2.0),
                c[1] * (1.0 - g),
                c[2] * (1.0 - t) * (1.0 + g / 2.0),
            ];
        }
        if self.shadows != 0.0 || self.highlights != 0.0 {
            // Bumps that peak in the dark and light tones and leave black and white where they are.
            let y = luma(c).clamp(0.0, 1.0);
            let d = self.shadows * 1.2 * y * (1.0 - y).powi(2) + self.highlights * 1.2 * y * y * (1.0 - y);
            c = c.map(|v| v + d);
        }
        if self.colours.iter().any(|h| *h != Hsl::default())
            && let Some(hue) = hue_of(c)
        {
            let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
            // Greys have no colour to adjust.
            let chroma = (max - min).clamp(0.0, 1.0);
            let mut shift = Hsl::default();
            for (i, h) in self.colours.iter().enumerate() {
                let w = weight(hue, i);
                shift.hue += h.hue * w;
                shift.saturation += h.saturation * w;
                shift.lightness += h.lightness * w;
            }
            c = rotate_hue(c, shift.hue * 30.0);
            c = saturate(c, 1.0 + shift.saturation);
            c = c.map(|v| v + shift.lightness * 0.25 * chroma);
        }
        if self.vibrance != 0.0 {
            let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
            let sat = if max > 1e-5 { (max - min) / max } else { 0.0 };
            // Skin sits around orange; boosts there are kept small.
            let skin = hue_of(c).map_or(0.0, |h| (1.0 - (h - 25.0).abs() / 25.0).max(0.0));
            let boost = if self.vibrance > 0.0 {
                self.vibrance * (1.0 - sat) * (1.0 - 0.7 * skin)
            } else {
                // Some colour is kept even at the bottom of the range.
                self.vibrance * 0.7
            };
            c = saturate(c, 1.0 + boost);
        }
        c
    }

    /// One table with the adjustments followed by `look` at `amount`.
    pub fn cube(&self, look: Option<(&Cube, f32)>) -> Cube {
        let step = 1.0 / (SIZE - 1) as f32;
        let data = (0..SIZE * SIZE * SIZE)
            .map(|i| {
                let rgb = [i % SIZE, i / SIZE % SIZE, i / (SIZE * SIZE)].map(|v| v as f32 * step);
                let a = self.apply(rgb).map(|v| v.clamp(0.0, 1.0));
                match look {
                    Some((cube, amount)) => {
                        let l = cube.sample(a);
                        [0, 1, 2].map(|c| a[c] + (l[c] - a[c]) * amount)
                    }
                    None => a,
                }
                .map(|v| v.clamp(0.0, 1.0))
            })
            .collect();
        Cube {
            size: SIZE,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            data,
        }
    }

    /// Sets the named adjustment from a value of -100 to 100. Colours are `<colour> <hue> <saturation> <lightness>`.
    pub fn set(&mut self, name: &str, values: &[f32]) -> Option<()> {
        let colour = COLOURS.iter().position(|(n, _)| *n == name);
        if values.len() != if colour.is_some() { 3 } else { 1 } || values.iter().any(|v| !(-100.0..=100.0).contains(v))
        {
            return None;
        }
        let v = |i: usize| values.get(i).map(|v| (v / 100.0).clamp(-1.0, 1.0));
        match name {
            "temperature" => self.temperature = v(0)?,
            "tint" => self.tint = v(0)?,
            "shadows" => self.shadows = v(0)?,
            "highlights" => self.highlights = v(0)?,
            "vibrance" => self.vibrance = v(0)?,
            _ => {
                self.colours[colour?] = Hsl {
                    hue: v(0)?,
                    saturation: v(1)?,
                    lightness: v(2)?,
                };
            }
        }
        Some(())
    }
}

/// Temperature and tint, each -1 to 1, that make the colour `rgb` (0 to 255, as shown with the current
/// `temperature` and `tint`) come out grey.
pub fn neutralise(rgb: [f32; 3], temperature: f32, tint: f32) -> (f32, f32) {
    let [r, g, b] = rgb.map(|v| v.max(1.0));
    // Undo the current setting, so the new one is found from the camera's own colour.
    let (t0, g0) = (temperature * TEMPERATURE, tint * TINT);
    let (r, g, b) = (
        r / ((1.0 + t0) * (1.0 + g0 / 2.0)),
        g / (1.0 - g0),
        b / ((1.0 - t0) * (1.0 + g0 / 2.0)),
    );
    // Red and blue level out when (1 + t) / (1 - t) = b / r.
    let t = (b / r - 1.0) / (b / r + 1.0);
    let m = (r * (1.0 + t) + b * (1.0 - t)) / 2.0;
    // Green meets them when g (1 - k) = m (1 + k / 2).
    let k = (g - m) / (g + m / 2.0);
    ((t / TEMPERATURE).clamp(-1.0, 1.0), (k / TINT).clamp(-1.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn neutral_changes_nothing() {
        let a = Adjust::default();
        for c in [[0.2, 0.5, 0.9], [1.0, 1.0, 1.0], [0.0, 0.0, 0.0], [0.7, 0.3, 0.3]] {
            assert!(close(a.apply(c), c));
        }
        assert!(a.is_neutral());
    }

    #[test]
    fn temperature_and_tint() {
        let grey = [0.5; 3];
        let mut a = Adjust {
            temperature: 1.0,
            ..Default::default()
        };
        let w = a.apply(grey);
        assert!(w[0] > w[2]);
        a.temperature = -1.0;
        let c = a.apply(grey);
        assert!(c[2] > c[0]);
        a.temperature = 0.0;
        a.tint = 1.0;
        let m = a.apply(grey);
        assert!(m[1] < m[0] && m[1] < m[2]);
    }

    #[test]
    fn shadows_and_highlights_keep_black_and_white() {
        let a = Adjust {
            shadows: 1.0,
            highlights: -1.0,
            ..Default::default()
        };
        assert!(close(a.apply([0.0; 3]), [0.0; 3]));
        assert!(close(a.apply([1.0; 3]), [1.0; 3]));
        assert!(a.apply([0.25; 3])[0] > 0.3);
        assert!(a.apply([0.75; 3])[0] < 0.7);
    }

    #[test]
    fn colour_weights_meet_at_neighbours() {
        assert_eq!(weight(0.0, 0), 1.0);
        assert_eq!(weight(30.0, 0), 0.0);
        assert!((weight(15.0, 0) - 0.5).abs() < 1e-5);
        assert!((weight(345.0, 0) - 0.75).abs() < 1e-5, "magenta is 60 degrees from red");
        for h in (0..360).step_by(7) {
            let total: f32 = (0..8).map(|i| weight(h as f32, i)).sum();
            assert!((total - 1.0).abs() < 1e-4, "hue {h}: {total}");
        }
    }

    #[test]
    fn colour_adjustment_only_touches_its_colour() {
        let mut a = Adjust::default();
        a.set("blue", &[0.0, -100.0, 0.0]).unwrap();
        let blue = a.apply([0.1, 0.2, 0.9]);
        assert!(blue[2] - blue[0] < 0.2, "blue desaturated: {blue:?}");
        let red = [0.9, 0.1, 0.1];
        assert!(close(a.apply(red), red));
        let grey = [0.4; 3];
        assert!(close(a.apply(grey), grey));
    }

    #[test]
    fn vibrance_spares_skin_and_saturated_colours() {
        let a = Adjust {
            vibrance: 1.0,
            ..Default::default()
        };
        let gain = |c: [f32; 3]| {
            let o = a.apply(c);
            (o[0] - o[2]).abs() / (c[0] - c[2]).abs()
        };
        let dull_blue = [0.4, 0.45, 0.55];
        let skin = [0.8, 0.6, 0.5];
        assert!(gain(dull_blue) > gain(skin));
        assert!(gain([0.9, 0.1, 0.05]) < gain(dull_blue));
    }

    #[test]
    fn neutralise_makes_the_sample_grey() {
        for sample in [[200.0, 180.0, 150.0], [120.0, 130.0, 160.0], [150.0, 170.0, 150.0]] {
            let (t, k) = neutralise(sample, 0.0, 0.0);
            let a = Adjust {
                temperature: t,
                tint: k,
                ..Default::default()
            };
            let out = a.apply(sample.map(|v| v / 255.0));
            assert!(
                (out[0] - out[2]).abs() < 0.01 && (out[1] - out[0]).abs() < 0.01,
                "{sample:?} -> {out:?}"
            );
        }
        // Picking from a picture that already has a setting gives the same answer.
        let base = Adjust {
            temperature: 0.3,
            tint: -0.2,
            ..Default::default()
        };
        let raw = [200.0, 180.0, 150.0];
        let shown = base.apply(raw.map(|v| v / 255.0)).map(|v| v * 255.0);
        let (t1, k1) = neutralise(raw, 0.0, 0.0);
        let (t2, k2) = neutralise(shown, 0.3, -0.2);
        assert!((t1 - t2).abs() < 1e-3 && (k1 - k2).abs() < 1e-3);
    }

    #[test]
    fn set_parses_commands() {
        let mut a = Adjust::default();
        assert!(a.set("temperature", &[50.0]).is_some());
        assert_eq!(a.temperature, 0.5);
        assert!(a.set("green", &[10.0, 20.0, -30.0]).is_some());
        assert_eq!(a.colours[3].lightness, -0.3);
        assert!(a.set("green", &[10.0]).is_none());
        assert!(a.set("nope", &[1.0]).is_none());
        assert!(a.set("tint", &[101.0]).is_none());
    }
}
