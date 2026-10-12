//! Virtual cameras (Linux): "Opal C1 Virtual" plus extra ones the user adds and names. Linux lets one app use a
//! camera at a time, so each app that should see the Opal C1 at the same time needs a camera of its own.
//!
//! They are v4l2loopback devices made from its module options. Extra ones go in an override of the package's
//! options file, which only an administrator can write; the change applies when the module is next loaded, at once
//! if no camera is open.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

/// Written by `add` and `remove`; overrides the package's file of the same name.
pub const CONF: &str = "/etc/modprobe.d/opal-rs-v4l2loopback.conf";
/// The package's options: the main camera only.
const PACKAGED: &str = "/usr/lib/modprobe.d/opal-rs-v4l2loopback.conf";
/// A source install's options (INSTALL.md).
const SOURCE_INSTALL: &str = "/etc/modprobe.d/v4l2loopback.conf";
/// First line of `CONF`, so it is known to be ours.
pub const MARK: &str = "# Written by opal-rs: the Opal C1 virtual cameras.";
/// Device numbers for extra cameras; each keeps its number so apps find it again.
const EXTRA: std::ops::RangeInclusive<u32> = 11..=14;
/// Longest name v4l2loopback keeps, less its terminating zero.
const NAME_MAX: usize = 31;

#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    pub nr: u32,
    pub name: String,
}

impl Camera {
    pub fn path(&self) -> PathBuf {
        PathBuf::from(format!("/dev/video{}", self.nr))
    }

    /// True once the device exists with this name (it may still have the old one until the module reloads).
    pub fn present(&self) -> bool {
        std::fs::read_to_string(format!("/sys/class/video4linux/video{}/name", self.nr))
            .is_ok_and(|n| n.trim() == self.name)
    }
}

fn main_camera() -> Camera {
    Camera {
        nr: 10,
        name: "Opal C1 Virtual".into(),
    }
}

/// The cameras in a modprobe options file, the main one first.
fn parse(text: &str) -> Option<Vec<Camera>> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.split_whitespace().take(2).eq(["options", "v4l2loopback"]))?;
    let (mut numbers, mut names) = (Vec::new(), Vec::new());
    for (key, value) in options(line) {
        match key.as_str() {
            "video_nr" => numbers = value.split(',').map(|n| n.trim().parse::<u32>().ok()).collect(),
            "card_label" => names = value.split(',').map(str::to_owned).collect(),
            _ => {}
        }
    }
    let mut cameras: Vec<Camera> = numbers
        .into_iter()
        .zip(names)
        .map_while(|(nr, name)| Some(Camera { nr: nr?, name }))
        .collect();
    // The main camera first even in a file edited by hand; the rest by number.
    cameras.sort_by_key(|c| (c.nr != main_camera().nr, c.nr));
    (!cameras.is_empty()).then_some(cameras)
}

/// `key=value` pairs after "options v4l2loopback"; a value may be quoted to hold spaces.
fn options(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    let mut words = 0;
    while chars.peek().is_some() {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut word = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c == '"' {
                quoted = !quoted;
            } else if c.is_whitespace() && !quoted {
                break;
            } else {
                word.push(c);
            }
            chars.next();
        }
        words += 1;
        if words > 2
            && let Some((k, v)) = word.split_once('=')
        {
            out.push((k.to_owned(), v.to_owned()));
        }
    }
    out
}

/// The options file for `cameras`, the main one first.
fn render(cameras: &[Camera]) -> String {
    let join = |f: &dyn Fn(&Camera) -> String| cameras.iter().map(f).collect::<Vec<_>>().join(",");
    format!(
        "{MARK}\noptions v4l2loopback devices={} video_nr={} card_label=\"{}\" exclusive_caps={}\n",
        cameras.len(),
        join(&|c| c.nr.to_string()),
        join(&|c| c.name.clone()),
        join(&|_| "1".into()),
    )
}

/// The cameras as set up now: the override, else the package's or a source install's options.
pub fn configured() -> Vec<Camera> {
    [CONF, PACKAGED, SOURCE_INSTALL]
        .iter()
        .find_map(|p| parse(&std::fs::read_to_string(p).ok()?))
        .unwrap_or_else(|| vec![main_camera()])
}

