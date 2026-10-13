//! Faces from the camera's face detector.

use opal_dai::message::NnData;

/// Detections below this confidence are ignored.
pub const MIN_CONFIDENCE: f32 = 0.5;

/// A face box, normalised to the frame (0 to 1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Face {
    pub confidence: f32,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Face {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn center(&self) -> (f32, f32) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    fn area(&self) -> f32 {
        self.width() * self.height()
    }

    fn distance(&self, other: &Face) -> f32 {
        [
            self.x0 - other.x0,
            self.y0 - other.y0,
            self.x1 - other.x1,
            self.y1 - other.y1,
        ]
        .iter()
        .map(|d| d * d)
        .sum::<f32>()
        .sqrt()
    }
}

/// Faces in a detector output: rows of [image, label, confidence, x0, y0, x1, y1], ended by image -1.
pub fn decode(nn: &NnData, data: &[u8]) -> Vec<Face> {
    let Some(values) = nn.tensor_f32(data, "detection_out") else {
        return vec![];
    };
    values
        .as_chunks::<7>()
        .0
        .iter()
        .take_while(|d| d[0] >= 0.0)
        .filter(|d| d[2] >= MIN_CONFIDENCE)
        .map(|d| Face {
            confidence: d[2],
            x0: d[3].clamp(0.0, 1.0),
            y0: d[4].clamp(0.0, 1.0),
            x1: d[5].clamp(0.0, 1.0),
            y1: d[6].clamp(0.0, 1.0),
        })
        .filter(|f| f.width() > 0.0 && f.height() > 0.0)
        .collect()
}

/// Follows one face: first the biggest, then each time the detection closest to the face followed so far.
#[derive(Default)]
pub struct Tracker {
    last: Option<Face>,
}

impl Tracker {
    pub fn update(&mut self, faces: &[Face]) -> Option<Face> {
        let face = match self.last {
            Some(from) => faces
                .iter()
                .min_by(|a, b| a.distance(&from).total_cmp(&b.distance(&from))),
            None => faces.iter().max_by(|a, b| a.area().total_cmp(&b.area())),
        }
        .copied()?;
        self.last = Some(face);
        Some(face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x0: f32, y0: f32, x1: f32, y1: f32) -> Face {
        Face {
            confidence: 0.9,
            x0,
            y0,
            x1,
            y1,
        }
    }

    #[test]
    fn follows_the_nearest_face() {
        let mut t = Tracker::default();
        assert_eq!(t.update(&[]), None);
        let a = face(0.1, 0.1, 0.2, 0.2);
        let b = face(0.6, 0.6, 0.8, 0.8);
        assert_eq!(t.update(&[a, b]), Some(b));
        assert_eq!(t.update(&[a, face(0.62, 0.6, 0.82, 0.8)]).unwrap().x0, 0.62);
    }
}
