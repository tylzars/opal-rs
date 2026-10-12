//! Stickers over the video: text, name cards, a clock, a timer and pictures, kept in a file next to the settings.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use opal_effects::{Font, Mask, Rgba, composite};

/// Fonts tried in order for text stickers.
const FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/ubuntu/Ubuntu-M.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
];

const COLOURS: &[(&str, [u8; 3])] = &[
    ("white", [255, 255, 255]),
    ("black", [0, 0, 0]),
    ("red", [235, 64, 52]),
    ("orange", [255, 150, 40]),
    ("yellow", [255, 220, 90]),
    ("green", [80, 200, 120]),
    ("blue", [80, 140, 255]),
    ("purple", [170, 110, 240]),
    ("pink", [255, 120, 180]),
    ("grey", [160, 160, 160]),
];

/// A colour name from the list, or `#rrggbb`.
pub fn colour(name: &str) -> Option<[u8; 3]> {
    if let Some(hex) = name.strip_prefix('#')
        && hex.len() == 6
    {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some([(v >> 16) as u8, (v >> 8) as u8, v as u8]);
    }
    COLOURS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

fn colour_name(c: [u8; 3]) -> String {
    COLOURS.iter().find(|(_, v)| *v == c).map_or_else(
        || format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]),
        |(n, _)| (*n).to_owned(),
    )
}

pub enum Kind {
    Text { colour: [u8; 3], text: String },
    Card { name: String, title: String },
    Clock { colour: [u8; 3] },
    Timer { colour: [u8; 3], start: Instant },
    Picture { path: String, image: Arc<Rgba> },
}

pub struct Sticker {
    pub id: u32,
    pub kind: Kind,
    /// Centre, as a fraction of the frame; around the face while `follow` is on (0.5, 0.5 is on the face).
    pub x: f32,
    pub y: f32,
    /// Height as a fraction of the frame height.
    pub size: f32,
    /// Hidden where the person is, so it sits behind them.
    pub behind: bool,
    pub follow: bool,
    /// Last drawn content and height, and its image.
    cache: Option<(String, usize, Arc<Rgba>)>,
}

fn flags(behind: bool, follow: bool) -> &'static str {
    match (behind, follow) {
        (false, false) => "-",
        (true, false) => "b",
        (false, true) => "f",
        (true, true) => "bf",
    }
}

/// Sticker text kept short and free of `;`, which separates stickers in `sticker list`.
fn clean_text(t: &str) -> String {
    t.replace(';', ",").trim().chars().take(MAX_TEXT).collect()
}

const MAX_TEXT: usize = 120;

fn number(w: &str, lo: f32, hi: f32) -> Result<f32> {
    let v: f32 = w.parse().with_context(|| format!("bad number {w}"))?;
    anyhow::ensure!((lo..=hi).contains(&v), "{w} is outside {lo} to {hi}");
    Ok(v)
}

impl Sticker {
    /// From `<kind> <x> <y> <size> <flags> <details...>`, the words after `sticker add`.
    pub fn parse(id: u32, words: &[&str]) -> Result<Self> {
        let [kind, x, y, size, flag, rest @ ..] = words else {
            bail!("usage: sticker add <text|card|clock|timer|picture> <x> <y> <size> <-|b|f|bf> ...");
        };
        let colour_of = |w: Option<&&str>| -> Result<[u8; 3]> {
            w.map_or(Ok([255, 255, 255]), |w| {
                colour(w).ok_or_else(|| anyhow!("unknown colour {w}"))
            })
        };
        let kind = match *kind {
            "text" => {
                let (c, text) = rest
                    .split_first()
                    .ok_or_else(|| anyhow!("text needs a colour and words"))?;
                anyhow::ensure!(!text.is_empty(), "text needs words");
                Kind::Text {
                    colour: colour_of(Some(c))?,
                    text: clean_text(&text.join(" ")),
                }
            }
            "card" => {
                let line = rest.join(" ");
                let (name, title) = line.split_once('|').unwrap_or((&line, ""));
                anyhow::ensure!(!name.trim().is_empty(), "card needs a name");
                Kind::Card {
                    name: clean_text(name),
                    title: clean_text(title),
                }
            }
            "clock" => Kind::Clock {
                colour: colour_of(rest.first())?,
            },
            "timer" => Kind::Timer {
                colour: colour_of(rest.first())?,
                start: Instant::now(),
            },
            "picture" => {
                let path = rest.join(" ");
                let img = image::ImageReader::open(Path::new(&path))
                    .and_then(|r| r.with_guessed_format())
                    .with_context(|| path.clone())?
                    .decode()
                    .with_context(|| path.clone())?
                    .to_rgba8();
                let image = Arc::new(Rgba {
                    width: img.width() as usize,
                    height: img.height() as usize,
                    data: img.into_raw(),
                });
                Kind::Picture { path, image }
            }
            k => bail!("unknown sticker kind {k}"),
        };
        Ok(Self {
            id,
            kind,
            x: number(x, -1.0, 2.0)?,
            y: number(y, -1.0, 2.0)?,
            size: number(size, 0.01, 1.0)?,
            behind: flag.contains('b'),
            follow: flag.contains('f'),
            cache: None,
        })
    }

