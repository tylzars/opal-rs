//! Tray icon for the opal-rs service: shows whether the camera is on and offers the common switches.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::time::Duration;

#[cfg(unix)]
use ksni::blocking::TrayMethods;
#[cfg(unix)]
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem};
#[cfg(unix)]
use ksni::{Icon, MenuItem};

#[cfg(windows)]
mod windows;

/// Sends one command to the service and returns its reply.
fn send(command: &str) -> Option<String> {
    // Short: the tray's window waits on this.
    opal_ipc::request(command, Duration::from_secs(1)).ok()
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

    /// What the service waits for while no camera session runs.
    fn waiting(&self) -> Option<&str> {
        self.status.strip_prefix("waiting ").map(str::trim)
    }

    fn streaming(&self) -> bool {
        value(&self.status, "streaming") == Some("true")
    }

    fn video_off(&self) -> bool {
        value(&self.status, "video") == Some("off")
    }

    /// 0 when needed, 1 always on, 2 off.
    fn mode(&self) -> usize {
        match value(&self.status, "mode") {
            Some("on") => 1,
            Some("off") => 2,
            _ => 0,
        }
    }

    /// Grey while the service is not running, light grey idle or waiting for the camera, white while streaming, blue
    /// while the video is off.
    fn colour(&self) -> [u8; 3] {
        if self.waiting().is_some() {
            [150, 150, 150]
        } else if !self.running() {
            [110, 110, 110]
        } else if self.streaming() && self.video_off() {
            [70, 120, 255]
        } else if self.streaming() {
            [255, 255, 255]
        } else {
            [150, 150, 150]
        }
    }
}

/// Labels of the three camera modes, in [`Tray::mode`] order, and the commands that set them.
const MODES: [(&str, &str); 3] = [
    ("Camera on when needed", "auto"),
    ("Camera always on", "on"),
    ("Camera off", "off"),
];

/// A filled circle, as alpha and the colour per pixel.
fn dot(size: usize, colour: [u8; 3]) -> Vec<(u8, [u8; 3])> {
    let c = (size as f32 - 1.0) / 2.0;
    (0..size * size)
        .map(|i| {
            let (x, y) = ((i % size) as f32, (i / size) as f32);
            let d = ((x - c).powi(2) + (y - c).powi(2)).sqrt();
            (((size as f32 * 0.36 - d + 0.5).clamp(0.0, 1.0) * 255.0) as u8, colour)
        })
        .collect()
}

/// Starts the control panel.
fn open_panel() {
    match std::process::Command::new(opal_ipc::program("opal-rs-gui")).spawn() {
        // Collected when it closes, so it does not linger as a zombie.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(e) => eprintln!("opal-rs-gui: {e}"),
    }
}

#[cfg(unix)]
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "opal-rs".into()
    }

    fn title(&self) -> String {
        "Opal C1".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        // ARGB32 big-endian, as the tray protocol wants.
        [16, 24, 32, 48]
            .into_iter()
            .map(|size| Icon {
                width: size as i32,
                height: size as i32,
                data: dot(size, self.colour())
                    .into_iter()
                    .flat_map(|(a, [r, g, b])| [a, r, g, b])
                    .collect(),
            })
            .collect()
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
            let mode = self.mode();
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
                        send(MODES[i].1);
                        t.status = send("status").unwrap_or_default();
                    }),
                    options: MODES
                        .map(|(label, _)| RadioItem {
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
                activate: Box::new(|_: &mut Self| open_panel()),
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
        if let Some(code) = self.waiting() {
            opal_ipc::waiting_text(code).0.into()
        } else if !self.running() {
            "The camera service is not running".into()
        } else if !self.streaming() {
            "Camera idle".into()
        } else if self.video_off() {
            if value(&self.status, "gestures") == Some("on") {
                "Video off, watching for gestures".into()
            } else {
                "Video off".into()
            }
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

#[cfg(windows)]
fn main() {
    windows::run();
}

#[cfg(unix)]
fn main() {
    // A fallback for when the service is not enabled to start on its own.
    if let Err(e) = opal_ipc::start_service() {
        eprintln!("opal-rs: {e}");
    }
    // At login the desktop may start this before its tray host is up: wait for the host rather than give up.
    let handle = match Tray::default().assume_sni_available(true).spawn() {
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
