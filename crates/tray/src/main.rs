//! Tray icon for the opal-rs service: shows whether the camera is on and offers the common switches.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem};
use ksni::{Icon, MenuItem};

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("opal-rs.sock")
}

/// Sends one command to the service and returns its reply.
fn send(command: &str) -> Option<String> {
    let mut s = UnixStream::connect(socket_path()).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    writeln!(s, "{command}").ok()?;
    let mut reply = String::new();
    BufReader::new(&s).read_line(&mut reply).ok()?;
    Some(reply.trim_end().to_owned())
}

/// Word following `key` in the status line.
fn value<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    let mut words = status.split_whitespace();
    words.find(|w| *w == key)?;
    words.next()
}

#[derive(Default)]
struct Tray {
    /// Latest status line; empty while the service is not running.
    status: String,
}

impl Tray {
    fn running(&self) -> bool {
        self.status.starts_with("mode ")
    }

    fn streaming(&self) -> bool {
        value(&self.status, "streaming") == Some("true")
    }

    fn video_off(&self) -> bool {
        value(&self.status, "video") == Some("off")
    }
}

/// A filled circle, ARGB32 big-endian as the tray protocol wants.
fn dot(size: i32, [r, g, b]: [u8; 3]) -> Icon {
    let c = (size as f32 - 1.0) / 2.0;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let a = ((size as f32 * 0.36 - d + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
            data.extend([a, r, g, b]);
        }
    }
    Icon {
        width: size,
        height: size,
        data,
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "opal-rs".into()
    }

    fn title(&self) -> String {
        "Opal C1".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let colour = if !self.running() {
            [110, 110, 110]
        } else if self.streaming() && self.video_off() {
            [70, 120, 255]
        } else if self.streaming() {
            [255, 255, 255]
        } else {
            [150, 150, 150]
        };
        [16, 24, 32, 48].into_iter().map(|s| dot(s, colour)).collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Opal C1".into(),
            description: self.summary(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: self.summary(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        if self.running() {
            let mode = match value(&self.status, "mode") {
                Some("on") => 1,
                Some("off") => 2,
                _ => 0,
            };
            items.extend([
                CheckmarkItem {
                    label: "Video on".into(),
                    checked: !self.video_off(),
                    activate: Box::new(|t: &mut Self| {
                        send(if t.video_off() { "video on" } else { "video off" });
                        t.status = send("status").unwrap_or_default();
                    }),
                    ..Default::default()
                }
                .into(),
                RadioGroup {
                    selected: mode,
                    select: Box::new(|t: &mut Self, i| {
                        send(["auto", "on", "off"][i]);
                        t.status = send("status").unwrap_or_default();
                    }),
                    options: ["Camera on when needed", "Camera always on", "Camera off"]
                        .map(|label| RadioItem {
                            label: label.into(),
                            ..Default::default()
                        })
                        .into(),
                }
                .into(),
                MenuItem::Separator,
            ]);
        }
        items.extend([
            StandardItem {
                label: "Open control panel".into(),
                activate: Box::new(|_: &mut Self| {
                    if let Err(e) = std::process::Command::new("opal-rs-gui").spawn() {
                        eprintln!("opal-rs-gui: {e}");
                    }
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Hide this icon".into(),
                activate: Box::new(|_: &mut Self| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

impl Tray {
    fn summary(&self) -> String {
        if !self.running() {
            "The camera service is not running".into()
        } else if !self.streaming() {
            "Camera idle".into()
        } else if self.video_off() {
            "Video off, watching for gestures".into()
        } else {
            let apps: Vec<&str> = value(&self.status, "apps")
                .unwrap_or("-")
                .split(',')
                .filter(|a| *a != "-" && *a != "opal-rs-gui")
                .collect();
            if apps.is_empty() {
                "Camera on".into()
            } else {
                format!("Camera on, used by {}", apps.join(", "))
            }
        }
    }
}

fn main() {
    let handle = match Tray::default().spawn() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("no tray available: {e}");
            std::process::exit(1);
        }
    };
    loop {
        let status = send("status").unwrap_or_default();
        handle.update(|t| t.status = status);
        std::thread::sleep(Duration::from_secs(2));
    }
}
