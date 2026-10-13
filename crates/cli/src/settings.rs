//! Settings kept across runs, stored as the `ctl` commands that set them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Commands that change a camera image setting, resent whenever streaming starts.
const CAMERA: &[&str] = &[
    "antibanding",
    "focus",
    "exposure",
    "ev",
    "wb",
    "brightness",
    "contrast",
    "saturation",
    "sharpness",
];

/// Settings a preset holds, with the command that puts each back to normal.
const PRESET: &[(&str, &str)] = &[
    ("ev", "ev 0"),
    ("wb", "awb"),
    ("brightness", "brightness 0"),
    ("contrast", "contrast 0"),
    ("saturation", "saturation 0"),
    ("sharpness", "sharpness 1"),
    ("blur", "blur 0"),
    ("bokeh", "bokeh soft"),
    ("background", "background off"),
    ("lut", "lut off"),
    ("zoom", "zoom off"),
    ("magnify", "magnify off"),
    ("pixelate", "pixelate off"),
    ("touchup", "touchup off"),
    ("tone", "tone off"),
    ("tracking", "tracking normal"),
    ("warmth", "warmth 0"),
    ("tint", "tint 0"),
    ("shadows", "shadows 0"),
    ("highlights", "highlights 0"),
    ("vibrance", "vibrance 0"),
];

/// Colour adjustment groups, cleared by `adjust reset`.
const ADJUSTMENTS: &[&str] = &["warmth", "tint", "shadows", "highlights", "vibrance"];

pub fn default_path() -> PathBuf {
    crate::platform::config_dir().join("settings")
}

/// Commands kept as settings, each standing for its own setting.
const KEPT: &[&str] = &[
    "focus",
    "exposure",
    "wb",
    "ev",
    "brightness",
    "contrast",
    "saturation",
    "sharpness",
    "adjust",
    "antibanding",
    "gain",
    "mic",
    "blur",
    "bokeh",
    "lut",
    "background",
    "zoom",
    "warmth",
    "tint",
    "shadows",
    "highlights",
    "vibrance",
    "magnify",
    "pixelate",
    "touchup",
    "tone",
    "tracking",
    "resolution",
    "gestures",
    "power",
    "lockpause",
];

/// The setting a command changes, if it is one that is kept.
fn group(command: &str) -> Option<&'static str> {
    let mut words = command.split_whitespace();
    let first = words.next()?;
    Some(match first {
        "af" => "focus",
        "ae" => "exposure",
        "awb" => "wb",
        "colour" => {
            let name = words.next()?;
            opal_effects::COLOURS.iter().find(|(n, _)| *n == name)?.0
        }
        "gesture" => match words.next()? {
            "peace" => "gesture peace",
            "pinch" => "gesture pinch",
            _ => return None,
        },
        _ => KEPT.iter().find(|k| **k == first)?,
    })
}

fn in_preset(group: &str) -> bool {
    PRESET.iter().any(|(g, _)| *g == group) || opal_effects::COLOURS.iter().any(|(n, _)| *n == group)
}

/// An adjustment set back to zero, which needs no line of its own.
fn is_neutral_adjustment(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    let zero = |w: &[&str]| w.iter().all(|v| v.parse::<f32>() == Ok(0.0));
    match words.as_slice() {
        [name, rest @ ..] if ADJUSTMENTS.contains(name) => rest.len() == 1 && zero(rest),
        ["colour", _, rest @ ..] => rest.len() == 3 && zero(rest),
        _ => false,
    }
}

/// The commands that load a preset: its own, then a reset for every preset setting it leaves out.
pub fn preset_commands(text: &str) -> Vec<String> {
    let own: Vec<(&str, &str)> = text
        .lines()
        .map(str::trim)
        .filter_map(|l| Some((group(l)?, l)))
        .filter(|(g, _)| in_preset(g))
        .collect();
    let mut out: Vec<String> = own.iter().map(|(_, c)| (*c).to_owned()).collect();
    let has = |g: &str| own.iter().any(|(o, _)| *o == g);
    out.extend(PRESET.iter().filter(|(g, _)| !has(g)).map(|(_, c)| (*c).to_owned()));
    out.extend(
        opal_effects::COLOURS
            .iter()
            .filter(|(n, _)| !has(n))
            .map(|(n, _)| format!("colour {n} 0 0 0")),
    );
    out
}

