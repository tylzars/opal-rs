//! Settings kept across runs, stored as the `ctl` commands that set them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Commands that change a camera image setting, resent whenever streaming starts.
const CAMERA: &[&str] = &[
    "focus",
    "exposure",
    "ev",
    "wb",
    "brightness",
    "contrast",
    "saturation",
    "sharpness",
];

pub fn default_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("opal-rs")
        .join("settings")
}

/// The setting a command changes, if it is one that is kept.
fn group(command: &str) -> Option<&'static str> {
    Some(match command.split_whitespace().next()? {
        "focus" | "af" => "focus",
        "exposure" | "ae" => "exposure",
        "wb" | "awb" => "wb",
        "ev" => "ev",
        "brightness" => "brightness",
        "contrast" => "contrast",
        "saturation" => "saturation",
        "sharpness" => "sharpness",
        "gain" => "gain",
        "blur" => "blur",
        "lut" => "lut",
        _ => return None,
    })
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
        if command.split_whitespace().eq(["lut", "off"]) {
            self.commands.remove(g);
        } else {
            self.commands
                .insert(g, command.split_whitespace().collect::<Vec<_>>().join(" "));
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
        std::fs::write(&self.path, text)
    }

    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.commands.values().map(String::as_str)
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
}