    /// The words that make this sticker again with `sticker add`.
    pub fn line(&self) -> String {
        let head = |k: &str| {
            format!(
                "{k} {:.3} {:.3} {:.3} {}",
                self.x,
                self.y,
                self.size,
                flags(self.behind, self.follow)
            )
        };
        match &self.kind {
            Kind::Text { colour, text } => format!("{} {} {text}", head("text"), colour_name(*colour)),
            Kind::Card { name, title } => format!("{} {name} | {title}", head("card")),
            Kind::Clock { colour } => format!("{} {}", head("clock"), colour_name(*colour)),
            Kind::Timer { colour, .. } => format!("{} {}", head("timer"), colour_name(*colour)),
            Kind::Picture { path, .. } => format!("{} {path}", head("picture")),
        }
    }

    /// What to draw right now: the text for text stickers, the time for clocks and timers.
    fn content(&self) -> String {
        match &self.kind {
            Kind::Text { text, .. } => text.clone(),
            Kind::Card { name, title } => format!("{name}|{title}"),
            Kind::Clock { .. } => crate::platform::local_time(),
            Kind::Timer { start, .. } => {
                let s = start.elapsed().as_secs();
                format!("{:02}:{:02}", s / 60, s % 60)
            }
            Kind::Picture { path, .. } => path.clone(),
        }
    }

    fn image(&mut self, font: Option<&Font>, height: usize, max_width: usize) -> Option<Arc<Rgba>> {
        let content = self.content();
        // Long text at a big size would make a huge image; such text is drawn smaller to fit.
        let chars = content.chars().count().max(1);
        let height = match self.kind {
            Kind::Picture { .. } => height,
            _ => height.min(max_width * 2 / chars).max(4),
        };
        if let Some((c, h, img)) = &self.cache
            && *c == content
            && *h == height
        {
            return Some(img.clone());
        }
        let img = match &self.kind {
            Kind::Text { colour, .. } | Kind::Clock { colour } | Kind::Timer { colour, .. } => {
                font?.text(&content, height, *colour)
            }
            Kind::Card { name, title } => font?.card(name, title, height),
            Kind::Picture { image, .. } => opal_effects::scale(image, height),
        };
        let img = Arc::new(img);
        self.cache = Some((content, height, img.clone()));
        Some(img)
    }
}

#[derive(Default)]
pub struct Stickers {
    pub list: Vec<Sticker>,
    next_id: u32,
    font: Option<Font>,
}

/// Where the frame is cropped from, and where the face is, for placing stickers.
pub struct Scene<'a> {
    /// The crop of the camera picture that fills the frame, in pixels.
    pub crop: opal_effects::Rect,
    /// Centre of the face in the camera picture, 0 to 1.
    pub face: Option<(f32, f32)>,
    pub mask: Option<&'a Mask>,
}

impl Stickers {
    pub fn new() -> Self {
        Self {
            font: FONTS.iter().find_map(|p| Font::load(&std::fs::read(p).ok()?)),
            ..Default::default()
        }
    }

    pub fn add(&mut self, words: &[&str]) -> Result<u32> {
        self.next_id += 1;
        let s = Sticker::parse(self.next_id, words)?;
        anyhow::ensure!(
            self.font.is_some() || matches!(s.kind, Kind::Picture { .. }),
            "no font found for text stickers"
        );
        self.list.push(s);
        Ok(self.next_id)
    }