/// A preset name: letters, digits, dashes and underscores.
pub fn preset_name(name: &str) -> Option<&str> {
    (!name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .then_some(name)
}

pub struct Settings {
    path: PathBuf,
    commands: BTreeMap<&'static str, String>,
}

impl Settings {
    /// Reads `path`. A missing or unreadable file gives empty settings.
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let commands = text
            .lines()
            .map(str::trim)
            .filter_map(|l| Some((group(l)?, l.to_owned())))
            .collect();
        Self {
            path: path.to_owned(),
            commands,
        }
    }

    /// Remembers a command that succeeded and saves the file.
    pub fn record(&mut self, command: &str) -> std::io::Result<()> {
        let Some(g) = group(command) else {
            return Ok(());
        };
        if command.split_whitespace().eq(["lut", "off"])
            || command.split_whitespace().eq(["background", "off"])
            || is_neutral_adjustment(command)
        {
            self.commands.remove(g);
        } else if g == "adjust" {
            self.commands
                .retain(|k, _| !ADJUSTMENTS.contains(k) && !opal_effects::COLOURS.iter().any(|(n, _)| n == k));
        } else {
            self.commands
                // As typed: a file path may hold several spaces in a row.
                .insert(g, command.trim().to_owned());
        }
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text: String = self.commands.values().map(|c| format!("{c}\n")).collect();
        if text.is_empty() {
            text.push('\n');
        }
        crate::platform::write_file(&self.path, text)
    }

    /// The saved video resolution.
    pub fn resolution(&self) -> opal_c1::Resolution {
        self.commands
            .get("resolution")
            .and_then(|c| c.split_whitespace().nth(1))
            .and_then(opal_c1::Resolution::parse)
            .unwrap_or_default()
    }

    /// The saved anti-banding setting: off, 50, 60 or auto.
    pub fn antibanding(&self) -> &str {
        self.commands
            .get("antibanding")
            .and_then(|c| c.split_whitespace().nth(1))
            .unwrap_or("auto")
    }

    /// True when low power mode is saved.
    /// Low power mode is Linux only: on Windows neither the firmware loaded here nor the camera's own serves the
    /// camera's USB video reliably, so a saved choice from an older version is ignored there.
    pub fn low_power(&self) -> bool {
        cfg!(unix) && self.commands.get("power").is_some_and(|c| c == "power low")
    }

    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.commands.values().map(String::as_str)
    }

    /// Where presets are kept: a `presets` folder next to the settings file.
    pub fn preset_dir(&self) -> PathBuf {
        self.path.with_file_name("presets")
    }

    /// The stickers file, next to the settings file.
    pub fn stickers_path(&self) -> PathBuf {
        self.path.with_file_name("stickers")
    }

    /// The current settings that belong in a preset, one command per line.
    pub fn preset_text(&self) -> String {
        self.commands
            .iter()
            .filter(|(g, _)| in_preset(g))
            .map(|(_, c)| format!("{c}\n"))
            .collect()
    }

    pub fn camera_commands(&self) -> impl Iterator<Item = &str> {
        self.commands
            .iter()
            .filter(|(g, _)| CAMERA.contains(g))
            .map(|(_, c)| c.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_command_per_setting() {
        let path = std::env::temp_dir().join(format!("opal-rs-settings-test-{}", std::process::id()));
        let mut s = Settings::load(&path);
        s.record("focus  120").unwrap();
        s.record("af").unwrap();
        s.record("blur 24").unwrap();
        s.record("lut /tmp/a.cube 0.5").unwrap();
        s.record("status").unwrap();
        s.record("lut off").unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.commands().collect::<Vec<_>>(), ["blur 24", "af"]);
        assert_eq!(s.camera_commands().collect::<Vec<_>>(), ["af"]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn presets_hold_looks_and_reset_the_rest() {
        let path = std::env::temp_dir().join(format!("opal-rs-settings-preset-{}", std::process::id()));
        let mut s = Settings::load(&path);
        s.record("blur 24").unwrap();
        s.record("focus 120").unwrap();
        s.record("colour blue 0 -50 0").unwrap();
        s.record("resolution 1440p").unwrap();
        let text = s.preset_text();
        assert_eq!(text, "colour blue 0 -50 0\nblur 24\n");
        let cmds = preset_commands(&text);
        assert_eq!(&cmds[..2], ["colour blue 0 -50 0", "blur 24"]);
        assert!(cmds.contains(&"lut off".to_owned()));
        assert!(cmds.contains(&"colour red 0 0 0".to_owned()));
        assert!(!cmds.contains(&"blur 0".to_owned()));
        assert!(!cmds.contains(&"colour blue 0 0 0".to_owned()));
        assert!(
            !cmds
                .iter()
                .any(|c| c.starts_with("focus") || c.starts_with("resolution"))
        );
        assert_eq!(preset_name("Meeting_1"), Some("Meeting_1"));
        assert_eq!(preset_name("../x"), None);
        assert_eq!(preset_name(""), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn adjust_reset_clears_adjustments() {
        let path = std::env::temp_dir().join(format!("opal-rs-settings-adjust-{}", std::process::id()));
        let mut s = Settings::load(&path);
        s.record("warmth 20").unwrap();
        s.record("colour blue 0 -50 0").unwrap();
        s.record("colour red 10 0 0").unwrap();
        s.record("colour red 30 0 0").unwrap();
        s.record("blur 24").unwrap();
        assert_eq!(s.commands().count(), 4);
        assert!(s.commands().any(|c| c == "colour red 30 0 0"));
        s.record("colour red 0 0 0").unwrap();
        s.record("warmth 0").unwrap();
        assert_eq!(s.commands().collect::<Vec<_>>(), ["colour blue 0 -50 0", "blur 24"]);
        s.record("adjust reset").unwrap();
        assert_eq!(s.commands().collect::<Vec<_>>(), ["blur 24"]);
        std::fs::remove_file(&path).unwrap();
    }
}