/// Checks a new camera's name: plain letters and digits with a few marks, so it survives the module options and
/// the control lines, and different from the others.
pub fn check_name(name: &str, cameras: &[Camera]) -> Result<()> {
    if name.trim() != name || name.is_empty() {
        bail!("give the camera a name, without spaces at either end");
    }
    if name.len() > NAME_MAX {
        bail!("camera names are at most {NAME_MAX} characters");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || " -_.()".contains(c)) {
        bail!("camera names use letters, digits, spaces and - _ . ( )");
    }
    if cameras.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
        bail!("there is already a camera called {name}");
    }
    Ok(())
}

/// What `add` or `remove` did.
pub enum Applied {
    Now,
    /// A camera was open, so the module stays as it is until the computer restarts.
    AfterRestart,
}

/// Adds a camera called `name` (as administrator).
pub fn add(name: &str) -> Result<Applied> {
    let mut cameras = configured();
    check_name(name, &cameras)?;
    let Some(nr) = EXTRA.clone().find(|n| cameras.iter().all(|c| c.nr != *n)) else {
        bail!("there can be at most {} extra cameras", EXTRA.count());
    };
    cameras.push(Camera { nr, name: name.into() });
    apply(&cameras)
}

/// Removes the extra camera called `name` (as administrator).
pub fn remove(name: &str) -> Result<Applied> {
    let mut cameras = configured();
    let Some(i) = cameras.iter().position(|c| c.name == name) else {
        bail!("there is no camera called {name}");
    };
    if i == 0 {
        bail!("{name} is the main camera and stays");
    }
    cameras.remove(i);
    apply(&cameras)
}

fn apply(cameras: &[Camera]) -> Result<Applied> {
    if cameras.len() == 1 && Path::new(PACKAGED).exists() {
        // Back to what the package sets up.
        remove_file(CONF)?;
    } else {
        let tmp = format!("{CONF}.new");
        std::fs::write(&tmp, render(cameras))?;
        std::fs::rename(&tmp, CONF)?;
    }
    // A source install's file would be read after ours and win; it goes if it is the one INSTALL.md gives.
    if std::fs::read_to_string(SOURCE_INSTALL)
        .is_ok_and(|t| t.trim() == include_str!("../../../dist/v4l2loopback.conf").trim())
    {
        remove_file(SOURCE_INSTALL)?;
    }
    reload()
}

/// Loads v4l2loopback again so the cameras match the options (as administrator); not while a camera is open.
pub fn reload() -> Result<Applied> {
    let unloaded = Command::new("modprobe")
        .args(["-r", "v4l2loopback"])
        .stderr(std::process::Stdio::null())
        .status()?
        .success();
    if !unloaded {
        return Ok(Applied::AfterRestart);
    }
    if !Command::new("modprobe").arg("v4l2loopback").status()?.success() {
        bail!("could not load v4l2loopback again; restart the computer");
    }
    // The new devices get their permissions from udev; wait for it before anyone opens them.
    let _ = Command::new("udevadm").arg("settle").status();
    Ok(Applied::Now)
}

fn remove_file(path: &str) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_round_trip() {
        let cameras = vec![
            main_camera(),
            Camera {
                nr: 12,
                name: "Zoom (work)".into(),
            },
        ];
        let text = render(&cameras);
        assert!(
            text.contains("devices=2 video_nr=10,12 card_label=\"Opal C1 Virtual,Zoom (work)\" exclusive_caps=1,1")
        );
        assert_eq!(parse(&text), Some(cameras));
    }

    #[test]
    fn reads_the_shipped_options() {
        let shipped = parse(include_str!("../../../dist/v4l2loopback.conf")).unwrap();
        assert_eq!(shipped, vec![main_camera()]);
        assert_eq!(parse("# nothing\n"), None);
        let edited = parse("options v4l2loopback video_nr=12,10,11 card_label=\"B,Opal C1 Virtual,A\"").unwrap();
        assert_eq!(edited.iter().map(|c| c.nr).collect::<Vec<_>>(), [10, 11, 12]);
    }

    #[test]
    fn names_are_checked() {
        let cameras = [main_camera()];
        assert!(check_name("Zoom cam 2", &cameras).is_ok());
        assert!(check_name("opal c1 virtual", &cameras).is_err());
        for bad in ["", " Zoom", "a,b", "say \"hi\"", "x|y", "line\nbreak", &"x".repeat(32)] {
            assert!(check_name(bad, &cameras).is_err(), "{bad:?}");
        }
    }
}