    pub fn get(&mut self, id: &str) -> Result<&mut Sticker> {
        let id: u32 = id.parse().with_context(|| format!("bad sticker id {id}"))?;
        self.list
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow!("no sticker {id}"))
    }

    /// `id kind x y size flags details` for each sticker, separated by `; `.
    pub fn describe(&self) -> String {
        self.list
            .iter()
            .map(|s| format!("{} {}", s.id, s.line()))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Sticker lines for the stickers file.
    pub fn text(&self) -> String {
        self.list.iter().map(|s| format!("{}\n", s.line())).collect()
    }

    /// Replaces the stickers with those in a stickers file's text. Lines that fail are reported and skipped.
    pub fn load(&mut self, text: &str) {
        self.list.clear();
        self.next_id = 0;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let words: Vec<&str> = line.split_whitespace().collect();
            if let Err(e) = self.add(&words) {
                eprintln!("sticker {line}: {e}");
            }
        }
    }

    /// (behind, follow): whether any sticker needs the person mask or the face.
    pub fn needs(&self) -> (bool, bool) {
        (self.list.iter().any(|s| s.behind), self.list.iter().any(|s| s.follow))
    }

    pub fn draw(&mut self, frame: &mut [u8], w: usize, h: usize, scene: &Scene) {
        let font = self.font.as_ref();
        let c = scene.crop;
        for s in &mut self.list {
            let height = ((s.size * h as f32) as usize).max(4);
            let Some(img) = s.image(font, height, 2 * w) else {
                continue;
            };
            let (mut cx, mut cy) = (s.x, s.y);
            if s.follow
                && let Some((fx, fy)) = scene.face
            {
                // The face in frame fractions, after the crop.
                let (ox, oy) = ((fx * w as f32 - c.x) / c.width, (fy * h as f32 - c.y) / c.height);
                (cx, cy) = (ox + s.x - 0.5, oy + s.y - 0.5);
            }
            let x0 = (cx * w as f32 - img.width as f32 / 2.0) as i64;
            let y0 = (cy * h as f32 - img.height as f32 / 2.0) as i64;
            let hide = |px: usize, py: usize| -> f32 {
                let Some(m) = scene.mask else { return 0.0 };
                let ox = (c.x + px as f32 * c.width / w as f32) / w as f32;
                let oy = (c.y + py as f32 * c.height / h as f32) / h as f32;
                let mx = ((ox * m.width as f32) as usize).min(m.width - 1);
                let my = ((oy * m.height as f32) as usize).min(m.height - 1);
                ((m.data[my * m.width + mx] - 0.3) / 0.4).clamp(0.0, 1.0)
            };
            let hide: Option<&dyn Fn(usize, usize) -> f32> = (s.behind && scene.mask.is_some()).then_some(&hide);
            composite(frame, w, h, &img, x0, y0, hide);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_writes_back() {
        let mut st = Stickers::new();
        if st.font.is_none() {
            return;
        }
        let id = st
            .add(&"text 0.5 0.8 0.08 b yellow Back in 5".split(' ').collect::<Vec<_>>())
            .unwrap();
        st.add(
            &"card 0.25 0.85 0.14 - Ada Lovelace | Engineer"
                .split(' ')
                .collect::<Vec<_>>(),
        )
        .unwrap();
        st.add(&"clock 0.9 0.06 0.06 f #102030".split(' ').collect::<Vec<_>>())
            .unwrap();
        assert_eq!(
            st.text(),
            "text 0.500 0.800 0.080 b yellow Back in 5\ncard 0.250 0.850 0.140 - Ada Lovelace | Engineer\n\
             clock 0.900 0.060 0.060 f #102030\n"
        );
        assert_eq!(st.needs(), (true, true));
        st.get(&id.to_string()).unwrap().behind = false;
        let again = st.text();
        let mut other = Stickers::new();
        other.load(&again);
        assert_eq!(other.text(), again);
        other.load(&again);
        assert_eq!(other.list.iter().map(|s| s.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(st.add(&["text", "0.5", "0.5", "0.1", "-", "mauve", "hi"]).is_err());
        assert!(st.add(&["clock", "0.5", "0.5", "3", "-"]).is_err());
        assert!(st.get("99").is_err());
    }

    #[test]
    fn colours() {
        assert_eq!(colour("#ff8000"), Some([255, 128, 0]));
        assert_eq!(colour("white"), Some([255, 255, 255]));
        assert_eq!(colour("#ff80"), None);
        assert_eq!(colour_name([1, 2, 3]), "#010203");
    }

    #[test]
    fn draws_into_the_frame() {
        let mut st = Stickers::new();
        if st.font.is_none() {
            return;
        }
        st.add(&"text 0.5 0.5 0.3 - white HI".split(' ').collect::<Vec<_>>())
            .unwrap();
        let (w, h) = (160, 90);
        let mut f = vec![0u8; w * h * 3 / 2];
        let scene = Scene {
            crop: opal_effects::Rect::full(w, h),
            face: None,
            mask: None,
        };
        st.draw(&mut f, w, h, &scene);
        assert!(f[..w * h].iter().any(|&v| v > 200));
    }
}
